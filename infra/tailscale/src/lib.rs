//! The staging tailnet's access policy, checked without the coordinator
//! (#684).
//!
//! `infra/tailscale/policy.json` is the file `infra/tailscale/policy.sh`
//! sends to Tailscale. The coordinator runs its `tests` block when it
//! validates or applies it, but only with an API key; this crate runs the
//! same block in the workspace test job, for the subset of the policy
//! language the file uses. What the subset does not know is refused rather
//! than ignored, since each can open something the tests here would not
//! see: a key such as `ssh`, `grants`, `autoApprovers` or `nodeAttrs`; a
//! rule source `*`; a rule destination whose host is anything but a
//! declared tag (an address, a subnet, `autogroup:…`, a group, a login,
//! `*`); a malformed or inverted port range.

use std::collections::BTreeMap;

use serde::Deserialize;

/// The policy file, restricted to the keys this repository uses.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub groups: BTreeMap<String, Vec<String>>,
    #[serde(rename = "tagOwners")]
    pub tag_owners: BTreeMap<String, Vec<String>>,
    pub acls: Vec<Acl>,
    pub tests: Vec<PolicyTest>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Acl {
    pub action: String,
    pub src: Vec<String>,
    pub dst: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyTest {
    pub src: String,
    #[serde(default)]
    pub accept: Vec<String>,
    #[serde(default)]
    pub deny: Vec<String>,
}

/// Parses a policy, refusing any key outside the subset above.
pub fn parse(json: &str) -> Result<Policy, String> {
    serde_json::from_str(json).map_err(|e| e.to_string())
}

/// Whether the policy lets `src` (a group, a tag or a login) open `dst`
/// (`host:port`, one port).
///
/// Every rule is checked before any is matched, so a rule outside the
/// subset (an undeclared group or tag, a source `*`, a destination host
/// that is not a tag, a malformed port) is an error even when the question
/// asked does not touch it. A question may still name any host: since
/// rules only reach tags, an address is never reached.
pub fn allows(policy: &Policy, src: &str, dst: &str) -> Result<bool, String> {
    check_rules(policy)?;
    let principals = principals(policy, src)?;
    let (host, port) = split_host_port(dst)?;
    let port: u16 = port
        .parse()
        .map_err(|_| format!("{dst}: a test names one port"))?;
    for acl in policy.acls.iter().filter(|acl| acl.action == "accept") {
        if !acl.src.iter().any(|s| principals.iter().any(|p| p == s)) {
            continue;
        }
        for d in &acl.dst {
            let (rule_host, ports) = split_host_port(d)?;
            if rule_host == host && ports_match(ports, port)? {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// One line per expectation of the `tests` block the rules contradict.
pub fn failed_tests(policy: &Policy) -> Result<Vec<String>, String> {
    let mut failed = Vec::new();
    for test in &policy.tests {
        for dst in &test.accept {
            if !allows(policy, &test.src, dst)? {
                failed.push(format!("{} should reach {dst}", test.src));
            }
        }
        for dst in &test.deny {
            if allows(policy, &test.src, dst)? {
                failed.push(format!("{} should not reach {dst}", test.src));
            }
        }
    }
    Ok(failed)
}

/// What keeps the versioned file publishable: the repository is public, so
/// no login is written in it (groups stay empty, their members come from a
/// secret at apply time), every rule accepts, and the tests block exists.
pub fn publication_problems(policy: &Policy, raw: &str) -> Vec<String> {
    let mut problems = Vec::new();
    for (group, members) in &policy.groups {
        if !members.is_empty() {
            problems.push(format!(
                "{group} lists members: they come from the TS_ACL_GROUPS secret, not from the repository"
            ));
        }
    }
    if raw.contains('@') {
        problems.push("the file contains an @: no login is written in the repository".to_string());
    }
    for (i, acl) in policy.acls.iter().enumerate() {
        if acl.action != "accept" {
            problems.push(format!(
                r#"rule {} has action "{}", not "accept""#,
                i + 1,
                acl.action
            ));
        }
    }
    if policy.tests.is_empty() {
        problems.push("the tests block is empty".to_string());
    }
    problems
}

/// The identities a source stands for: itself, and for a login every group
/// that lists it.
fn principals(policy: &Policy, src: &str) -> Result<Vec<String>, String> {
    declared(policy, src)?;
    let mut out = vec![src.to_string()];
    if src.contains('@') {
        out.extend(
            policy
                .groups
                .iter()
                .filter(|(_, members)| members.iter().any(|m| m == src))
                .map(|(group, _)| group.clone()),
        );
    }
    Ok(out)
}

/// A group or a tag must be declared; a login stands for itself; nothing
/// else (`autogroup:…`, an IP as a source) is in the subset.
fn declared(policy: &Policy, name: &str) -> Result<(), String> {
    if name.starts_with("group:") {
        if policy.groups.contains_key(name) {
            return Ok(());
        }
        return Err(format!("{name} is not declared in groups"));
    }
    if name.starts_with("tag:") {
        if policy.tag_owners.contains_key(name) {
            return Ok(());
        }
        return Err(format!("{name} is not declared in tagOwners"));
    }
    if name.contains('@') {
        return Ok(());
    }
    Err(format!("{name}: not a group, a tag or a login"))
}

fn check_rules(policy: &Policy) -> Result<(), String> {
    for acl in &policy.acls {
        for src in &acl.src {
            declared(policy, src)?;
        }
        for dst in &acl.dst {
            let (host, ports) = split_host_port(dst)?;
            if !host.starts_with("tag:") {
                return Err(format!(
                    "{dst}: a rule destination is a declared tag, nothing else"
                ));
            }
            declared(policy, host)?;
            ports_match(ports, 0)?;
        }
    }
    Ok(())
}

fn split_host_port(dst: &str) -> Result<(&str, &str), String> {
    match dst.rsplit_once(':') {
        Some((host, ports)) if !host.is_empty() && !ports.is_empty() => Ok((host, ports)),
        _ => Err(format!("{dst}: expected host:port")),
    }
}

/// `*`, or a comma-separated list of ports and `low-high` ranges. Every
/// part is parsed, matched or not, so a typo or an inverted range is an
/// error.
fn ports_match(spec: &str, port: u16) -> Result<bool, String> {
    if spec == "*" {
        return Ok(true);
    }
    let parse = |s: &str| {
        s.parse::<u16>()
            .map_err(|_| format!("{spec}: {s} is not a port"))
    };
    let mut hit = false;
    for part in spec.split(',') {
        let (low, high) = match part.split_once('-') {
            Some((low, high)) => (parse(low)?, parse(high)?),
            None => (parse(part)?, parse(part)?),
        };
        if low > high {
            return Err(format!("{spec}: {part} is an inverted range"));
        }
        hit |= (low..=high).contains(&port);
    }
    Ok(hit)
}

/// Stub, implemented in the next commit.
pub fn differences(_actual: &str, _pinned: &str) -> Result<Vec<String>, String> {
    Ok(vec![])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The file this repository applies to the tailnet.
    const REAL: &str = include_str!("../policy.json");

    /// The access #684 asks for, written out in full: the family reaches
    /// the staging's HTTPS port and nothing else; the administration adds
    /// SSH and the Kubernetes API (Argo CD is reached through it, by
    /// `kubectl port-forward`); nobody reaches the home network, another
    /// device, or anything through `ssh`, `grants`, `nodeAttrs` or
    /// `autoApprovers`; only the tailnet's administrators may tag a device
    /// `tag:serveur`. `policy.json` must equal it key for key, so a change
    /// to the policy is a change to this text as well, read by a person in
    /// review. The groups stay empty: the repository is public, their
    /// members come from the TS_ACL_GROUPS secret at apply time.
    const PINNED: &str = r#"{
        "groups": {"group:famille": [], "group:admin": []},
        "tagOwners": {"tag:serveur": ["autogroup:admin"]},
        "acls": [
            {"action": "accept", "src": ["group:famille"], "dst": ["tag:serveur:443"]},
            {"action": "accept", "src": ["group:admin"], "dst": ["tag:serveur:22,443,6443"]}
        ],
        "tests": [
            {
                "src": "group:famille",
                "accept": ["tag:serveur:443"],
                "deny": [
                    "tag:serveur:22", "tag:serveur:80", "tag:serveur:6443",
                    "tag:serveur:10250", "192.168.1.254:80", "192.168.1.254:443"
                ]
            },
            {
                "src": "group:admin",
                "accept": ["tag:serveur:22", "tag:serveur:443", "tag:serveur:6443"],
                "deny": [
                    "tag:serveur:80", "tag:serveur:2379", "tag:serveur:10250",
                    "192.168.1.254:80", "192.168.1.254:443"
                ]
            }
        ]
    }"#;

    #[test]
    fn the_real_policy_is_the_pinned_one() {
        assert_eq!(differences(REAL, PINNED), Ok(vec![]));
    }

    #[test]
    fn the_real_policy_names_no_login() {
        assert!(!REAL.contains('@'), "a login is written in policy.json");
    }

    #[test]
    fn key_order_and_layout_do_not_matter() {
        let reordered = r#"{"tests": [], "acls": [], "tagOwners": {"tag:a": ["autogroup:admin"]},
            "groups": {"group:b": [], "group:a": []}}"#;
        let pinned = r#"{"groups": {"group:a": [], "group:b": []},
            "tagOwners": {"tag:a": ["autogroup:admin"]}, "acls": [], "tests": []}"#;
        assert_eq!(differences(reordered, pinned), Ok(vec![]));
    }

    #[test]
    fn each_differing_top_level_key_is_named() {
        let actual = r#"{"groups": {}, "acls": [1], "ssh": []}"#;
        let pinned = r#"{"groups": {}, "acls": [2], "tests": []}"#;
        assert_eq!(
            differences(actual, pinned),
            Ok(vec![
                "acls differs from the pinned policy".to_string(),
                "ssh is not in the pinned policy".to_string(),
                "tests is missing".to_string(),
            ])
        );
    }

    #[test]
    fn a_file_that_is_not_a_json_object_is_an_error() {
        assert!(differences("{", PINNED).is_err());
        assert!(differences("[]", PINNED).is_err());
        assert!(differences(PINNED, "[]").is_err());
    }

    /// Replaces `old`, which must occur exactly once in policy.json.
    fn mutate(old: &str, new: &str) -> String {
        assert_eq!(REAL.matches(old).count(), 1, "{old}");
        REAL.replacen(old, new, 1)
    }

    /// Each change that would open something #684 does not ask for is a
    /// difference, whatever the policy language makes of it.
    #[test]
    fn every_access_widening_change_is_a_difference() {
        let famille = r#""dst": ["tag:serveur:443"]"#;
        let admin = r#""dst": ["tag:serveur:22,443,6443"]"#;
        let owners = r#""tag:serveur": ["autogroup:admin"]"#;
        let mutations = [
            mutate(famille, r#""dst": ["tag:serveur:443", "192.168.1.0/24:*"]"#),
            mutate(famille, r#""dst": ["tag:serveur:443", "100.64.0.0/10:*"]"#),
            mutate(famille, r#""dst": ["tag:serveur:443", "autogroup:internet:*"]"#),
            mutate(famille, r#""dst": ["tag:serveur:443,53"]"#),
            mutate(admin, r#""dst": ["tag:serveur:22,443,6443", "group:famille:*"]"#),
            mutate(admin, r#""dst": ["tag:serveur:22,443,6443,60000"]"#),
            mutate(admin, r#""dst": ["tag:serveur:22,443,6443,7000-6000"]"#),
            mutate(r#""src": ["group:famille"]"#, r#""src": ["group:famille", "*"]"#),
            // m6: a second tag, declared, reached by the family.
            mutate(owners, r#""tag:serveur": ["autogroup:admin"], "tag:autre": ["autogroup:admin"]"#)
                .replacen(famille, r#""dst": ["tag:serveur:443", "tag:autre:*"]"#, 1),
            // m7: a rule from another tag to every port of the server.
            mutate(
                r#""acls": ["#,
                r#""acls": [{"action": "accept", "src": ["tag:autre"], "dst": ["tag:serveur:*"]},"#,
            ),
            // m9: the family may tag its own devices tag:serveur.
            mutate(owners, r#""tag:serveur": ["group:famille"]"#),
            mutate(r#""group:famille": []"#, r#""group:famille": ["x"]"#),
            mutate(r#""group:admin": []"#, r#""group:admin": [], "group:autre": []"#),
            // A tests block rewritten to fit a widened policy.
            mutate(r#""accept": ["tag:serveur:443"]"#, r#""accept": ["tag:serveur:443", "tag:serveur:22"]"#),
        ];
        for mutated in &mutations {
            let found = differences(mutated, PINNED).expect("the mutation parses");
            assert!(!found.is_empty(), "accepted: {mutated}");
        }
    }

    /// Any key added at the top of the file, known to Tailscale or not.
    #[test]
    fn every_added_top_level_key_is_a_difference() {
        for key in [
            "ssh",
            "grants",
            "nodeAttrs",
            "autoApprovers",
            "postures",
            "hosts",
            "ipsets",
            "derpMap",
            "disableIPv4",
            "randomizeClientPort",
            "unknown",
        ] {
            let mutated = REAL.replacen('{', &format!(r#"{{"{key}": [],"#), 1);
            assert_eq!(
                differences(&mutated, PINNED),
                Ok(vec![format!("{key} is not in the pinned policy")]),
                "{key}"
            );
        }
    }
}

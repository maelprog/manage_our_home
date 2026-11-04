//! The staging tailnet's access policy, checked without the coordinator
//! (#684).
//!
//! `infra/tailscale/policy.json` is the file `infra/tailscale/policy.sh`
//! sends to Tailscale. The coordinator runs its `tests` block when it
//! validates or applies it, but only with an API key; this crate runs the
//! same block in the workspace test job, for the subset of the policy
//! language the file uses. A key the subset does not know (`ssh`, `grants`,
//! `autoApprovers`, `nodeAttrs`…) is refused rather than ignored: each of
//! them can open something the tests here would not see.

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
/// Every rule is checked before any is matched, so a rule naming an
/// undeclared group or tag, or a malformed port, is an error even when
/// the question asked does not touch it: the coordinator would refuse the
/// whole file.
pub fn allows(policy: &Policy, src: &str, dst: &str) -> Result<bool, String> {
    check_rules(policy)?;
    let principals = principals(policy, src)?;
    let (host, port) = split_host_port(dst)?;
    let port: u16 = port
        .parse()
        .map_err(|_| format!("{dst}: a test names one port"))?;
    for acl in policy.acls.iter().filter(|acl| acl.action == "accept") {
        if !acl
            .src
            .iter()
            .any(|s| s == "*" || principals.iter().any(|p| p == s))
        {
            continue;
        }
        for d in &acl.dst {
            let (rule_host, ports) = split_host_port(d)?;
            if (rule_host == "*" || rule_host == host) && ports_match(ports, port)? {
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
        for src in acl.src.iter().filter(|s| *s != "*") {
            declared(policy, src)?;
        }
        for dst in &acl.dst {
            let (host, ports) = split_host_port(dst)?;
            if host.starts_with("tag:") {
                declared(policy, host)?;
            }
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
/// part is parsed, matched or not, so a typo is an error.
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
        hit |= (low..=high).contains(&port);
    }
    Ok(hit)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = r#"{
        "groups": {"group:famille": ["a@example.org"], "group:admin": ["b@example.org"]},
        "tagOwners": {"tag:serveur": ["autogroup:admin"]},
        "acls": [
            {"action": "accept", "src": ["group:famille"], "dst": ["tag:serveur:443"]},
            {"action": "accept", "src": ["group:admin"], "dst": ["tag:serveur:22,443,6000-6443"]}
        ],
        "tests": []
    }"#;

    fn base() -> Policy {
        parse(BASE).expect("the base policy parses")
    }

    #[test]
    fn refuses_a_key_outside_the_subset() {
        for extra in [
            r#""ssh": []"#,
            r#""grants": []"#,
            r#""autoApprovers": {}"#,
            r#""nodeAttrs": []"#,
        ] {
            let json = BASE.replacen('{', &format!("{{{extra},"), 1);
            assert!(parse(&json).is_err(), "{extra} was accepted");
        }
    }

    #[test]
    fn refuses_an_unknown_key_inside_a_rule() {
        let json = BASE.replacen(
            r#""action": "accept","#,
            r#""action": "accept", "proto": "udp","#,
            1,
        );
        assert!(parse(&json).is_err());
    }

    #[test]
    fn a_group_reaches_only_the_ports_of_its_rules() {
        let p = base();
        assert_eq!(allows(&p, "group:famille", "tag:serveur:443"), Ok(true));
        assert_eq!(allows(&p, "group:famille", "tag:serveur:6443"), Ok(false));
        assert_eq!(allows(&p, "group:famille", "tag:serveur:22"), Ok(false));
    }

    #[test]
    fn a_port_list_and_a_range_both_match() {
        let p = base();
        assert_eq!(allows(&p, "group:admin", "tag:serveur:22"), Ok(true));
        assert_eq!(allows(&p, "group:admin", "tag:serveur:6000"), Ok(true));
        assert_eq!(allows(&p, "group:admin", "tag:serveur:6443"), Ok(true));
        assert_eq!(allows(&p, "group:admin", "tag:serveur:6444"), Ok(false));
        assert_eq!(allows(&p, "group:admin", "tag:serveur:80"), Ok(false));
    }

    #[test]
    fn a_login_gets_the_rules_of_its_groups() {
        let p = base();
        assert_eq!(allows(&p, "a@example.org", "tag:serveur:443"), Ok(true));
        assert_eq!(allows(&p, "a@example.org", "tag:serveur:22"), Ok(false));
        assert_eq!(allows(&p, "b@example.org", "tag:serveur:22"), Ok(true));
        assert_eq!(
            allows(&p, "nobody@example.org", "tag:serveur:443"),
            Ok(false)
        );
    }

    #[test]
    fn another_host_is_not_reached_through_a_tag() {
        let p = base();
        assert_eq!(allows(&p, "group:admin", "192.168.1.254:443"), Ok(false));
        assert_eq!(allows(&p, "group:admin", "tag:autre:443"), Ok(false));
    }

    /// A rule whose destination host is not a declared tag would reach
    /// something this check cannot see (an address, a subnet, the
    /// internet, every device of a group…): it is refused, not ignored.
    #[test]
    fn a_rule_destination_other_than_a_declared_tag_is_an_error() {
        for dst in [
            "192.168.1.0/24:*",
            "192.168.1.254:80",
            "100.64.0.0/10:*",
            "autogroup:internet:*",
            "autogroup:self:*",
            "group:famille:*",
            "a@example.org:443",
            "serveur:443",
            "*:*",
        ] {
            let json = BASE.replacen(
                r#""dst": ["tag:serveur:443"]"#,
                &format!(r#""dst": ["tag:serveur:443", "{dst}"]"#),
                1,
            );
            let p = parse(&json).unwrap();
            assert!(
                allows(&p, "group:admin", "tag:serveur:22").is_err(),
                "{dst} was accepted as a rule destination"
            );
        }
    }

    #[test]
    fn a_wildcard_source_is_an_error() {
        let json = BASE.replacen(
            r#""src": ["group:famille"]"#,
            r#""src": ["group:famille", "*"]"#,
            1,
        );
        let p = parse(&json).unwrap();
        assert!(allows(&p, "group:admin", "tag:serveur:22").is_err());
    }

    #[test]
    fn an_inverted_port_range_is_an_error() {
        let json = BASE.replacen("6000-6443", "6443-6000", 1);
        let p = parse(&json).unwrap();
        assert!(allows(&p, "group:famille", "tag:serveur:443").is_err());
    }

    #[test]
    fn malformed_destinations_are_errors_not_denials() {
        let p = base();
        assert!(allows(&p, "group:admin", "tag:serveur").is_err());
        assert!(allows(&p, "group:admin", "tag:serveur:abc").is_err());
        assert!(allows(&p, "group:admin", "tag:serveur:22,443").is_err());
        let bad_rule = BASE.replacen("tag:serveur:443", "tag:serveur:4x3", 1);
        let p = parse(&bad_rule).unwrap();
        assert!(allows(&p, "group:famille", "tag:serveur:443").is_err());
    }

    #[test]
    fn an_undeclared_group_or_tag_is_an_error() {
        let p = base();
        assert!(allows(&p, "group:inconnu", "tag:serveur:443").is_err());
        let json = BASE.replacen(
            r#""src": ["group:famille"]"#,
            r#""src": ["group:famile"]"#,
            1,
        );
        let p = parse(&json).unwrap();
        assert!(allows(&p, "group:admin", "tag:serveur:22").is_err());
        let json = BASE.replacen("tag:serveur:443", "tag:serveru:443", 1);
        let p = parse(&json).unwrap();
        assert!(allows(&p, "group:admin", "tag:serveur:22").is_err());
    }

    #[test]
    fn failed_tests_names_each_broken_expectation() {
        let json = BASE.replacen(
            r#""tests": []"#,
            r#""tests": [
                {"src": "group:famille", "accept": ["tag:serveur:443", "tag:serveur:22"], "deny": ["tag:serveur:6443"]},
                {"src": "group:admin", "deny": ["tag:serveur:22"]}
            ]"#,
            1,
        );
        let p = parse(&json).unwrap();
        assert_eq!(
            failed_tests(&p),
            Ok(vec![
                "group:famille should reach tag:serveur:22".to_string(),
                "group:admin should not reach tag:serveur:22".to_string(),
            ])
        );
    }

    #[test]
    fn failed_tests_is_empty_when_every_expectation_holds() {
        let json = BASE.replacen(
            r#""tests": []"#,
            r#""tests": [{"src": "group:famille", "accept": ["tag:serveur:443"], "deny": ["tag:serveur:22"]}]"#,
            1,
        );
        assert_eq!(failed_tests(&parse(&json).unwrap()), Ok(vec![]));
    }

    #[test]
    fn a_published_file_names_no_login_and_keeps_its_tests() {
        let p = base();
        let problems = publication_problems(&p, BASE);
        assert!(
            problems.iter().any(|m| m.contains("group:famille")),
            "{problems:?}"
        );
        assert!(problems.iter().any(|m| m.contains("@")), "{problems:?}");
        assert!(problems.iter().any(|m| m.contains("tests")), "{problems:?}");
    }

    #[test]
    fn a_published_file_only_accepts() {
        let json = BASE
            .replace(r#"["a@example.org"]"#, "[]")
            .replace(r#"["b@example.org"]"#, "[]")
            .replacen(r#""action": "accept""#, r#""action": "drop""#, 1)
            .replacen(
                r#""tests": []"#,
                r#""tests": [{"src": "group:admin", "accept": ["tag:serveur:22"]}]"#,
                1,
            );
        let p = parse(&json).unwrap();
        assert_eq!(
            publication_problems(&p, &json),
            vec![r#"rule 1 has action "drop", not "accept""#.to_string()]
        );
    }

    /// The file this repository applies to the tailnet.
    const REAL: &str = include_str!("../policy.json");

    #[test]
    fn the_real_policy_passes_its_own_tests() {
        let p = parse(REAL).expect("policy.json parses");
        assert_eq!(failed_tests(&p), Ok(vec![]));
    }

    #[test]
    fn the_real_policy_is_publishable() {
        let p = parse(REAL).unwrap();
        assert_eq!(publication_problems(&p, REAL), Vec::<String>::new());
    }

    /// #684: the family reaches the staging's HTTPS port and nothing else;
    /// the administration adds SSH and the Kubernetes API (Argo CD is
    /// reached through it, by `kubectl port-forward`); nobody reaches the
    /// home network. Pinned here so that a change to the file that also
    /// rewrites its tests block still meets the issue.
    #[test]
    fn the_real_policy_meets_the_issue() {
        let p = parse(REAL).unwrap();
        // Every port, not a sample: a port added to a rule is seen.
        for port in 1..=u16::MAX {
            let dst = format!("tag:serveur:{port}");
            assert_eq!(
                allows(&p, "group:famille", &dst),
                Ok(port == 443),
                "famille and port {port}"
            );
            assert_eq!(
                allows(&p, "group:admin", &dst),
                Ok([22, 443, 6443].contains(&port)),
                "admin and port {port}"
            );
            assert_eq!(
                allows(&p, "tag:serveur", &dst),
                Ok(false),
                "the server and its own port {port}"
            );
        }
        for src in ["group:famille", "group:admin", "tag:serveur"] {
            assert_eq!(allows(&p, src, "192.168.1.254:80"), Ok(false));
        }
        assert_eq!(
            p.groups.keys().collect::<Vec<_>>(),
            ["group:admin", "group:famille"]
        );
    }
}

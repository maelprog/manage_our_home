//! The staging tailnet's access policy, pinned (#684).
//!
//! `infra/tailscale/policy.json` is the file `infra/tailscale/policy.sh`
//! sends to Tailscale. Rather than interpret the policy language, which is
//! wide (tags, autogroups, addresses, `grants`, `ssh`, `nodeAttrs`…) and
//! where every construct left unchecked can open something, the test of
//! this crate compares the file, key for key, with the policy written out
//! in full in the test itself: an added, removed or changed key fails, so
//! changing the access means changing that text too, read by a person in
//! review. Tailscale's own `tests` block is run by the coordinator, when
//! the `Tailscale policy` workflow validates the policy of main.

use std::collections::BTreeSet;

use serde_json::{Map, Value};

/// One line per top-level key on which `actual` and `pinned` (two JSON
/// objects) disagree: present in one only, or with a different value.
/// Key order and layout do not count; everything else does, array order
/// included.
pub fn differences(actual: &str, pinned: &str) -> Result<Vec<String>, String> {
    let actual = object(actual, "the policy")?;
    let pinned = object(pinned, "the pinned policy")?;
    let keys: BTreeSet<&String> = actual.keys().chain(pinned.keys()).collect();
    Ok(keys
        .into_iter()
        .filter_map(|key| match (actual.get(key), pinned.get(key)) {
            (Some(a), Some(p)) if a == p => None,
            (Some(_), Some(_)) => Some(format!("{key} differs from the pinned policy")),
            (Some(_), None) => Some(format!("{key} is not in the pinned policy")),
            (None, _) => Some(format!("{key} is missing")),
        })
        .collect())
}

fn object(json: &str, what: &str) -> Result<Map<String, Value>, String> {
    match serde_json::from_str(json) {
        Ok(Value::Object(map)) => Ok(map),
        Ok(_) => Err(format!("{what} is not a JSON object")),
        Err(e) => Err(format!("{what}: {e}")),
    }
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
            mutate(
                famille,
                r#""dst": ["tag:serveur:443", "autogroup:internet:*"]"#,
            ),
            mutate(famille, r#""dst": ["tag:serveur:443,53"]"#),
            mutate(
                admin,
                r#""dst": ["tag:serveur:22,443,6443", "group:famille:*"]"#,
            ),
            mutate(admin, r#""dst": ["tag:serveur:22,443,6443,60000"]"#),
            mutate(admin, r#""dst": ["tag:serveur:22,443,6443,7000-6000"]"#),
            mutate(
                r#""src": ["group:famille"]"#,
                r#""src": ["group:famille", "*"]"#,
            ),
            // m6: a second tag, declared, reached by the family.
            mutate(
                owners,
                r#""tag:serveur": ["autogroup:admin"], "tag:autre": ["autogroup:admin"]"#,
            )
            .replacen(famille, r#""dst": ["tag:serveur:443", "tag:autre:*"]"#, 1),
            // m7: a rule from another tag to every port of the server.
            mutate(
                r#""acls": ["#,
                r#""acls": [{"action": "accept", "src": ["tag:autre"], "dst": ["tag:serveur:*"]},"#,
            ),
            // m9: the family may tag its own devices tag:serveur.
            mutate(owners, r#""tag:serveur": ["group:famille"]"#),
            mutate(r#""group:famille": []"#, r#""group:famille": ["x"]"#),
            mutate(
                r#""group:admin": []"#,
                r#""group:admin": [], "group:autre": []"#,
            ),
            // A tests block rewritten to fit a widened policy.
            mutate(
                r#""accept": ["tag:serveur:443"]"#,
                r#""accept": ["tag:serveur:443", "tag:serveur:22"]"#,
            ),
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

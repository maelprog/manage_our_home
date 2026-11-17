//! The repository's licence, declared where tools read it (#913).
//!
//! `LICENSE` holds the PolyForm Noncommercial 1.0.0 text; every crate of the
//! workspace and both npm packages must announce the same SPDX identifier,
//! and no crate may be publishable to crates.io. A crate added to the
//! workspace without these fields fails here, on the `test` job.
//!
//! The manifests are read as text, line by line: the workspace has no TOML
//! parser among its dependencies, and the fields checked are plain
//! `key = "value"` lines of the `[package]` table.

use std::path::{Path, PathBuf};

const SPDX: &str = "PolyForm-Noncommercial-1.0.0";

fn repo_root() -> PathBuf {
    // apps/shared -> repository root.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repository root")
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// The raw value of `key` in the `[package]` table of a Cargo manifest:
/// `Some("\"x\"")` for `key = "x"`, `Some("false")` for `key = false`.
fn package_field<'a>(manifest: &'a str, key: &str) -> Option<&'a str> {
    let mut in_package = false;
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_package = line == "[package]";
            continue;
        }
        if !in_package {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        if k.trim() == key {
            return Some(v.trim());
        }
    }
    None
}

/// The paths listed in the root manifest's `members = [...]` (one line).
fn workspace_members(root_manifest: &str) -> Vec<String> {
    let line = root_manifest
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("members"))
        .expect("a `members = [...]` line in the root Cargo.toml");
    let list = line
        .split_once('[')
        .and_then(|(_, rest)| rest.split_once(']'))
        .map(|(inside, _)| inside)
        .expect("`members` written on one line");
    list.split(',')
        .map(|m| m.trim().trim_matches('"').to_string())
        .filter(|m| !m.is_empty())
        .collect()
}

#[test]
fn package_field_reads_only_the_package_table() {
    let manifest = "[package]\nname = \"a\"\nlicense = \"MIT\"\npublish = false\n\n\
                    [dependencies]\nlicense = \"x\"\n";
    assert_eq!(package_field(manifest, "license"), Some("\"MIT\""));
    assert_eq!(package_field(manifest, "publish"), Some("false"));
    assert_eq!(package_field(manifest, "edition"), None);
    let elsewhere = "[package]\nname = \"a\"\n[dependencies]\nlicense = \"x\"\n";
    assert_eq!(package_field(elsewhere, "license"), None);
}

#[test]
fn workspace_members_reads_the_one_line_list() {
    let root = "[workspace]\nresolver = \"2\"\nmembers = [\"apps/api\", \"infra/x\"]\n";
    assert_eq!(workspace_members(root), vec!["apps/api", "infra/x"]);
}

#[test]
fn licence_file_is_polyform_noncommercial_with_the_required_notice() {
    let licence = read(&repo_root().join("LICENSE"));
    assert!(
        licence.starts_with("# PolyForm Noncommercial License 1.0.0\n"),
        "LICENSE must start with the PolyForm Noncommercial 1.0.0 title"
    );
    assert!(
        licence
            .lines()
            .any(|l| l == "Required Notice: Copyright (c) 2026 maelprog"),
        "LICENSE must carry the Required Notice line"
    );
    assert!(
        !licence.contains("Permission is hereby granted"),
        "LICENSE still holds the MIT text"
    );
}

#[test]
fn every_workspace_crate_declares_the_licence_and_is_not_publishable() {
    let root = repo_root();
    let members = workspace_members(&read(&root.join("Cargo.toml")));
    assert!(members.iter().any(|m| m == "apps/shared"), "{members:?}");
    let expected = format!("\"{SPDX}\"");
    for member in &members {
        let manifest = read(&root.join(member).join("Cargo.toml"));
        assert_eq!(
            package_field(&manifest, "license"),
            Some(expected.as_str()),
            "{member}/Cargo.toml: license"
        );
        assert_eq!(
            package_field(&manifest, "publish"),
            Some("false"),
            "{member}/Cargo.toml: publish"
        );
    }
}

#[test]
fn npm_packages_declare_the_licence() {
    let root = repo_root();
    let expected = format!("\"license\": \"{SPDX}\"");
    for package in ["apps/mobile/package.json", "e2e/package.json"] {
        let json = read(&root.join(package));
        assert!(
            json.lines()
                .any(|l| l.trim().trim_end_matches(',') == expected),
            "{package}: {expected}"
        );
    }
}

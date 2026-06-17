mod common;

use common::*;

/// A workspace with a single repo `a` and one lock file written under
/// `.ezgitx/locks/`.
fn workspace_with_lock(file: &str, contents: &str) -> Fixture {
    let f = Fixture::new();
    f.repo("a");
    f.config("version: 1\ngroups:\n  g:\n    - path: ./a\n");
    let lock_dir = f.root().join(".ezgitx/locks");
    std::fs::create_dir_all(&lock_dir).unwrap();
    std::fs::write(lock_dir.join(file), contents).unwrap();
    f
}

#[test]
fn lists_live_repo_lock() {
    let f = workspace_with_lock("repo-a.lock", &live_lock_json("pull"));

    let assert = f.ezgitx().arg("sessions").assert().code(0);
    let lines = jsonl(&assert.get_output().stdout);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["repo"], "a");
    assert_eq!(lines[0]["scope"], "repo");
    assert_eq!(lines[0]["op"], "pull");
    assert!(lines[0]["pid"].is_number());
}

#[test]
fn skips_stale_lock() {
    let f = workspace_with_lock("repo-a.lock", &stale_lock_json("pull"));

    let assert = f.ezgitx().arg("sessions").assert().code(0);
    let lines = jsonl(&assert.get_output().stdout);
    assert!(lines.is_empty(), "stale lock must not be listed: {lines:?}");
}

#[test]
fn lists_workspace_lock() {
    let f = workspace_with_lock("workspace.lock", &live_lock_json("sync"));

    let assert = f.ezgitx().arg("sessions").assert().code(0);
    let lines = jsonl(&assert.get_output().stdout);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["scope"], "workspace");
    assert_eq!(lines[0]["op"], "sync");
    assert!(lines[0].get("repo").is_none());
}

#[test]
fn empty_when_no_locks() {
    let f = Fixture::new();
    f.repo("a");
    f.config("version: 1\ngroups:\n  g:\n    - path: ./a\n");

    let assert = f.ezgitx().arg("sessions").assert().code(0);
    let lines = jsonl(&assert.get_output().stdout);
    assert!(lines.is_empty(), "no locks → no session lines: {lines:?}");
}

#[test]
fn human_mode_emits_no_json() {
    let f = workspace_with_lock("repo-a.lock", &live_lock_json("pull"));

    let assert = f.ezgitx().args(["sessions", "--human"]).assert().code(0);
    let out = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    assert!(out.contains("SCOPE"));
    assert!(!out.contains('{'), "human mode must not emit JSON: {out}");
}

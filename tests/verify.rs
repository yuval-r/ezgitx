mod common;

use common::*;

/// The trailing verdict line emitted by `verify`.
fn verdict(lines: &[serde_json::Value]) -> &serde_json::Value {
    lines
        .iter()
        .find(|l| l["type"] == "verdict")
        .unwrap_or_else(|| panic!("no verdict line in {lines:?}"))
}

/// Mark a repo dirty by dropping an untracked file in it.
fn dirty(repo: &std::path::Path) {
    std::fs::write(repo.join("wip.txt"), "wip").unwrap();
}

#[test]
fn clean_workspace_passes() {
    let f = Fixture::new();
    f.repo("a");
    f.repo("b");
    f.config(
        "version: 1\ngroups:\n  g:\n    - path: ./a\n      check_cmd: \"true\"\n    - path: ./b\n      check_cmd: \"true\"\n",
    );

    let assert = f.ezgitx().arg("verify").assert().code(0);
    let lines = jsonl(&assert.get_output().stdout);
    let v = verdict(&lines);
    assert_eq!(v["verdict"], "pass");
    assert_eq!(v["checked"], 0);
    assert_eq!(v["failed"], serde_json::json!([]));
    // Nothing dirty: only the verdict line, no per-repo lines.
    assert_eq!(lines.len(), 1);
}

#[test]
fn dirty_repo_checked_and_passes() {
    let f = Fixture::new();
    let app = f.repo("app");
    f.config("version: 1\ngroups:\n  g:\n    - path: ./app\n      check_cmd: \"true\"\n");
    dirty(&app);

    let assert = f.ezgitx().arg("verify").assert().code(0);
    let lines = jsonl(&assert.get_output().stdout);
    assert_eq!(line_for(&lines, "app")["exit_code"], 0);
    let v = verdict(&lines);
    assert_eq!(v["verdict"], "pass");
    assert_eq!(v["checked"], 1);
}

#[test]
fn dirty_repo_failing_check_fails_verdict() {
    let f = Fixture::new();
    let app = f.repo("app");
    f.config("version: 1\ngroups:\n  g:\n    - path: ./app\n      check_cmd: \"false\"\n");
    dirty(&app);

    let assert = f.ezgitx().arg("verify").assert().code(1);
    let lines = jsonl(&assert.get_output().stdout);
    assert_ne!(line_for(&lines, "app")["exit_code"], 0);
    let v = verdict(&lines);
    assert_eq!(v["verdict"], "fail");
    assert_eq!(v["failed"], serde_json::json!(["app"]));
}

#[test]
fn downstream_of_dirty_is_checked() {
    let f = Fixture::new();
    let app = f.repo("app");
    f.repo("web");
    f.config(
        "version: 1\ngroups:\n  g:\n    - path: ./app\n      check_cmd: \"true\"\n    - path: ./web\n      check_cmd: \"true\"\n      depends_on: [\"app\"]\n",
    );
    dirty(&app); // web is clean, but downstream of dirty app

    let assert = f.ezgitx().arg("verify").assert().code(0);
    let lines = jsonl(&assert.get_output().stdout);
    // Both the dirty repo and its downstream dependent are checked.
    assert_eq!(line_for(&lines, "app")["exit_code"], 0);
    assert_eq!(line_for(&lines, "web")["exit_code"], 0);
    assert_eq!(verdict(&lines)["checked"], 2);
}

#[test]
fn downstream_failure_named_in_verdict() {
    let f = Fixture::new();
    let app = f.repo("app");
    f.repo("web");
    f.config(
        "version: 1\ngroups:\n  g:\n    - path: ./app\n      check_cmd: \"true\"\n    - path: ./web\n      check_cmd: \"false\"\n      depends_on: [\"app\"]\n",
    );
    dirty(&app);

    let assert = f.ezgitx().arg("verify").assert().code(1);
    let lines = jsonl(&assert.get_output().stdout);
    let v = verdict(&lines);
    assert_eq!(v["verdict"], "fail");
    assert_eq!(v["failed"], serde_json::json!(["web"]));
}

#[test]
fn upstream_failure_propagates() {
    let f = Fixture::new();
    let app = f.repo("app");
    f.repo("web");
    f.config(
        "version: 1\ngroups:\n  g:\n    - path: ./app\n      check_cmd: \"false\"\n    - path: ./web\n      check_cmd: \"true\"\n      depends_on: [\"app\"]\n",
    );
    dirty(&app);

    let assert = f.ezgitx().arg("verify").assert().code(1);
    let lines = jsonl(&assert.get_output().stdout);
    // app fails its own check; web is blocked because its upstream failed.
    assert_eq!(line_for(&lines, "web")["error"]["code"], "upstream_failed");
    let v = verdict(&lines);
    assert_eq!(v["verdict"], "fail");
    assert_eq!(v["failed"], serde_json::json!(["app", "web"]));
}

#[test]
fn human_mode_emits_no_json() {
    let f = Fixture::new();
    let app = f.repo("app");
    f.config("version: 1\ngroups:\n  g:\n    - path: ./app\n      check_cmd: \"true\"\n");
    dirty(&app);

    let assert = f.ezgitx().args(["verify", "--human"]).assert().code(0);
    let out = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    assert!(out.contains("REPO"));
    assert!(out.contains("app"));
    assert!(!out.contains('{'), "human mode must not emit JSON: {out}");
}

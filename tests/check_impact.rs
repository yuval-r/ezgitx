mod common;

use common::*;

/// core <- lib <- {app, tool}; side has no relation to core.
fn graph_config(f: &Fixture) {
    for name in ["core", "lib", "app", "tool", "side"] {
        f.repo(name);
    }
    f.config(
        "version: 1\n\
         groups:\n\
         \x20 g:\n\
         \x20   - path: ./core\n\
         \x20   - path: ./lib\n\
         \x20     depends_on: [\"core\"]\n\
         \x20     check_cmd: \"echo check-lib >> ../check.log\"\n\
         \x20   - path: ./app\n\
         \x20     depends_on: [\"lib\"]\n\
         \x20     default_cmd: \"echo build-app >> ../check.log\"\n\
         \x20   - path: ./tool\n\
         \x20     depends_on: [\"lib\"]\n\
         \x20     check_cmd: \"echo check-tool >> ../check.log\"\n\
         \x20   - path: ./side\n",
    );
}

#[test]
fn lists_downstream_closure_with_depth_and_via() {
    let f = Fixture::new();
    graph_config(&f);
    let assert = f
        .ezgitx()
        .args(["check-impact", "--repo", "core"])
        .assert()
        .code(0);
    let lines = jsonl(&assert.get_output().stdout);

    let lib = line_for(&lines, "lib");
    assert_eq!(lib["depth"], 1);
    assert_eq!(lib["via"], serde_json::json!(["core"]));
    let app = line_for(&lines, "app");
    assert_eq!(app["depth"], 2);
    assert_eq!(app["via"], serde_json::json!(["core", "lib"]));
    assert!(lines.iter().all(|l| l["repo"] != "side"));

    let s = summary(&lines);
    assert_eq!(s["changed"], "core");
    assert_eq!(s["affected"], 3);
}

#[test]
fn defaults_to_current_repo() {
    let f = Fixture::new();
    graph_config(&f);
    let assert = f
        .ezgitx_in(&f.root().join("lib"))
        .arg("check-impact")
        .assert()
        .code(0);
    let lines = jsonl(&assert.get_output().stdout);
    assert_eq!(summary(&lines)["changed"], "lib");
    assert_eq!(summary(&lines)["affected"], 2);
}

#[test]
fn at_root_without_repo_flag_is_usage_error() {
    let f = Fixture::new();
    graph_config(&f);
    f.ezgitx().arg("check-impact").assert().code(2);
}

#[test]
fn unknown_repo_is_usage_error() {
    let f = Fixture::new();
    graph_config(&f);
    f.ezgitx()
        .args(["check-impact", "--repo", "ghost"])
        .assert()
        .code(2);
}

#[test]
fn leaf_repo_has_no_impact() {
    let f = Fixture::new();
    graph_config(&f);
    let assert = f
        .ezgitx()
        .args(["check-impact", "--repo", "app"])
        .assert()
        .code(0);
    let lines = jsonl(&assert.get_output().stdout);
    assert_eq!(lines.len(), 1); // summary only
    assert_eq!(summary(&lines)["affected"], 0);
}

#[test]
fn check_runs_check_cmd_with_default_fallback_in_order() {
    let f = Fixture::new();
    graph_config(&f);
    let assert = f
        .ezgitx()
        .args(["check-impact", "--repo", "core", "--check"])
        .assert()
        .code(0);
    let lines = jsonl(&assert.get_output().stdout);

    // Listing lines first, then run-shaped result lines and a run summary.
    assert_eq!(line_for(&lines, "lib")["depth"], 1);
    let results: Vec<_> = lines
        .iter()
        .filter(|l| l.get("exit_code").is_some())
        .collect();
    assert_eq!(results.len(), 3);
    assert!(results.iter().all(|l| l["exit_code"] == 0));

    let log = std::fs::read_to_string(f.root().join("check.log")).unwrap();
    let order: Vec<&str> = log.lines().collect();
    assert_eq!(order[0], "check-lib"); // wave 1
    let mut rest = order[1..].to_vec();
    rest.sort();
    assert_eq!(rest, ["build-app", "check-tool"]); // wave 2: app fell back to default_cmd
}

#[test]
fn check_propagates_upstream_failure() {
    let f = Fixture::new();
    for name in ["core", "lib", "app"] {
        f.repo(name);
    }
    f.config(
        "version: 1\n\
         groups:\n\
         \x20 g:\n\
         \x20   - path: ./core\n\
         \x20   - path: ./lib\n\
         \x20     depends_on: [\"core\"]\n\
         \x20     check_cmd: \"false\"\n\
         \x20   - path: ./app\n\
         \x20     depends_on: [\"lib\"]\n\
         \x20     check_cmd: \"echo ok\"\n",
    );
    let assert = f
        .ezgitx()
        .args(["check-impact", "--repo", "core", "--check"])
        .assert()
        .code(1);
    let lines = jsonl(&assert.get_output().stdout);
    let results: Vec<_> = lines
        .iter()
        .filter(|l| l.get("exit_code").is_some())
        .collect();
    let lib = results.iter().find(|l| l["repo"] == "lib").unwrap();
    assert_eq!(lib["exit_code"], 1);
    let app = results.iter().find(|l| l["repo"] == "app").unwrap();
    assert_eq!(app["error"]["code"], "upstream_failed");
}

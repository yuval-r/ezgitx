mod common;

use common::*;

fn basic_config(f: &Fixture) {
    f.config("version: 1\ngroups:\n  g:\n    - path: ./a\n    - path: ./b\n");
}

#[test]
fn clean_repos_no_upstream() {
    let f = Fixture::new();
    f.repo("a");
    f.repo("b");
    basic_config(&f);
    let assert = f.ezgitx().arg("status").assert().code(0);
    let lines = jsonl(&assert.get_output().stdout);
    assert_eq!(lines.len(), 2);
    let a = line_for(&lines, "a");
    assert_eq!(a["branch"], "main");
    assert_eq!(a["state"], "clean");
    assert!(a["ahead"].is_null());
    assert!(a["behind"].is_null());
    assert_eq!(a["head"].as_str().unwrap().len(), 7);
    assert!(a["path"].as_str().unwrap().ends_with("/a"));
    assert!(a.get("stale_deps").is_none());
}

#[test]
fn dirty_and_filter() {
    let f = Fixture::new();
    let a = f.repo("a");
    f.repo("b");
    basic_config(&f);
    std::fs::write(a.join("wip.txt"), "wip").unwrap();

    let assert = f.ezgitx().arg("status").assert().code(0);
    let lines = jsonl(&assert.get_output().stdout);
    assert_eq!(line_for(&lines, "a")["state"], "dirty");
    assert_eq!(line_for(&lines, "b")["state"], "clean");

    let assert = f.ezgitx().args(["status", "--dirty"]).assert().code(0);
    let lines = jsonl(&assert.get_output().stdout);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["repo"], "a");
}

#[test]
fn detached_head() {
    let f = Fixture::new();
    let a = f.repo("a");
    f.repo("b");
    basic_config(&f);
    git(&a, &["checkout", "-q", "--detach"]);
    let assert = f.ezgitx().args(["status", "--repo", "a"]).assert().code(0);
    let lines = jsonl(&assert.get_output().stdout);
    assert_eq!(lines[0]["state"], "detached");
    assert!(lines[0]["branch"].is_null());
}

#[test]
fn behind_after_fetch_never_fetches_itself() {
    let f = Fixture::new();
    let a = f.repo_with_remote("a");
    f.repo("b");
    basic_config(&f);
    f.push_upstream_commit("a", "new.txt");

    // status never fetches: still 0/0 against the stale remote-tracking ref.
    let assert = f.ezgitx().args(["status", "--repo", "a"]).assert().code(0);
    let lines = jsonl(&assert.get_output().stdout);
    assert_eq!(lines[0]["ahead"], 0);
    assert_eq!(lines[0]["behind"], 0);

    git(&a, &["fetch", "-q"]);
    let assert = f.ezgitx().args(["status", "--repo", "a"]).assert().code(0);
    let lines = jsonl(&assert.get_output().stdout);
    assert_eq!(lines[0]["behind"], 1);
}

#[test]
fn not_a_repo_is_reported_lazily() {
    let f = Fixture::new();
    f.repo("a");
    std::fs::create_dir_all(f.root().join("b")).unwrap(); // exists, not a repo
    basic_config(&f);
    let assert = f.ezgitx().arg("status").assert().code(1);
    let lines = jsonl(&assert.get_output().stdout);
    assert_eq!(line_for(&lines, "a")["state"], "clean");
    assert_eq!(line_for(&lines, "b")["error"]["code"], "not_a_repo");
}

#[test]
fn stale_deps_surface_for_dependent_repos() {
    let f = Fixture::new();
    f.repo("lib");
    f.repo("app");
    f.config(
        "version: 1\ngroups:\n  g:\n    - path: ./lib\n      default_cmd: \"true\"\n    - path: ./app\n      depends_on: [\"lib\"]\n",
    );
    // No freshness record yet: lib is stale from app's perspective.
    let assert = f
        .ezgitx()
        .args(["status", "--repo", "app"])
        .assert()
        .code(0);
    let lines = jsonl(&assert.get_output().stdout);
    assert_eq!(lines[0]["stale_deps"], serde_json::json!(["lib"]));

    // Build lib through ezgitx run, recording freshness.
    f.ezgitx().args(["run", "--repo", "lib"]).assert().code(0);
    let assert = f
        .ezgitx()
        .args(["status", "--repo", "app"])
        .assert()
        .code(0);
    let lines = jsonl(&assert.get_output().stdout);
    assert_eq!(lines[0]["stale_deps"], serde_json::json!([]));

    // A new commit in lib makes it stale again.
    f.commit(&f.root().join("lib"), "more.txt", "x");
    let assert = f
        .ezgitx()
        .args(["status", "--repo", "app"])
        .assert()
        .code(0);
    let lines = jsonl(&assert.get_output().stdout);
    assert_eq!(lines[0]["stale_deps"], serde_json::json!(["lib"]));
}

#[test]
fn human_mode_prints_table() {
    let f = Fixture::new();
    f.repo("a");
    f.repo("b");
    basic_config(&f);
    let assert = f.ezgitx().args(["status", "--human"]).assert().code(0);
    let out = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    assert!(out.contains("REPO"));
    assert!(out.contains("clean"));
    assert!(!out.contains('{'), "human mode must not emit JSON: {out}");
}

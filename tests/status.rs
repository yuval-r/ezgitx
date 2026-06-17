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
        "version: 1\ngroups:\n  g:\n    - path: ./lib\n      default_cmd: \"true\"\n    - path: ./app\n      default_cmd: \"true\"\n      depends_on: [\"lib\"]\n",
    );

    // No record for app yet: its single upstream counts as drift.
    let assert = f
        .ezgitx()
        .args(["status", "--repo", "app"])
        .assert()
        .code(0);
    let lines = jsonl(&assert.get_output().stdout);
    assert_eq!(lines[0]["stale_deps"], serde_json::json!(["lib"]));

    // Build app (and its upstream) so app records the lib head it built against.
    f.ezgitx()
        .args(["run", "--repo", "app", "--with-deps"])
        .assert()
        .code(0);
    let assert = f
        .ezgitx()
        .args(["status", "--repo", "app"])
        .assert()
        .code(0);
    let lines = jsonl(&assert.get_output().stdout);
    assert_eq!(lines[0]["stale_deps"], serde_json::json!([]));

    // A new commit in lib moves it past the head app built against.
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
fn rebuilding_shared_upstream_keeps_other_consumer_flagged() {
    let f = Fixture::new();
    f.repo("core");
    f.repo("lib");
    f.repo("tool");
    f.config(
        "version: 1\ngroups:\n  g:\n    - path: ./core\n      default_cmd: \"true\"\n    - path: ./lib\n      default_cmd: \"true\"\n      depends_on: [\"core\"]\n    - path: ./tool\n      default_cmd: \"true\"\n      depends_on: [\"core\"]\n",
    );

    // Build everything so all three have manifests.
    f.ezgitx()
        .args(["run", "--all", "--with-deps"])
        .assert()
        .code(0);

    // core moves, then we rebuild it for lib only.
    f.commit(&f.root().join("core"), "c.txt", "x");
    f.ezgitx()
        .args(["run", "--repo", "lib", "--with-deps"])
        .assert()
        .code(0);

    let status_deps = |repo: &str| -> serde_json::Value {
        let assert = f.ezgitx().args(["status", "--repo", repo]).assert().code(0);
        let lines = jsonl(&assert.get_output().stdout);
        lines[0]["stale_deps"].clone()
    };

    // lib was rebuilt against the new core; tool was not, so it stays flagged.
    assert_eq!(status_deps("lib"), serde_json::json!([]));
    assert_eq!(status_deps("tool"), serde_json::json!(["core"]));
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

#[test]
fn build_stale_when_never_run() {
    let f = Fixture::new();
    f.repo("a");
    f.config("version: 1\ngroups:\n  g:\n    - path: ./a\n");
    let assert = f.ezgitx().args(["status", "--repo", "a"]).assert().code(0);
    let lines = jsonl(&assert.get_output().stdout);
    // No recorded green build yet → stale.
    assert_eq!(lines[0]["build"], "stale");
}

#[test]
fn build_fresh_after_recorded_run() {
    let f = Fixture::new();
    f.repo("a");
    f.config("version: 1\ngroups:\n  g:\n    - path: ./a\n      default_cmd: \"true\"\n");
    // A successful run records the green build at the current HEAD.
    f.ezgitx().args(["run", "--repo", "a"]).assert().code(0);
    let assert = f.ezgitx().args(["status", "--repo", "a"]).assert().code(0);
    let lines = jsonl(&assert.get_output().stdout);
    assert_eq!(lines[0]["build"], "fresh");
}

#[test]
fn build_stale_after_head_moves() {
    let f = Fixture::new();
    let a = f.repo("a");
    f.config("version: 1\ngroups:\n  g:\n    - path: ./a\n      default_cmd: \"true\"\n");
    f.ezgitx().args(["run", "--repo", "a"]).assert().code(0);
    // A new commit moves HEAD past the recorded build.
    f.commit(&a, "more.txt", "x");
    let assert = f.ezgitx().args(["status", "--repo", "a"]).assert().code(0);
    let lines = jsonl(&assert.get_output().stdout);
    assert_eq!(lines[0]["build"], "stale");
}

#[test]
fn build_stale_when_upstream_drifts() {
    let f = Fixture::new();
    f.repo("lib");
    f.repo("app");
    f.config(
        "version: 1\ngroups:\n  g:\n    - path: ./lib\n      default_cmd: \"true\"\n    - path: ./app\n      default_cmd: \"true\"\n      depends_on: [\"lib\"]\n",
    );
    // Build app against the current lib head.
    f.ezgitx()
        .args(["run", "--repo", "app", "--with-deps"])
        .assert()
        .code(0);
    let assert = f
        .ezgitx()
        .args(["status", "--repo", "app"])
        .assert()
        .code(0);
    let lines = jsonl(&assert.get_output().stdout);
    assert_eq!(lines[0]["build"], "fresh");

    // Moving lib drifts app's upstream manifest → app's build is stale.
    f.commit(&f.root().join("lib"), "more.txt", "x");
    let assert = f
        .ezgitx()
        .args(["status", "--repo", "app"])
        .assert()
        .code(0);
    let lines = jsonl(&assert.get_output().stdout);
    assert_eq!(lines[0]["build"], "stale");
}

#[test]
fn human_mode_includes_build_column() {
    let f = Fixture::new();
    f.repo("a");
    f.repo("b");
    basic_config(&f);
    let assert = f.ezgitx().args(["status", "--human"]).assert().code(0);
    let out = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    assert!(out.contains("BUILD"), "missing BUILD column: {out}");
}

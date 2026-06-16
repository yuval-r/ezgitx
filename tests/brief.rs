mod common;

use common::*;

fn basic_config(f: &Fixture) {
    f.config("version: 1\ngroups:\n  g:\n    - path: ./a\n    - path: ./b\n");
}

fn one_repo_config(f: &Fixture) {
    f.config("version: 1\ngroups:\n  g:\n    - path: ./a\n");
}

#[test]
fn first_run_omits_delta() {
    let f = Fixture::new();
    f.repo("a");
    one_repo_config(&f);

    let assert = f.ezgitx().arg("brief").assert().code(0);
    let lines = jsonl(&assert.get_output().stdout);
    let a = line_for(&lines, "a");
    assert_eq!(a["state"], "clean");
    assert_eq!(a["branch"], "main");
    assert!(a.get("new_commits").is_none(), "first run has no delta");
    assert!(a.get("commits").is_none());
    assert!(a.get("truncated").is_none());
    // The baseline is still recorded so the next brief can delta against it.
    assert!(f.root().join(".ezgitx/state/a.brief.json").exists());
}

#[test]
fn second_run_shows_new_commits() {
    let f = Fixture::new();
    f.repo("a");
    one_repo_config(&f);

    f.ezgitx().arg("brief").assert().code(0); // record baseline
    f.commit(&f.root().join("a"), "one.txt", "1");
    f.commit(&f.root().join("a"), "two.txt", "2");

    let assert = f.ezgitx().arg("brief").assert().code(0);
    let lines = jsonl(&assert.get_output().stdout);
    let a = line_for(&lines, "a");
    assert_eq!(a["new_commits"], 2);
    let commits = a["commits"].as_array().unwrap();
    assert_eq!(commits.len(), 2);
    // Newest-first.
    assert_eq!(commits[0]["subject"], "add two.txt");
    assert_eq!(commits[1]["subject"], "add one.txt");
    assert!(commits[0]["sha"].as_str().unwrap().len() >= 40); // full sha
    assert_eq!(a["truncated"], false);
}

#[test]
fn idempotent_no_new_commits() {
    let f = Fixture::new();
    f.repo("a");
    one_repo_config(&f);

    f.ezgitx().arg("brief").assert().code(0);
    let assert = f.ezgitx().arg("brief").assert().code(0);
    let lines = jsonl(&assert.get_output().stdout);
    let a = line_for(&lines, "a");
    assert_eq!(a["new_commits"], 0);
    assert_eq!(a["commits"].as_array().unwrap().len(), 0);
    assert_eq!(a["truncated"], false);
}

#[test]
fn cap_truncates_commit_list() {
    let f = Fixture::new();
    f.repo("a");
    one_repo_config(&f);

    f.ezgitx().arg("brief").assert().code(0);
    for i in 0..21 {
        f.commit(&f.root().join("a"), &format!("f{i}.txt"), "x");
    }
    let assert = f.ezgitx().arg("brief").assert().code(0);
    let lines = jsonl(&assert.get_output().stdout);
    let a = line_for(&lines, "a");
    assert_eq!(a["new_commits"], 21); // full uncapped count
    assert_eq!(a["commits"].as_array().unwrap().len(), 20); // MAX_COMMITS
    assert_eq!(a["truncated"], true);
}

#[test]
fn unborn_head_brand_new_repo() {
    let f = Fixture::new();
    let a = f.root().join("a");
    std::fs::create_dir_all(&a).unwrap();
    git(&a, &["init", "-q", "-b", "main"]); // no commit → unborn HEAD
    one_repo_config(&f);

    let assert = f.ezgitx().arg("brief").assert().code(0);
    let lines = jsonl(&assert.get_output().stdout);
    let a_line = line_for(&lines, "a");
    assert_eq!(a_line["state"], "clean");
    assert!(a_line.get("new_commits").is_none());
    // Nothing to baseline on an unborn branch.
    assert!(!f.root().join(".ezgitx/state/a.brief.json").exists());

    // First commit → brief is a "first run" (records baseline, still no delta).
    f.commit(&a, "r.txt", "x");
    f.ezgitx().arg("brief").assert().code(0);
    assert!(f.root().join(".ezgitx/state/a.brief.json").exists());

    // A subsequent commit then shows up as a delta.
    f.commit(&a, "s.txt", "y");
    let assert = f.ezgitx().arg("brief").assert().code(0);
    let lines = jsonl(&assert.get_output().stdout);
    assert_eq!(line_for(&lines, "a")["new_commits"], 1);
}

#[test]
fn baseline_unreachable_degrades() {
    let f = Fixture::new();
    f.repo("a");
    one_repo_config(&f);

    // Plant a baseline pointing at a sha that doesn't exist in the repo.
    let p = f.root().join(".ezgitx/state/a.brief.json");
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(
        &p,
        r#"{"head":"dddddddddddddddddddddddddddddddddddddddd","recorded_at":"t"}"#,
    )
    .unwrap();

    let assert = f.ezgitx().arg("brief").assert().code(0);
    let lines = jsonl(&assert.get_output().stdout);
    let a = line_for(&lines, "a");
    assert_eq!(a["delta_unavailable"], "baseline_unreachable");
    assert!(a.get("new_commits").is_none());

    // brief re-baselined to the current HEAD → the next run is normal.
    let assert = f.ezgitx().arg("brief").assert().code(0);
    let lines = jsonl(&assert.get_output().stdout);
    assert_eq!(line_for(&lines, "a")["new_commits"], 0);
}

#[test]
fn detached_head_still_deltas() {
    let f = Fixture::new();
    let a = f.repo("a");
    one_repo_config(&f);

    f.ezgitx().arg("brief").assert().code(0); // baseline at commit 1
    f.commit(&a, "x.txt", "1");
    git(&a, &["checkout", "-q", "--detach"]);

    let assert = f.ezgitx().arg("brief").assert().code(0);
    let lines = jsonl(&assert.get_output().stdout);
    let a_line = line_for(&lines, "a");
    assert_eq!(a_line["state"], "detached");
    assert!(a_line["branch"].is_null());
    assert_eq!(a_line["new_commits"], 1);
}

#[test]
fn dirty_and_new_commits_coexist() {
    let f = Fixture::new();
    let a = f.repo("a");
    one_repo_config(&f);

    f.ezgitx().arg("brief").assert().code(0);
    f.commit(&a, "committed.txt", "c"); // committed delta
    std::fs::write(a.join("wip.txt"), "wip").unwrap(); // uncommitted

    let assert = f.ezgitx().arg("brief").assert().code(0);
    let lines = jsonl(&assert.get_output().stdout);
    let a_line = line_for(&lines, "a");
    assert_eq!(a_line["state"], "dirty");
    assert_eq!(a_line["new_commits"], 1);
}

#[test]
fn not_a_repo_reported() {
    let f = Fixture::new();
    f.repo("a");
    std::fs::create_dir_all(f.root().join("b")).unwrap(); // exists, not a repo
    basic_config(&f);

    let assert = f.ezgitx().arg("brief").assert().code(1);
    let lines = jsonl(&assert.get_output().stdout);
    assert_eq!(line_for(&lines, "a")["state"], "clean");
    assert_eq!(line_for(&lines, "b")["error"]["code"], "not_a_repo");
    // No baseline written for the unreadable repo.
    assert!(!f.root().join(".ezgitx/state/b.brief.json").exists());
}

#[test]
fn stale_deps_parity() {
    let f = Fixture::new();
    f.repo("lib");
    f.repo("app");
    f.config(
        "version: 1\ngroups:\n  g:\n    - path: ./lib\n      default_cmd: \"true\"\n    - path: ./app\n      default_cmd: \"true\"\n      depends_on: [\"lib\"]\n",
    );

    // No record for app yet: its single upstream counts as drift — same as `status`.
    let assert = f.ezgitx().args(["brief", "--repo", "app"]).assert().code(0);
    let lines = jsonl(&assert.get_output().stdout);
    assert_eq!(
        line_for(&lines, "app")["stale_deps"],
        serde_json::json!(["lib"])
    );
}

#[test]
fn summary_line_present() {
    let f = Fixture::new();
    f.repo("a");
    f.repo("b");
    basic_config(&f);

    let assert = f.ezgitx().arg("brief").assert().code(0);
    let lines = jsonl(&assert.get_output().stdout);
    let s = summary(&lines);
    assert_eq!(s["repos"], 2);
    assert_eq!(s["with_new_commits"], 0);
    assert_eq!(s["failed"], 0);
}

#[test]
fn no_record_does_not_advance_baseline() {
    let f = Fixture::new();
    f.repo("a");
    one_repo_config(&f);

    f.ezgitx().arg("brief").assert().code(0); // baseline
    f.commit(&f.root().join("a"), "x.txt", "x");

    // Peek twice: both show the delta, neither advances the baseline.
    for _ in 0..2 {
        let assert = f.ezgitx().args(["brief", "--no-record"]).assert().code(0);
        let lines = jsonl(&assert.get_output().stdout);
        assert_eq!(line_for(&lines, "a")["new_commits"], 1);
    }

    // A normal brief advances it → the next run reports zero.
    f.ezgitx().arg("brief").assert().code(0);
    let assert = f.ezgitx().arg("brief").assert().code(0);
    let lines = jsonl(&assert.get_output().stdout);
    assert_eq!(line_for(&lines, "a")["new_commits"], 0);
}

#[test]
fn dirty_does_not_consume_hidden_repo_delta() {
    // A `--dirty` brief must NOT advance the baseline of a clean repo it didn't
    // display — otherwise commits made to that repo silently vanish from the
    // delta stream. Only repos actually shown get their baseline advanced.
    let f = Fixture::new();
    let a = f.repo("a"); // made dirty below
    f.repo("b"); // clean; gets a commit while hidden
    basic_config(&f);

    // Steady state: both repos already have a baseline.
    f.ezgitx().arg("brief").assert().code(0);

    // A commit lands in the clean repo `b`; then `a` is made dirty.
    f.commit(&f.root().join("b"), "feature.txt", "important");
    std::fs::write(a.join("wip.txt"), "wip").unwrap();

    // `brief --dirty` shows only `a` and must leave `b`'s baseline untouched.
    let assert = f.ezgitx().args(["brief", "--dirty"]).assert().code(0);
    let lines = jsonl(&assert.get_output().stdout);
    assert!(lines.iter().any(|l| l["repo"] == "a"));
    assert!(!lines.iter().any(|l| l["repo"] == "b"));

    // The next unfiltered brief STILL surfaces b's commit (not swallowed).
    let assert = f.ezgitx().args(["brief", "--repo", "b"]).assert().code(0);
    let lines = jsonl(&assert.get_output().stdout);
    assert_eq!(line_for(&lines, "b")["new_commits"], 1);
    assert_eq!(
        line_for(&lines, "b")["commits"][0]["subject"],
        "add feature.txt"
    );
}

#[test]
fn dirty_advances_baseline_of_shown_repo() {
    // The flip side: a repo that IS displayed under --dirty does advance, so its
    // already-shown committed delta isn't repeated next time.
    let f = Fixture::new();
    let a = f.repo("a");
    f.repo("b");
    basic_config(&f);
    f.ezgitx().arg("brief").assert().code(0); // baseline both

    f.commit(&a, "x.txt", "x"); // committed delta in a
    std::fs::write(a.join("wip.txt"), "wip").unwrap(); // a is dirty

    let assert = f.ezgitx().args(["brief", "--dirty"]).assert().code(0);
    let lines = jsonl(&assert.get_output().stdout);
    assert_eq!(line_for(&lines, "a")["new_commits"], 1); // shown with its delta

    // a was shown → its baseline advanced → the committed delta isn't repeated.
    let assert = f.ezgitx().args(["brief", "--repo", "a"]).assert().code(0);
    let lines = jsonl(&assert.get_output().stdout);
    assert_eq!(line_for(&lines, "a")["new_commits"], 0);
}

#[test]
fn human_mode_table() {
    let f = Fixture::new();
    f.repo("a");
    f.repo("b");
    basic_config(&f);

    let assert = f.ezgitx().args(["brief", "--human"]).assert().code(0);
    let out = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    assert!(out.contains("REPO"));
    assert!(out.contains("NEW"));
    assert!(!out.contains('{'), "human mode must not emit JSON: {out}");
}

#[test]
fn subject_with_newlines_and_unicode() {
    let f = Fixture::new();
    let a = f.repo("a");
    one_repo_config(&f);

    f.ezgitx().arg("brief").assert().code(0); // baseline
    std::fs::write(a.join("u.txt"), "x").unwrap();
    git(&a, &["add", "."]);
    git(&a, &["commit", "-q", "-m", "héllo wörld 日本\n\nbody line"]);

    let assert = f.ezgitx().arg("brief").assert().code(0);
    let lines = jsonl(&assert.get_output().stdout);
    let a_line = line_for(&lines, "a");
    assert_eq!(a_line["new_commits"], 1);
    // `%s` is the subject (first line) only.
    assert_eq!(a_line["commits"][0]["subject"], "héllo wörld 日本");
}

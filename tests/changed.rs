mod common;

use common::*;

fn basic_config(f: &Fixture) {
    f.config("version: 1\ngroups:\n  g:\n    - path: ./a\n    - path: ./b\n");
}

fn one_repo_config(f: &Fixture) {
    f.config("version: 1\ngroups:\n  g:\n    - path: ./a\n");
}

#[test]
fn since_explicit_ref_lists_files_and_commits() {
    let f = Fixture::new();
    let a = f.repo("a");
    one_repo_config(&f);
    f.commit(&a, "one.txt", "1");
    f.commit(&a, "two.txt", "2");

    let assert = f
        .ezgitx()
        .args(["changed", "--since", "HEAD~2"])
        .assert()
        .code(0);
    let lines = jsonl(&assert.get_output().stdout);
    let a = line_for(&lines, "a");
    assert_eq!(a["since_ref"], "HEAD~2");
    assert_eq!(a["new_commits"], 2);
    assert!(a["from"].is_string() && a["to"].is_string());
    let files = a["files"].as_array().unwrap();
    let paths: Vec<&str> = files.iter().map(|f| f["path"].as_str().unwrap()).collect();
    assert!(paths.contains(&"one.txt") && paths.contains(&"two.txt"));
    assert!(files.iter().all(|f| f["status"] == "A"));
    assert_eq!(a["truncated"], false);
}

#[test]
fn default_since_is_last_brief() {
    let f = Fixture::new();
    let a = f.repo("a");
    one_repo_config(&f);
    f.ezgitx().arg("brief").assert().code(0); // record baseline
    f.commit(&a, "feat.txt", "x");

    // Bare `changed` == `--since last-brief`.
    let assert = f.ezgitx().arg("changed").assert().code(0);
    let lines = jsonl(&assert.get_output().stdout);
    let a = line_for(&lines, "a");
    assert_eq!(a["since_ref"], "last-brief");
    assert_eq!(a["new_commits"], 1);
    assert_eq!(a["files"][0]["status"], "A");
    assert_eq!(a["files"][0]["path"], "feat.txt");
}

#[test]
fn no_baseline_degrades() {
    let f = Fixture::new();
    f.repo("a");
    one_repo_config(&f);
    let assert = f.ezgitx().arg("changed").assert().code(0);
    let lines = jsonl(&assert.get_output().stdout);
    let a = line_for(&lines, "a");
    assert_eq!(a["delta_unavailable"], "no_baseline");
    assert!(a.get("new_commits").is_none());
}

#[test]
fn ref_not_found_degrades() {
    let f = Fixture::new();
    f.repo("a");
    one_repo_config(&f);
    let assert = f
        .ezgitx()
        .args(["changed", "--since", "no-such-ref"])
        .assert()
        .code(0);
    let lines = jsonl(&assert.get_output().stdout);
    assert_eq!(line_for(&lines, "a")["delta_unavailable"], "ref_not_found");
}

#[test]
fn file_statuses_added_modified_deleted() {
    let f = Fixture::new();
    let a = f.repo("a");
    one_repo_config(&f);
    f.commit(&a, "todelete.txt", "x"); // exists at the base ref

    // One commit: add new.txt, modify README.md, delete todelete.txt.
    std::fs::write(a.join("new.txt"), "n").unwrap();
    std::fs::write(a.join("README.md"), "changed").unwrap();
    std::fs::remove_file(a.join("todelete.txt")).unwrap();
    git(&a, &["add", "-A"]);
    git(&a, &["commit", "-q", "-m", "mixed change"]);

    let assert = f
        .ezgitx()
        .args(["changed", "--since", "HEAD~1"])
        .assert()
        .code(0);
    let lines = jsonl(&assert.get_output().stdout);
    let files = line_for(&lines, "a")["files"].as_array().unwrap();
    let status_of = |p: &str| {
        files
            .iter()
            .find(|f| f["path"] == p)
            .map(|f| f["status"].as_str().unwrap().to_string())
    };
    assert_eq!(status_of("new.txt"), Some("A".into()));
    assert_eq!(status_of("README.md"), Some("M".into()));
    assert_eq!(status_of("todelete.txt"), Some("D".into()));
}

#[test]
fn rename_reports_new_path() {
    let f = Fixture::new();
    let a = f.repo("a");
    one_repo_config(&f);
    f.commit(
        &a,
        "original.txt",
        "identical content kept across the rename so git detects it",
    );
    git(&a, &["mv", "original.txt", "renamed.txt"]);
    git(&a, &["commit", "-q", "-m", "rename"]);

    let assert = f
        .ezgitx()
        .args(["changed", "--since", "HEAD~1"])
        .assert()
        .code(0);
    let lines = jsonl(&assert.get_output().stdout);
    let files = line_for(&lines, "a")["files"].as_array().unwrap();
    assert!(
        files
            .iter()
            .any(|f| f["status"] == "R" && f["path"] == "renamed.txt"),
        "expected rename R -> renamed.txt, got {files:?}"
    );
}

#[test]
fn cap_truncates_files() {
    let f = Fixture::new();
    let a = f.repo("a");
    one_repo_config(&f);
    for i in 0..55 {
        std::fs::write(a.join(format!("f{i}.txt")), "x").unwrap();
    }
    git(&a, &["add", "-A"]);
    git(&a, &["commit", "-q", "-m", "many files"]);

    let assert = f
        .ezgitx()
        .args(["changed", "--since", "HEAD~1"])
        .assert()
        .code(0);
    let lines = jsonl(&assert.get_output().stdout);
    let a = line_for(&lines, "a");
    assert_eq!(a["files"].as_array().unwrap().len(), 50); // MAX_FILES
    assert_eq!(a["truncated"], true);
    assert_eq!(a["new_commits"], 1);
}

#[test]
fn nothing_since_head_is_empty() {
    let f = Fixture::new();
    f.repo("a");
    one_repo_config(&f);
    let assert = f
        .ezgitx()
        .args(["changed", "--since", "HEAD"])
        .assert()
        .code(0);
    let lines = jsonl(&assert.get_output().stdout);
    let a = line_for(&lines, "a");
    assert_eq!(a["new_commits"], 0);
    assert_eq!(a["files"].as_array().unwrap().len(), 0);
    assert_eq!(a["truncated"], false);
}

#[test]
fn not_a_repo_reported() {
    let f = Fixture::new();
    f.repo("a");
    std::fs::create_dir_all(f.root().join("b")).unwrap();
    basic_config(&f);
    let assert = f
        .ezgitx()
        .args(["changed", "--since", "HEAD"])
        .assert()
        .code(1);
    let lines = jsonl(&assert.get_output().stdout);
    assert_eq!(line_for(&lines, "b")["error"]["code"], "not_a_repo");
}

#[test]
fn unborn_degrades() {
    let f = Fixture::new();
    let a = f.root().join("a");
    std::fs::create_dir_all(&a).unwrap();
    git(&a, &["init", "-q", "-b", "main"]);
    one_repo_config(&f);
    let assert = f
        .ezgitx()
        .args(["changed", "--since", "HEAD"])
        .assert()
        .code(0);
    let lines = jsonl(&assert.get_output().stdout);
    assert_eq!(line_for(&lines, "a")["delta_unavailable"], "unborn");
}

#[test]
fn targeting_repo() {
    let f = Fixture::new();
    let a = f.repo("a");
    f.repo("b");
    basic_config(&f);
    f.commit(&a, "x.txt", "1");
    let assert = f
        .ezgitx()
        .args(["changed", "--since", "HEAD~1", "--repo", "a"])
        .assert()
        .code(0);
    let lines = jsonl(&assert.get_output().stdout);
    assert!(lines.iter().any(|l| l["repo"] == "a"));
    assert!(!lines.iter().any(|l| l["repo"] == "b"));
}

#[test]
fn summary_and_multi_repo_ref_resolution() {
    // `b` has only its initial commit, so HEAD~1 doesn't resolve there: it
    // degrades (ref_not_found) while `a` reports its delta — the run stays exit 0.
    let f = Fixture::new();
    let a = f.repo("a");
    f.repo("b");
    basic_config(&f);
    f.commit(&a, "x.txt", "1");

    let assert = f
        .ezgitx()
        .args(["changed", "--since", "HEAD~1"])
        .assert()
        .code(0);
    let lines = jsonl(&assert.get_output().stdout);
    let s = summary(&lines);
    assert_eq!(s["repos"], 2);
    assert_eq!(s["with_changes"], 1);
    assert_eq!(s["failed"], 0);
    assert_eq!(line_for(&lines, "b")["delta_unavailable"], "ref_not_found");
}

#[test]
fn human_mode_table() {
    let f = Fixture::new();
    let a = f.repo("a");
    one_repo_config(&f);
    f.commit(&a, "x.txt", "1");
    let assert = f
        .ezgitx()
        .args(["changed", "--since", "HEAD~1", "--human"])
        .assert()
        .code(0);
    let out = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    assert!(out.contains("RANGE"));
    assert!(out.contains("FILES"));
    assert!(!out.contains('{'), "human mode must not emit JSON: {out}");
}

#[test]
fn path_with_spaces_and_unicode() {
    let f = Fixture::new();
    let a = f.repo("a");
    one_repo_config(&f);
    std::fs::write(a.join("a file ünïcode.txt"), "x").unwrap();
    git(&a, &["add", "-A"]);
    git(&a, &["commit", "-q", "-m", "weird path"]);
    let assert = f
        .ezgitx()
        .args(["changed", "--since", "HEAD~1"])
        .assert()
        .code(0);
    let lines = jsonl(&assert.get_output().stdout);
    let files = line_for(&lines, "a")["files"].as_array().unwrap();
    assert!(files.iter().any(|f| f["path"] == "a file ünïcode.txt"));
}

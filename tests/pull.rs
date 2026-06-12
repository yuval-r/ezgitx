mod common;

use common::*;

fn config_ab(f: &Fixture) {
    f.config("version: 1\ngroups:\n  g:\n    - path: ./a\n    - path: ./b\n");
}

#[test]
fn up_to_date_and_updated() {
    let f = Fixture::new();
    f.repo_with_remote("a");
    f.repo_with_remote("b");
    config_ab(&f);

    f.push_upstream_commit("a", "one.txt");
    f.push_upstream_commit("a", "two.txt");

    let assert = f.ezgitx().arg("pull").assert().code(0);
    let lines = jsonl(&assert.get_output().stdout);
    let a = line_for(&lines, "a");
    assert_eq!(a["status"], "updated");
    assert_eq!(a["commits_pulled"], 2);
    assert_eq!(a["head"].as_str().unwrap().len(), 7);
    let b = line_for(&lines, "b");
    assert_eq!(b["status"], "up_to_date");
    assert_eq!(b["commits_pulled"], 0);
}

#[test]
fn dirty_repo_is_skipped_but_fetch_runs() {
    let f = Fixture::new();
    let a = f.repo_with_remote("a");
    f.repo_with_remote("b");
    config_ab(&f);
    f.push_upstream_commit("a", "new.txt");
    std::fs::write(a.join("wip.txt"), "wip").unwrap();

    let assert = f.ezgitx().arg("pull").assert().code(1);
    let lines = jsonl(&assert.get_output().stdout);
    let line = line_for(&lines, "a");
    assert_eq!(line["status"], "skipped_dirty");
    assert_eq!(line["error"]["code"], "dirty_tree");

    // The fetch still ran, so `behind` is now accurate without another fetch.
    let assert = f.ezgitx().args(["status", "--repo", "a"]).assert().code(0);
    let lines = jsonl(&assert.get_output().stdout);
    assert_eq!(lines[0]["behind"], 1);
}

#[test]
fn diverged_branch_is_reported() {
    let f = Fixture::new();
    let a = f.repo_with_remote("a");
    f.repo_with_remote("b");
    config_ab(&f);
    f.push_upstream_commit("a", "theirs.txt");
    f.commit(&a, "ours.txt", "local");

    let assert = f.ezgitx().arg("pull").assert().code(1);
    let lines = jsonl(&assert.get_output().stdout);
    let line = line_for(&lines, "a");
    assert_eq!(line["status"], "diverged");
    assert_eq!(line["error"]["code"], "diverged");
    // Never creates merge commits: HEAD is still our local commit.
}

#[test]
fn detached_head_is_reported() {
    let f = Fixture::new();
    let a = f.repo_with_remote("a");
    f.repo_with_remote("b");
    config_ab(&f);
    git(&a, &["checkout", "-q", "--detach"]);

    let assert = f.ezgitx().args(["pull", "--repo", "a"]).assert().code(1);
    let lines = jsonl(&assert.get_output().stdout);
    assert_eq!(lines[0]["status"], "detached");
    assert_eq!(lines[0]["error"]["code"], "detached");
}

#[test]
fn one_failure_never_stops_others() {
    let f = Fixture::new();
    let a = f.repo_with_remote("a");
    f.repo_with_remote("b");
    config_ab(&f);
    std::fs::write(a.join("wip.txt"), "wip").unwrap();
    f.push_upstream_commit("b", "new.txt");

    let assert = f.ezgitx().arg("pull").assert().code(1);
    let lines = jsonl(&assert.get_output().stdout);
    assert_eq!(line_for(&lines, "a")["status"], "skipped_dirty");
    assert_eq!(line_for(&lines, "b")["status"], "updated");
}

#[test]
fn no_remote_is_a_per_repo_error() {
    let f = Fixture::new();
    f.repo("a"); // no remote at all
    f.repo_with_remote("b");
    config_ab(&f);
    let assert = f.ezgitx().arg("pull").assert().code(1);
    let lines = jsonl(&assert.get_output().stdout);
    assert_eq!(line_for(&lines, "a")["status"], "error");
    assert_eq!(line_for(&lines, "a")["error"]["code"], "git_failed");
    assert_eq!(line_for(&lines, "b")["status"], "up_to_date");
}

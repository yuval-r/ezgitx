mod common;

use common::*;

#[test]
fn runs_command_in_each_repo() {
    let f = Fixture::new();
    f.repo("a");
    f.repo("b");
    f.config("version: 1\ngroups:\n  g:\n    - path: ./a\n    - path: ./b\n");

    let assert = f
        .ezgitx()
        .args(["run", "echo hi from $PWD"])
        .assert()
        .code(0);
    let lines = jsonl(&assert.get_output().stdout);
    let a = line_for(&lines, "a");
    assert_eq!(a["exit_code"], 0);
    assert!(a["stdout_tail"].as_str().unwrap().contains("hi from"));
    assert!(a["stdout_tail"].as_str().unwrap().contains("/a"));
    assert_eq!(a["truncated"], false);
    assert!(a["duration_ms"].is_u64());

    let s = summary(&lines);
    assert_eq!(s["total"], 2);
    assert_eq!(s["passed"], 2);
    assert_eq!(s["failed"], 0);
}

#[test]
fn failing_command_sets_exit_1() {
    let f = Fixture::new();
    f.repo("a");
    f.repo("b");
    f.config("version: 1\ngroups:\n  g:\n    - path: ./a\n    - path: ./b\n");

    let assert = f
        .ezgitx()
        .args(["run", "test $(basename $PWD) != a"])
        .assert()
        .code(1);
    let lines = jsonl(&assert.get_output().stdout);
    assert_eq!(line_for(&lines, "a")["exit_code"], 1);
    assert_eq!(line_for(&lines, "b")["exit_code"], 0);
    let s = summary(&lines);
    assert_eq!(s["passed"], 1);
    assert_eq!(s["failed"], 1);
}

#[test]
fn default_cmd_and_no_default_cmd() {
    let f = Fixture::new();
    f.repo("a");
    f.repo("b");
    f.config(
        "version: 1\ngroups:\n  g:\n    - path: ./a\n      default_cmd: \"echo built-a\"\n    - path: ./b\n",
    );
    let assert = f.ezgitx().arg("run").assert().code(1);
    let lines = jsonl(&assert.get_output().stdout);
    let a = line_for(&lines, "a");
    assert_eq!(a["exit_code"], 0);
    assert!(a["stdout_tail"].as_str().unwrap().contains("built-a"));
    let b = line_for(&lines, "b");
    assert!(b["exit_code"].is_null());
    assert_eq!(b["error"]["code"], "no_default_cmd");
}

#[test]
fn output_tails_are_byte_capped() {
    let f = Fixture::new();
    f.repo("a");
    f.config("version: 1\ngroups:\n  g:\n    - path: ./a\n");
    let assert = f
        .ezgitx()
        .args(["--max-bytes", "16", "run", "seq 1 1000"])
        .assert()
        .code(0);
    let lines = jsonl(&assert.get_output().stdout);
    let a = line_for(&lines, "a");
    assert_eq!(a["truncated"], true);
    assert!(a["stdout_tail"].as_str().unwrap().len() <= 16);
    assert!(a["stdout_tail"].as_str().unwrap().contains("1000"));
}

#[test]
fn targets_sibling_repo_from_inside_another() {
    let f = Fixture::new();
    f.repo("a");
    f.repo("b");
    f.config("version: 1\ngroups:\n  g:\n    - path: ./a\n    - path: ./b\n");
    // From inside ./a, target b explicitly (PRD §4.2).
    let assert = f
        .ezgitx_in(&f.root().join("a"))
        .args(["run", "--repo", "b", "basename $PWD"])
        .assert()
        .code(0);
    let lines = jsonl(&assert.get_output().stdout);
    assert_eq!(lines.len(), 2); // b + summary
    assert!(
        line_for(&lines, "b")["stdout_tail"]
            .as_str()
            .unwrap()
            .contains('b')
    );
}

#[test]
fn no_flags_inside_repo_targets_only_it() {
    let f = Fixture::new();
    f.repo("a");
    f.repo("b");
    f.config("version: 1\ngroups:\n  g:\n    - path: ./a\n    - path: ./b\n");
    let assert = f
        .ezgitx_in(&f.root().join("a"))
        .args(["run", "basename $PWD"])
        .assert()
        .code(0);
    let lines = jsonl(&assert.get_output().stdout);
    assert_eq!(summary(&lines)["total"], 1);
    assert!(lines.iter().any(|l| l["repo"] == "a"));
}

#[test]
fn run_ignores_held_repo_locks() {
    let f = Fixture::new();
    f.repo("a");
    f.config("version: 1\ngroups:\n  g:\n    - path: ./a\n");
    // A live lock on the repo must not block `run` (PRD §7: run takes no locks).
    let lock_dir = f.root().join(".ezgitx/locks");
    std::fs::create_dir_all(&lock_dir).unwrap();
    std::fs::write(
        lock_dir.join("repo-a.lock"),
        format!(
            r#"{{"pid": {}, "hostname": "{}", "started_at": "{}", "op": "pull"}}"#,
            std::process::id(),
            gethostname(),
            now_iso()
        ),
    )
    .unwrap();
    f.ezgitx().args(["run", "true"]).assert().code(0);
}

fn gethostname() -> String {
    String::from_utf8_lossy(
        &std::process::Command::new("hostname")
            .output()
            .map(|o| o.stdout)
            .unwrap_or_default(),
    )
    .trim()
    .to_string()
}

fn now_iso() -> String {
    let out = std::process::Command::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

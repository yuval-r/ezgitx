mod common;

use common::*;

#[test]
fn missing_config_is_exit_2() {
    let dir = tempfile::tempdir().unwrap();
    let mut cmd = assert_cmd::Command::cargo_bin("ezgitx").unwrap();
    let assert = cmd.current_dir(dir.path()).arg("status").assert().code(2);
    let lines = jsonl(&assert.get_output().stdout);
    assert_eq!(lines[0]["error"]["code"], "config_invalid");
}

#[test]
fn unknown_key_is_rejected() {
    let f = Fixture::new();
    f.config("version: 1\ngroups:\n  g:\n    - path: ./a\n      surprise: 1\n");
    let assert = f.ezgitx().arg("status").assert().code(2);
    let lines = jsonl(&assert.get_output().stdout);
    assert_eq!(lines[0]["error"]["code"], "config_invalid");
}

#[test]
fn bad_version_is_rejected() {
    let f = Fixture::new();
    f.config("version: 7\ngroups: {}\n");
    f.ezgitx().arg("status").assert().code(2);
}

#[test]
fn dependency_cycle_fails_at_load() {
    let f = Fixture::new();
    f.repo("a");
    f.repo("b");
    f.config(
        "version: 1\ngroups:\n  g:\n    - path: ./a\n      depends_on: [\"b\"]\n    - path: ./b\n      depends_on: [\"a\"]\n",
    );
    let assert = f.ezgitx().arg("status").assert().code(2);
    let lines = jsonl(&assert.get_output().stdout);
    assert_eq!(lines[0]["error"]["code"], "dependency_cycle");
}

#[test]
fn unknown_dependency_is_rejected() {
    let f = Fixture::new();
    f.repo("a");
    f.config("version: 1\ngroups:\n  g:\n    - path: ./a\n      depends_on: [\"ghost\"]\n");
    f.ezgitx().arg("status").assert().code(2);
}

#[test]
fn unknown_repo_flag_is_exit_2() {
    let f = Fixture::new();
    f.repo("a");
    f.config("version: 1\ngroups:\n  g:\n    - path: ./a\n");
    f.ezgitx()
        .args(["status", "--repo", "ghost"])
        .assert()
        .code(2);
}

#[test]
fn unknown_group_flag_is_exit_2() {
    let f = Fixture::new();
    f.repo("a");
    f.config("version: 1\ngroups:\n  g:\n    - path: ./a\n");
    f.ezgitx()
        .args(["status", "--group", "ghost"])
        .assert()
        .code(2);
}

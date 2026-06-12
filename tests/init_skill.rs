mod common;

use common::*;

#[test]
fn writes_skill_file_at_workspace_root() {
    let f = Fixture::new();
    f.repo("a");
    f.config("version: 1\ngroups:\n  g:\n    - path: ./a\n");

    // Works from inside a member repo too: the file lands at the root.
    let assert = f
        .ezgitx_in(&f.root().join("a"))
        .arg("init-skill")
        .assert()
        .code(0);
    let lines = jsonl(&assert.get_output().stdout);
    assert_eq!(lines[0]["status"], "written");

    let path = f.root().join(".claude/skills/ezgitx/SKILL.md");
    assert!(path.exists());
    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains("JSONL"));
    assert!(content.contains("lock_held"));
    assert!(content.contains("--with-deps"));
    assert!(content.contains("check-impact"));
    assert!(content.starts_with("---\nname: ezgitx"));
}

#[test]
fn rerunning_overwrites_idempotently() {
    let f = Fixture::new();
    f.repo("a");
    f.config("version: 1\ngroups:\n  g:\n    - path: ./a\n");
    f.ezgitx().arg("init-skill").assert().code(0);

    let path = f.root().join(".claude/skills/ezgitx/SKILL.md");
    std::fs::write(&path, "stale edits").unwrap();
    f.ezgitx().arg("init-skill").assert().code(0);
    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.contains("JSONL"));
}

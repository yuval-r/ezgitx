mod common;

use common::*;

#[test]
fn held_repo_lock_fails_pull_with_exit_3() {
    let f = Fixture::new();
    f.repo_with_remote("a");
    f.config("version: 1\ngroups:\n  g:\n    - path: ./a\n");
    let lock_dir = f.root().join(".ezgitx/locks");
    std::fs::create_dir_all(&lock_dir).unwrap();
    std::fs::write(lock_dir.join("repo-a.lock"), live_lock_json("pull")).unwrap();

    let assert = f.ezgitx().arg("pull").assert().code(3);
    let lines = jsonl(&assert.get_output().stdout);
    let a = line_for(&lines, "a");
    assert_eq!(a["status"], "error");
    assert_eq!(a["error"]["code"], "lock_held");
}

#[test]
fn held_workspace_lock_blocks_pull() {
    let f = Fixture::new();
    f.repo_with_remote("a");
    f.config("version: 1\ngroups:\n  g:\n    - path: ./a\n");
    let lock_dir = f.root().join(".ezgitx/locks");
    std::fs::create_dir_all(&lock_dir).unwrap();
    std::fs::write(lock_dir.join("workspace.lock"), live_lock_json("sync")).unwrap();

    let assert = f.ezgitx().arg("pull").assert().code(3);
    let lines = jsonl(&assert.get_output().stdout);
    assert_eq!(lines[0]["error"]["code"], "lock_held");
}

#[test]
fn stale_repo_lock_is_broken_and_pull_proceeds() {
    let f = Fixture::new();
    f.repo_with_remote("a");
    f.config("version: 1\ngroups:\n  g:\n    - path: ./a\n");
    let lock_dir = f.root().join(".ezgitx/locks");
    std::fs::create_dir_all(&lock_dir).unwrap();
    // Dead PID on this host: stale, broken automatically with a stderr notice.
    std::fs::write(
        lock_dir.join("repo-a.lock"),
        live_lock_json("pull").replace(
            &format!("\"pid\": {}", std::process::id()),
            "\"pid\": 999999999",
        ),
    )
    .unwrap();

    let assert = f.ezgitx().arg("pull").assert().code(0);
    let lines = jsonl(&assert.get_output().stdout);
    assert_eq!(line_for(&lines, "a")["status"], "up_to_date");
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(stderr.contains("stale lock"), "stderr: {stderr}");
}

#[test]
fn wait_flag_retries_until_release() {
    let f = Fixture::new();
    f.repo_with_remote("a");
    f.config("version: 1\ngroups:\n  g:\n    - path: ./a\n");
    let lock_dir = f.root().join(".ezgitx/locks");
    std::fs::create_dir_all(&lock_dir).unwrap();
    let lock_path = lock_dir.join("repo-a.lock");
    std::fs::write(&lock_path, live_lock_json("pull")).unwrap();

    // Release the lock from another thread after ~600ms.
    let release_path = lock_path.clone();
    let releaser = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(600));
        std::fs::remove_file(&release_path).unwrap();
    });

    f.ezgitx().args(["pull", "--wait", "10"]).assert().code(0);
    releaser.join().unwrap();
}

#[test]
fn pull_releases_locks_on_completion() {
    let f = Fixture::new();
    f.repo_with_remote("a");
    f.config("version: 1\ngroups:\n  g:\n    - path: ./a\n");
    f.ezgitx().arg("pull").assert().code(0);
    assert!(!f.root().join(".ezgitx/locks/repo-a.lock").exists());
}

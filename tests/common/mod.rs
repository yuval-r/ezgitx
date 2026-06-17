#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

/// A temp workspace of real git repos for end-to-end tests: repos live at the
/// root, bare "origins" under `.remotes/`, and writer clones (for pushing
/// upstream commits) under `.writers/`.
pub struct Fixture {
    pub dir: TempDir,
}

impl Fixture {
    pub fn new() -> Self {
        Fixture {
            dir: TempDir::new().expect("tempdir"),
        }
    }

    pub fn root(&self) -> &Path {
        self.dir.path()
    }

    pub fn config(&self, yaml: &str) {
        std::fs::write(self.root().join(".ezgitx.yml"), yaml).expect("write config");
    }

    /// Local-only repo (no remote) with one commit.
    pub fn repo(&self, name: &str) -> PathBuf {
        let path = self.root().join(name);
        std::fs::create_dir_all(&path).unwrap();
        git(&path, &["init", "-q", "-b", "main"]);
        self.commit(&path, "README.md", "init");
        path
    }

    /// Repo cloned from a bare origin, so pull/fetch work. Returns the
    /// workspace clone's path.
    pub fn repo_with_remote(&self, name: &str) -> PathBuf {
        let bare = self.root().join(".remotes").join(format!("{name}.git"));
        std::fs::create_dir_all(&bare).unwrap();
        git(&bare, &["init", "-q", "--bare", "-b", "main"]);

        let writer = self.writer(name);
        std::fs::create_dir_all(&writer).unwrap();
        git(&writer, &["init", "-q", "-b", "main"]);
        git(
            &writer,
            &["remote", "add", "origin", bare.to_str().unwrap()],
        );
        self.commit(&writer, "README.md", "init");
        git(&writer, &["push", "-q", "-u", "origin", "main"]);

        let clone = self.root().join(name);
        git(
            self.root(),
            &[
                "clone",
                "-q",
                bare.to_str().unwrap(),
                clone.to_str().unwrap(),
            ],
        );
        clone
    }

    pub fn writer(&self, name: &str) -> PathBuf {
        self.root().join(".writers").join(name)
    }

    /// Commit a file in the writer clone and push, making the workspace
    /// clone's upstream move.
    pub fn push_upstream_commit(&self, name: &str, file: &str) {
        let writer = self.writer(name);
        self.commit(&writer, file, "upstream change");
        git(&writer, &["push", "-q", "origin", "main"]);
    }

    pub fn commit(&self, repo: &Path, file: &str, content: &str) {
        std::fs::write(repo.join(file), content).unwrap();
        git(repo, &["add", "."]);
        git(repo, &["commit", "-q", "-m", &format!("add {file}")]);
    }

    /// An `ezgitx` invocation rooted at the workspace (or a subdir).
    pub fn ezgitx(&self) -> assert_cmd::Command {
        self.ezgitx_in(self.root())
    }

    pub fn ezgitx_in(&self, dir: &Path) -> assert_cmd::Command {
        let mut cmd = assert_cmd::Command::cargo_bin("ezgitx").expect("binary");
        cmd.current_dir(dir).env("SHELL", "/bin/sh");
        git_env(cmd.env("NO_COLOR", "1"));
        cmd
    }
}

pub fn git(dir: &Path, args: &[&str]) {
    let mut cmd = Command::new("git");
    cmd.args(args).current_dir(dir);
    git_env_std(&mut cmd);
    let out = cmd.output().expect("spawn git");
    assert!(
        out.status.success(),
        "git {:?} failed in {}: {}",
        args,
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn git_env_std(cmd: &mut Command) {
    cmd.env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0");
}

fn git_env(cmd: &mut assert_cmd::Command) {
    cmd.env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null");
}

/// Parse stdout as JSONL.
pub fn jsonl(output: &[u8]) -> Vec<serde_json::Value> {
    String::from_utf8_lossy(output)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("bad JSONL line {l:?}: {e}")))
        .collect()
}

/// The line for a given repo (panics if absent).
pub fn line_for<'a>(lines: &'a [serde_json::Value], repo: &str) -> &'a serde_json::Value {
    lines
        .iter()
        .find(|l| l["repo"] == repo)
        .unwrap_or_else(|| panic!("no line for repo {repo} in {lines:?}"))
}

/// The trailing summary line, if any.
pub fn summary(lines: &[serde_json::Value]) -> &serde_json::Value {
    lines
        .iter()
        .find(|l| l["type"] == "summary")
        .expect("summary line")
}

/// JSON for a live advisory lock held by THIS process (its pid is alive on this
/// host, so liveness checks treat it as held).
pub fn live_lock_json(op: &str) -> String {
    lock_json(std::process::id(), op)
}

/// JSON for a stale advisory lock: a dead pid on this host, so liveness checks
/// treat it as breakable / inactive.
pub fn stale_lock_json(op: &str) -> String {
    lock_json(999_999_999, op)
}

fn lock_json(pid: u32, op: &str) -> String {
    // Mirror exactly what `lock.rs` writes: gethostname + an RFC 3339 jiff
    // timestamp, so liveness/TTL classification matches production.
    let hostname = gethostname::gethostname().to_string_lossy().into_owned();
    let started_at = jiff::Timestamp::now();
    format!(
        r#"{{"pid": {pid}, "hostname": "{hostname}", "started_at": "{started_at}", "op": "{op}"}}"#
    )
}

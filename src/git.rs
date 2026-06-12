use std::path::Path;
use std::process::Output;

use crate::errors::{ErrorCode, ErrorInfo};

/// Spawn the system `git` binary (PRD §3.6) with interactivity disabled
/// (PRD §3.1).
pub async fn git(dir: &Path, args: &[&str]) -> Result<Output, ErrorInfo> {
    tokio::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .await
        .map_err(|e| ErrorInfo::new(ErrorCode::SpawnFailed, format!("cannot spawn git: {e}")))
}

/// Like `git`, but a non-zero exit becomes a `git_failed` error carrying a
/// capped stderr snippet.
pub async fn git_ok(dir: &Path, args: &[&str], max_bytes: usize) -> Result<Output, ErrorInfo> {
    let out = git(dir, args).await?;
    if out.status.success() {
        Ok(out)
    } else {
        Err(ErrorInfo::new(
            ErrorCode::GitFailed,
            format!("git {} failed", args.join(" ")),
        )
        .with_snippet(&out.stderr, max_bytes))
    }
}

/// Lazy repo validation (PRD §4.1): the path must contain `.git`
/// (a directory, or a file for worktrees/submodules).
pub fn check_is_repo(path: &Path) -> Result<(), ErrorInfo> {
    if path.join(".git").exists() {
        Ok(())
    } else {
        Err(ErrorInfo::new(
            ErrorCode::NotARepo,
            format!("{} is not a git repository", path.display()),
        ))
    }
}

pub async fn head_sha(dir: &Path, max_bytes: usize) -> Result<String, ErrorInfo> {
    let out = git_ok(dir, &["rev-parse", "HEAD"], max_bytes).await?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum TreeState {
    Clean,
    Dirty,
    Detached,
    Conflicted,
}

impl TreeState {
    pub fn as_str(&self) -> &'static str {
        match self {
            TreeState::Clean => "clean",
            TreeState::Dirty => "dirty",
            TreeState::Detached => "detached",
            TreeState::Conflicted => "conflicted",
        }
    }
}

#[derive(Debug, PartialEq)]
pub struct PorcelainStatus {
    /// None when detached.
    pub branch: Option<String>,
    /// Short (7-char) commit id; "(initial)" on an unborn branch.
    pub head: String,
    pub upstream: Option<String>,
    pub ahead: Option<i64>,
    pub behind: Option<i64>,
    pub state: TreeState,
    /// First few changed-entry lines, for `dirty_tree` snippets.
    pub change_sample: String,
}

/// Parse `git status --porcelain=v2 --branch` output (PRD §5.1).
///
/// State precedence: conflicted (any `u` entry) > detached > dirty (any
/// changed/renamed/untracked entry — untracked counts as uncommitted work) >
/// clean.
pub fn parse_porcelain_v2(text: &str) -> PorcelainStatus {
    let mut branch = None;
    let mut head = String::new();
    let mut upstream = None;
    let mut ahead = None;
    let mut behind = None;
    let mut detached = false;
    let mut conflicted = false;
    let mut changed = false;
    let mut sample: Vec<&str> = Vec::new();

    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("# branch.oid ") {
            head = if rest == "(initial)" {
                rest.to_string()
            } else {
                rest.chars().take(7).collect()
            };
        } else if let Some(rest) = line.strip_prefix("# branch.head ") {
            if rest == "(detached)" {
                detached = true;
            } else {
                branch = Some(rest.to_string());
            }
        } else if let Some(rest) = line.strip_prefix("# branch.upstream ") {
            upstream = Some(rest.to_string());
        } else if let Some(rest) = line.strip_prefix("# branch.ab ") {
            for part in rest.split_whitespace() {
                if let Some(n) = part.strip_prefix('+') {
                    ahead = n.parse().ok();
                } else if let Some(n) = part.strip_prefix('-') {
                    behind = n.parse().ok();
                }
            }
        } else if line.starts_with("u ") {
            conflicted = true;
            if sample.len() < 5 {
                sample.push(line);
            }
        } else if line.starts_with("1 ") || line.starts_with("2 ") || line.starts_with("? ") {
            changed = true;
            if sample.len() < 5 {
                sample.push(line);
            }
        }
    }

    let state = if conflicted {
        TreeState::Conflicted
    } else if detached {
        TreeState::Detached
    } else if changed {
        TreeState::Dirty
    } else {
        TreeState::Clean
    };

    PorcelainStatus {
        branch,
        head,
        upstream,
        ahead,
        behind,
        state,
        change_sample: sample.join("\n"),
    }
}

pub async fn status(dir: &Path, max_bytes: usize) -> Result<PorcelainStatus, ErrorInfo> {
    let out = git_ok(dir, &["status", "--porcelain=v2", "--branch"], max_bytes).await?;
    Ok(parse_porcelain_v2(&String::from_utf8_lossy(&out.stdout)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_clean_with_upstream() {
        let s = parse_porcelain_v2(
            "# branch.oid 0123456789abcdef0123456789abcdef01234567\n\
             # branch.head main\n\
             # branch.upstream origin/main\n\
             # branch.ab +2 -1\n",
        );
        assert_eq!(s.branch.as_deref(), Some("main"));
        assert_eq!(s.head, "0123456");
        assert_eq!(s.ahead, Some(2));
        assert_eq!(s.behind, Some(1));
        assert_eq!(s.state, TreeState::Clean);
    }

    #[test]
    fn parses_dirty_no_upstream() {
        let s = parse_porcelain_v2(
            "# branch.oid 0123456789abcdef0123456789abcdef01234567\n\
             # branch.head main\n\
             1 .M N... 100644 100644 100644 abc abc src/main.rs\n\
             ? new.txt\n",
        );
        assert_eq!(s.ahead, None);
        assert_eq!(s.behind, None);
        assert_eq!(s.state, TreeState::Dirty);
        assert!(s.change_sample.contains("src/main.rs"));
    }

    #[test]
    fn untracked_only_counts_as_dirty() {
        let s = parse_porcelain_v2(
            "# branch.oid 0123456789abcdef0123456789abcdef01234567\n\
             # branch.head main\n\
             ? scratch.txt\n",
        );
        assert_eq!(s.state, TreeState::Dirty);
    }

    #[test]
    fn detached_head() {
        let s = parse_porcelain_v2(
            "# branch.oid 0123456789abcdef0123456789abcdef01234567\n\
             # branch.head (detached)\n",
        );
        assert_eq!(s.branch, None);
        assert_eq!(s.state, TreeState::Detached);
    }

    #[test]
    fn conflict_outranks_dirty_and_detached() {
        let s = parse_porcelain_v2(
            "# branch.oid 0123456789abcdef0123456789abcdef01234567\n\
             # branch.head (detached)\n\
             1 .M N... 100644 100644 100644 abc abc a.rs\n\
             u UU N... 100644 100644 100644 100644 abc abc abc b.rs\n",
        );
        assert_eq!(s.state, TreeState::Conflicted);
    }

    #[test]
    fn initial_commitless_branch() {
        let s = parse_porcelain_v2("# branch.oid (initial)\n# branch.head main\n");
        assert_eq!(s.head, "(initial)");
        assert_eq!(s.state, TreeState::Clean);
    }
}

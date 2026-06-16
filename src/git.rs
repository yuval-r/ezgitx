use std::path::Path;
use std::process::Output;

use serde::Serialize;

use crate::errors::{ErrorCode, ErrorInfo};

/// Spawn the system `git` binary (PRD §3.6) with interactivity disabled
/// (PRD §3.1). LC_ALL=C pins git's messages to English — pull classification
/// matches against stderr text, which is otherwise localized.
pub async fn git(dir: &Path, args: &[&str]) -> Result<Output, ErrorInfo> {
    tokio::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
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

/// A single commit in a range, for `brief`/`changed --since` deltas.
#[derive(Debug, PartialEq, Clone, Serialize)]
pub struct CommitSummary {
    /// Full 40-char commit hash.
    pub sha: String,
    /// First line of the commit message.
    pub subject: String,
}

/// Listing a commit range: the full count plus a capped, newest-first sample.
#[derive(Debug, PartialEq)]
pub struct CommitRange {
    /// Total commits in the range (uncapped).
    pub total: usize,
    /// Up to `max_count` newest commits, also bounded by accumulated subject bytes.
    pub commits: Vec<CommitSummary>,
    /// `true` when the sample dropped commits the range actually contains.
    pub truncated: bool,
}

/// List commits in `range` (e.g. "A..HEAD"), newest-first, capped to `max_count`
/// entries and `max_bytes` of accumulated subject bytes. Offline; no network.
///
/// `-z` NUL-separates records so multi-line/binary subjects can't break parsing;
/// `%x1f` (US) splits the full sha from the subject. Locale-stable under LC_ALL=C.
pub async fn commit_range(
    dir: &Path,
    range: &str,
    max_count: usize,
    max_bytes: usize,
) -> Result<CommitRange, ErrorInfo> {
    let out = git_ok(
        dir,
        &["log", "-z", "--pretty=format:%H%x1f%s", range],
        max_bytes,
    )
    .await?;
    Ok(parse_commit_log(&out.stdout, max_count, max_bytes))
}

/// Parse `git log -z --pretty=format:%H%x1f%s` output. Free function so the
/// capping/framing logic is unit-testable without spawning git.
pub fn parse_commit_log(stdout: &[u8], max_count: usize, max_bytes: usize) -> CommitRange {
    let mut total = 0usize;
    let mut commits = Vec::new();
    let mut subject_bytes = 0usize;
    let mut stop_including = false;

    for record in stdout.split(|&b| b == 0) {
        if record.is_empty() {
            continue; // guards the trailing record git emits after the last commit
        }
        total += 1;
        if stop_including || commits.len() >= max_count {
            stop_including = true;
            continue;
        }
        let text = String::from_utf8_lossy(record);
        let (sha, subject) = match text.split_once('\u{1f}') {
            Some((sha, subject)) => (sha.to_string(), subject.to_string()),
            None => (text.into_owned(), String::new()),
        };
        // Always keep at least one commit; otherwise stop once the next subject
        // would push past the byte cap (still counting it toward `total`).
        if !commits.is_empty() && subject_bytes + subject.len() > max_bytes {
            stop_including = true;
            continue;
        }
        subject_bytes += subject.len();
        commits.push(CommitSummary { sha, subject });
    }

    let truncated = total > commits.len();
    CommitRange {
        total,
        commits,
        truncated,
    }
}

/// Whether `rev` resolves to a commit in `dir`. `false` (not an error) when the
/// rev is syntactically fine but unreachable (rewritten history / gc). Uses the
/// raw `git` helper so a clean exit-1 isn't turned into a `git_failed`.
pub async fn rev_exists(dir: &Path, rev: &str) -> Result<bool, ErrorInfo> {
    let spec = format!("{rev}^{{commit}}");
    let out = git(dir, &["rev-parse", "--verify", "--quiet", &spec]).await?;
    Ok(out.status.success())
}

/// Resolve `rev` to its short sha, or `None` if it doesn't name a commit in
/// `dir` (absent ref / unreachable). `Err` only on spawn failure. Combines the
/// existence check with the display sha; uses the raw `git` helper so a clean
/// exit-1 isn't turned into a `git_failed`.
pub async fn resolve_commit(dir: &Path, rev: &str) -> Result<Option<String>, ErrorInfo> {
    let spec = format!("{rev}^{{commit}}");
    let out = git(dir, &["rev-parse", "--verify", "--quiet", "--short", &spec]).await?;
    if out.status.success() {
        Ok(Some(
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
        ))
    } else {
        Ok(None)
    }
}

/// A single changed path in a range, for `changed --since`.
#[derive(Debug, PartialEq, Clone, Serialize)]
pub struct ChangedFile {
    /// Single-letter change code: `A`/`M`/`D`/`R`/`C`/`T`.
    pub status: String,
    pub path: String,
}

/// Listing changed files in a range: the full count plus a capped sample.
#[derive(Debug, PartialEq)]
pub struct ChangedFiles {
    pub total: usize,
    pub files: Vec<ChangedFile>,
    pub truncated: bool,
}

/// Changed files in `range` (e.g. "A..HEAD"), capped to `max_count` entries and
/// `max_bytes` of accumulated path bytes. Offline; no network.
pub async fn changed_files(
    dir: &Path,
    range: &str,
    max_count: usize,
    max_bytes: usize,
) -> Result<ChangedFiles, ErrorInfo> {
    let out = git_ok(dir, &["diff", "--name-status", "-z", range], max_bytes).await?;
    Ok(parse_name_status(&out.stdout, max_count, max_bytes))
}

/// Parse `git diff --name-status -z` output. Free function so the framing/capping
/// logic is unit-testable without spawning git.
///
/// `-z` yields a flat NUL-separated token stream: `<status>\0<path>` per change,
/// or `<status>\0<old>\0<new>` for renames/copies (status starts `R`/`C`) — we
/// report the new path. Status is normalized to its first char.
pub fn parse_name_status(stdout: &[u8], max_count: usize, max_bytes: usize) -> ChangedFiles {
    let mut tokens = stdout.split(|&b| b == 0).filter(|t| !t.is_empty());
    let mut total = 0usize;
    let mut files = Vec::new();
    let mut path_bytes = 0usize;
    let mut stop = false;

    while let Some(status_tok) = tokens.next() {
        let code = String::from_utf8_lossy(status_tok)
            .chars()
            .next()
            .unwrap_or('?');
        // Rename/copy carry two paths (old, new); report the new one.
        let path_tok = if matches!(code, 'R' | 'C') {
            tokens.next(); // old path
            tokens.next() // new path
        } else {
            tokens.next() // path
        };
        let Some(path_tok) = path_tok else { break }; // malformed tail
        total += 1;
        if stop || files.len() >= max_count {
            stop = true;
            continue;
        }
        let path = String::from_utf8_lossy(path_tok).into_owned();
        if !files.is_empty() && path_bytes + path.len() > max_bytes {
            stop = true;
            continue;
        }
        path_bytes += path.len();
        files.push(ChangedFile {
            status: code.to_string(),
            path,
        });
    }

    let truncated = total > files.len();
    ChangedFiles {
        total,
        files,
        truncated,
    }
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

    /// Mimic `git log -z --pretty=format:%H%x1f%s`: `<sha>\x1f<subject>` records
    /// each terminated by NUL.
    fn log_bytes(records: &[(&str, &str)]) -> Vec<u8> {
        let mut out = Vec::new();
        for (sha, subject) in records {
            out.extend_from_slice(sha.as_bytes());
            out.push(0x1f);
            out.extend_from_slice(subject.as_bytes());
            out.push(0);
        }
        out
    }

    #[test]
    fn parse_commit_log_empty() {
        let r = parse_commit_log(b"", 20, 2048);
        assert_eq!(r.total, 0);
        assert!(r.commits.is_empty());
        assert!(!r.truncated);
    }

    #[test]
    fn parse_commit_log_orders_and_splits() {
        let bytes = log_bytes(&[("sha1", "first subject"), ("sha2", "second")]);
        let r = parse_commit_log(&bytes, 20, 2048);
        assert_eq!(r.total, 2);
        assert_eq!(r.commits.len(), 2);
        assert_eq!(r.commits[0].sha, "sha1");
        assert_eq!(r.commits[0].subject, "first subject");
        assert_eq!(r.commits[1].subject, "second");
        assert!(!r.truncated);
    }

    #[test]
    fn parse_commit_log_caps_by_count() {
        let bytes = log_bytes(&[("a", "1"), ("b", "2"), ("c", "3")]);
        let r = parse_commit_log(&bytes, 2, 2048);
        assert_eq!(r.total, 3);
        assert_eq!(r.commits.len(), 2);
        assert!(r.truncated);
    }

    #[test]
    fn parse_commit_log_caps_by_bytes() {
        let bytes = log_bytes(&[("a", "xxxx"), ("b", "yyyy")]);
        let r = parse_commit_log(&bytes, 20, 4);
        assert_eq!(r.total, 2);
        assert_eq!(r.commits.len(), 1); // first fits (4 bytes); second would exceed
        assert!(r.truncated);
    }

    #[test]
    fn parse_commit_log_keeps_at_least_one_over_byte_cap() {
        let bytes = log_bytes(&[("a", "this subject is well over the tiny cap")]);
        let r = parse_commit_log(&bytes, 20, 4);
        assert_eq!(r.total, 1);
        assert_eq!(r.commits.len(), 1);
        assert!(!r.truncated);
    }

    #[test]
    fn parse_commit_log_lossy_utf8_and_missing_separator() {
        // Invalid UTF-8, no US separator: whole record becomes the sha (lossy),
        // subject empty — never panics.
        let bytes = vec![0xff, 0xfe, 0x00];
        let r = parse_commit_log(&bytes, 20, 2048);
        assert_eq!(r.total, 1);
        assert_eq!(r.commits[0].subject, "");
        assert!(r.commits[0].sha.contains('\u{fffd}'));
    }

    /// Mimic `git diff --name-status -z`: `<status>\0<path>\0` tokens.
    fn name_status_bytes(entries: &[(&str, &str)]) -> Vec<u8> {
        let mut out = Vec::new();
        for (status, path) in entries {
            out.extend_from_slice(status.as_bytes());
            out.push(0);
            out.extend_from_slice(path.as_bytes());
            out.push(0);
        }
        out
    }

    #[test]
    fn parse_name_status_empty() {
        let r = parse_name_status(b"", 50, 2048);
        assert_eq!(r.total, 0);
        assert!(r.files.is_empty());
        assert!(!r.truncated);
    }

    #[test]
    fn parse_name_status_basic_codes() {
        let bytes = name_status_bytes(&[("M", "src/a.rs"), ("A", "src/b.rs"), ("D", "old.rs")]);
        let r = parse_name_status(&bytes, 50, 2048);
        assert_eq!(r.total, 3);
        assert_eq!(r.files.len(), 3);
        assert_eq!(r.files[0].status, "M");
        assert_eq!(r.files[0].path, "src/a.rs");
        assert_eq!(r.files[1].status, "A");
        assert_eq!(r.files[2].status, "D");
        assert!(!r.truncated);
    }

    #[test]
    fn parse_name_status_rename_reports_new_path() {
        // Rename: status, old, new — report the new path.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"R100\0old/name.rs\0new/name.rs\0");
        // followed by a normal modify, to prove the stream stays aligned.
        bytes.extend_from_slice(b"M\0other.rs\0");
        let r = parse_name_status(&bytes, 50, 2048);
        assert_eq!(r.total, 2);
        assert_eq!(r.files[0].status, "R");
        assert_eq!(r.files[0].path, "new/name.rs");
        assert_eq!(r.files[1].status, "M");
        assert_eq!(r.files[1].path, "other.rs");
    }

    #[test]
    fn parse_name_status_caps_by_count() {
        let bytes = name_status_bytes(&[("M", "a"), ("M", "b"), ("M", "c")]);
        let r = parse_name_status(&bytes, 2, 2048);
        assert_eq!(r.total, 3);
        assert_eq!(r.files.len(), 2);
        assert!(r.truncated);
    }

    #[test]
    fn parse_name_status_caps_by_bytes() {
        let bytes = name_status_bytes(&[("M", "aaaa"), ("M", "bbbb")]);
        let r = parse_name_status(&bytes, 50, 4);
        assert_eq!(r.total, 2);
        assert_eq!(r.files.len(), 1); // first fits (4 bytes); second would exceed
        assert!(r.truncated);
    }

    #[test]
    fn parse_name_status_lossy_path() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"A\0");
        bytes.extend_from_slice(&[0xff, 0xfe]); // invalid UTF-8 path
        bytes.push(0);
        let r = parse_name_status(&bytes, 50, 2048);
        assert_eq!(r.total, 1);
        assert_eq!(r.files[0].status, "A");
        assert!(r.files[0].path.contains('\u{fffd}'));
    }
}

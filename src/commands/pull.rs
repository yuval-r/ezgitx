use std::path::PathBuf;

use serde::Serialize;

use crate::errors::{ErrorCode, ErrorInfo, aggregate_exit};
use crate::exec::run_parallel;
use crate::git::{self, TreeState};
use crate::lock;
use crate::output::Emitter;
use crate::workspace::{Repo, Workspace};

#[derive(Serialize)]
struct PullLine {
    repo: String,
    status: &'static str,
    commits_pulled: u64,
    head: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<ErrorInfo>,
}

const HEADERS: &[&str] = &["REPO", "STATUS", "PULLED", "HEAD"];

/// PRD §5.2: fetch always runs (so `behind` stays accurate); dirty, diverged,
/// and detached are per-repo failures that never stop other repos; merges are
/// ff-only and never create merge commits.
async fn pull_one(root: PathBuf, repo: Repo, wait: Option<u64>, max_bytes: usize) -> PullLine {
    let fail = |status: &'static str, head: String, error: Option<ErrorInfo>| PullLine {
        repo: repo.name.clone(),
        status,
        commits_pulled: 0,
        head,
        error,
    };

    if let Err(e) = git::check_is_repo(&repo.path) {
        return fail("error", String::new(), Some(e));
    }

    let lock_path = lock::repo_lock_path(&root, &repo.name);
    let _guard = match lock::acquire(&lock_path, "pull", wait).await {
        Ok(g) => g,
        Err(e) => return fail("error", String::new(), Some(e)),
    };

    let pre = match git::head_sha(&repo.path, max_bytes).await {
        Ok(sha) => sha,
        Err(e) => return fail("error", String::new(), Some(e)),
    };
    let short = |sha: &str| sha.chars().take(7).collect::<String>();

    if let Err(e) = git::git_ok(&repo.path, &["fetch"], max_bytes).await {
        return fail("error", short(&pre), Some(e));
    }

    let status = match git::status(&repo.path, max_bytes).await {
        Ok(s) => s,
        Err(e) => return fail("error", short(&pre), Some(e)),
    };
    match status.state {
        TreeState::Detached => {
            return fail(
                "detached",
                short(&pre),
                Some(ErrorInfo::new(ErrorCode::Detached, "HEAD is detached")),
            );
        }
        TreeState::Dirty | TreeState::Conflicted => {
            return fail(
                "skipped_dirty",
                short(&pre),
                Some(
                    ErrorInfo::new(
                        ErrorCode::DirtyTree,
                        "uncommitted changes block ff-only pull",
                    )
                    .with_snippet(status.change_sample.as_bytes(), max_bytes),
                ),
            );
        }
        TreeState::Clean => {}
    }

    let merge = match git::git(&repo.path, &["merge", "--ff-only", "@{upstream}"]).await {
        Ok(out) => out,
        Err(e) => return fail("error", short(&pre), Some(e)),
    };
    if !merge.status.success() {
        let stderr = String::from_utf8_lossy(&merge.stderr);
        let (status, error) = if stderr.contains("fast-forward") || stderr.contains("divergent") {
            (
                "diverged",
                ErrorInfo::new(ErrorCode::Diverged, "branch and upstream have diverged"),
            )
        } else {
            (
                "error",
                ErrorInfo::new(ErrorCode::GitFailed, "git merge --ff-only failed"),
            )
        };
        return fail(
            status,
            short(&pre),
            Some(error.with_snippet(&merge.stderr, max_bytes)),
        );
    }

    let post = match git::head_sha(&repo.path, max_bytes).await {
        Ok(sha) => sha,
        Err(e) => return fail("error", short(&pre), Some(e)),
    };
    if post == pre {
        return PullLine {
            repo: repo.name.clone(),
            status: "up_to_date",
            commits_pulled: 0,
            head: short(&post),
            error: None,
        };
    }
    let range = format!("{pre}..{post}");
    let commits_pulled =
        match git::git_ok(&repo.path, &["rev-list", "--count", &range], max_bytes).await {
            Ok(out) => String::from_utf8_lossy(&out.stdout)
                .trim()
                .parse()
                .unwrap_or(0),
            Err(_) => 0,
        };
    PullLine {
        repo: repo.name.clone(),
        status: "updated",
        commits_pulled,
        head: short(&post),
        error: None,
    }
}

pub async fn run(
    ws: &Workspace,
    repos: Vec<Repo>,
    wait: Option<u64>,
    jobs: usize,
    max_bytes: usize,
    human: bool,
) -> i32 {
    // Mutating ops check the global lock before proceeding (PRD §7).
    if let Err(e) = lock::check_workspace_lock(&ws.root) {
        crate::errors::print_top_level(&e);
        return crate::errors::EXIT_LOCK;
    }

    let mut emitter = Emitter::new(human, HEADERS);
    let mut any_failure = false;
    let mut any_lock_held = false;

    let root = ws.root.clone();
    run_parallel(
        repos,
        jobs,
        |repo| pull_one(root.clone(), repo, wait, max_bytes),
        |line| {
            if line.status != "updated" && line.status != "up_to_date" {
                any_failure = true;
            }
            if line
                .error
                .as_ref()
                .is_some_and(|e| e.code == ErrorCode::LockHeld)
            {
                any_lock_held = true;
            }
            let row = vec![
                line.repo.clone(),
                line.status.to_string(),
                line.commits_pulled.to_string(),
                line.head.clone(),
            ];
            emitter.emit(&line, row);
        },
    )
    .await;

    emitter.finish();
    aggregate_exit(any_failure, any_lock_held)
}

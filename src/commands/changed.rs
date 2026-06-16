use serde::Serialize;

use crate::errors::{ErrorInfo, aggregate_exit};
use crate::exec::run_parallel;
use crate::git;
use crate::output::Emitter;
use crate::state;
use crate::workspace::{Repo, Workspace};

const MAX_COMMITS: usize = 20;
const MAX_FILES: usize = 50;

#[derive(Serialize)]
struct ChangedLine {
    repo: String,
    /// Echoes the requested `--since` value ("last-brief" or the literal ref).
    since_ref: String,
    /// Short sha of the resolved since-ref / of HEAD. Omitted when degraded.
    #[serde(skip_serializing_if = "Option::is_none")]
    from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    to: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    new_commits: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    commits: Option<Vec<git::CommitSummary>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    files: Option<Vec<git::ChangedFile>>,
    /// True if either the commits or files list was capped.
    #[serde(skip_serializing_if = "Option::is_none")]
    truncated: Option<bool>,
    /// Set when no delta could be computed (still exit 0): "no_baseline",
    /// "baseline_unreachable", "ref_not_found", or "unborn".
    #[serde(skip_serializing_if = "Option::is_none")]
    delta_unavailable: Option<&'static str>,
}

#[derive(Serialize)]
struct ErrorLine {
    repo: String,
    error: ErrorInfo,
}

#[derive(Serialize)]
struct ChangedSummary {
    r#type: &'static str,
    repos: usize,
    with_changes: usize,
    failed: usize,
}

enum Outcome {
    Ok(Box<ChangedLine>),
    Err(String, ErrorInfo),
}

const HEADERS: &[&str] = &["REPO", "COMMITS", "FILES", "RANGE"];

pub async fn run(
    ws: &Workspace,
    repos: Vec<Repo>,
    since: String,
    jobs: usize,
    max_bytes: usize,
    human: bool,
) -> i32 {
    let mut emitter = Emitter::new(human, HEADERS);
    let mut any_failure = false;
    let mut repos_count = 0usize;
    let mut with_changes = 0usize;
    let mut failed = 0usize;

    let root = ws.root.clone();

    run_parallel(
        repos,
        jobs,
        |repo| {
            let root = root.clone();
            let since = since.clone();
            async move {
                if let Err(e) = git::check_is_repo(&repo.path) {
                    return Outcome::Err(repo.name, e);
                }
                let mut line = ChangedLine {
                    repo: repo.name.clone(),
                    since_ref: since.clone(),
                    from: None,
                    to: None,
                    new_commits: None,
                    commits: None,
                    files: None,
                    truncated: None,
                    delta_unavailable: None,
                };

                // `to` side: HEAD. An unborn branch has nothing to diff.
                let to = match git::resolve_commit(&repo.path, "HEAD").await {
                    Ok(Some(sha)) => sha,
                    Ok(None) => {
                        line.delta_unavailable = Some("unborn");
                        return Outcome::Ok(Box::new(line));
                    }
                    Err(e) => return Outcome::Err(repo.name, e),
                };

                // `from` side: the last brief baseline, or the literal ref.
                let from_ref = if since == "last-brief" {
                    match state::read_brief(&root, &repo.name) {
                        Some(b) => b.head,
                        None => {
                            line.delta_unavailable = Some("no_baseline");
                            return Outcome::Ok(Box::new(line));
                        }
                    }
                } else {
                    since.clone()
                };
                let from = match git::resolve_commit(&repo.path, &from_ref).await {
                    Ok(Some(sha)) => sha,
                    Ok(None) => {
                        // Multi-repo friendly: a ref absent here isn't a failure.
                        line.delta_unavailable = Some(if since == "last-brief" {
                            "baseline_unreachable"
                        } else {
                            "ref_not_found"
                        });
                        return Outcome::Ok(Box::new(line));
                    }
                    Err(e) => return Outcome::Err(repo.name, e),
                };

                // Delta over the *resolved* endpoints, so the range always matches
                // the reported from/to even if HEAD or the ref moves mid-run. Both
                // endpoints already resolved, so an error here is a real git failure
                // (not a missing ref) and propagates rather than degrading.
                let range = format!("{from}..{to}");
                let commits =
                    match git::commit_range(&repo.path, &range, MAX_COMMITS, max_bytes).await {
                        Ok(c) => c,
                        Err(e) => return Outcome::Err(repo.name, e),
                    };
                let files = match git::changed_files(&repo.path, &range, MAX_FILES, max_bytes).await
                {
                    Ok(f) => f,
                    Err(e) => return Outcome::Err(repo.name, e),
                };
                line.from = Some(from);
                line.to = Some(to);
                line.new_commits = Some(commits.total);
                line.commits = Some(commits.commits);
                line.truncated = Some(commits.truncated || files.truncated);
                line.files = Some(files.files);
                Outcome::Ok(Box::new(line))
            }
        },
        |outcome| match outcome {
            Outcome::Ok(line) => {
                repos_count += 1;
                let has_changes = line.new_commits.unwrap_or(0) > 0
                    || line.files.as_ref().is_some_and(|f| !f.is_empty());
                if has_changes {
                    with_changes += 1;
                }
                let range_cell = match line.delta_unavailable {
                    Some(reason) => reason.to_string(),
                    None => format!(
                        "{}..{}",
                        line.from.as_deref().unwrap_or("-"),
                        line.to.as_deref().unwrap_or("-")
                    ),
                };
                let row = vec![
                    line.repo.clone(),
                    line.new_commits.map_or("-".to_string(), |n| n.to_string()),
                    line.files
                        .as_ref()
                        .map_or("-".to_string(), |f| f.len().to_string()),
                    range_cell,
                ];
                emitter.emit(&line, row);
            }
            Outcome::Err(repo, error) => {
                any_failure = true;
                failed += 1;
                repos_count += 1;
                let row = vec![
                    repo.clone(),
                    "-".into(),
                    "-".into(),
                    format!("error: {}", error.code.as_str()),
                ];
                emitter.emit(&ErrorLine { repo, error }, row);
            }
        },
    )
    .await;

    let summary = ChangedSummary {
        r#type: "summary",
        repos: repos_count,
        with_changes,
        failed,
    };
    let human_text = format!("repos {repos_count} with-changes {with_changes} failed {failed}");
    emitter.emit_summary(&summary, human_text);
    emitter.finish();
    aggregate_exit(any_failure, false)
}

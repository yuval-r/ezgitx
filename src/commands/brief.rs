use std::collections::BTreeSet;
use std::sync::Arc;

use serde::Serialize;

use crate::errors::{ErrorInfo, aggregate_exit};
use crate::exec::run_parallel;
use crate::git::{self, TreeState};
use crate::output::Emitter;
use crate::state;
use crate::workspace::{Repo, Workspace};

/// A repo paired with the names of its transitive upstreams, if any.
type PreparedRepo = (Repo, Option<BTreeSet<String>>);

/// Cap on commits listed per repo. The full count is always reported in
/// `new_commits`; `truncated` flags when the sample dropped some.
const MAX_COMMITS: usize = 20;

#[derive(Serialize)]
struct BriefLine {
    repo: String,
    path: String,
    branch: Option<String>,
    /// Short (7-char) head, for display parity with `status`.
    head: String,
    state: &'static str,
    ahead: Option<i64>,
    behind: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stale_deps: Option<Vec<String>>,
    /// Delta vs the last brief. All omitted on the first run (no baseline yet).
    #[serde(skip_serializing_if = "Option::is_none")]
    new_commits: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    commits: Option<Vec<git::CommitSummary>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    truncated: Option<bool>,
    /// Set (e.g. "baseline_unreachable") when a delta couldn't be computed but
    /// the snapshot is still valid — not an error; the repo stays exit 0.
    #[serde(skip_serializing_if = "Option::is_none")]
    delta_unavailable: Option<&'static str>,
}

#[derive(Serialize)]
struct ErrorLine {
    repo: String,
    error: ErrorInfo,
}

#[derive(Serialize)]
struct BriefSummary {
    r#type: &'static str,
    repos: usize,
    with_new_commits: usize,
    failed: usize,
}

/// `Ok(line, tree_state, to_record)`: `to_record` is the full HEAD to persist as
/// the new baseline, or `None` for an unborn branch (nothing to record).
enum Outcome {
    Ok(Box<BriefLine>, TreeState, Option<String>),
    Err(String, ErrorInfo),
}

const HEADERS: &[&str] = &[
    "REPO",
    "BRANCH",
    "HEAD",
    "STATE",
    "AHEAD",
    "BEHIND",
    "STALE_DEPS",
    "NEW",
];

/// Render an optional numeric cell for human mode: the value, or "-" when absent.
fn opt<T: std::fmt::Display>(v: Option<T>) -> String {
    v.map_or("-".to_string(), |n| n.to_string())
}

pub async fn run(
    ws: &Workspace,
    repos: Vec<Repo>,
    dirty_only: bool,
    jobs: usize,
    max_bytes: usize,
    no_record: bool,
    human: bool,
) -> i32 {
    let mut emitter = Emitter::new(human, HEADERS);
    let mut any_failure = false;
    let mut repos_count = 0usize;
    let mut with_new_commits = 0usize;
    let mut failed = 0usize;

    // Resolve each repo's transitive upstream names up front (cheap, no git).
    let root = ws.root.clone();
    let root_cb = ws.root.clone();
    let prepared: Vec<PreparedRepo> = repos
        .into_iter()
        .map(|repo| {
            let upstreams = if repo.depends_on.is_empty() {
                None
            } else {
                Some(crate::graph::transitive_upstreams(ws, &repo.name))
            };
            (repo, upstreams)
        })
        .collect();

    // Probe every distinct upstream's HEAD once, up front (same approach as
    // `status`): probing inside each task would multiply concurrency and
    // re-probe upstreams shared by multiple repos.
    let all_upstreams: BTreeSet<String> = prepared
        .iter()
        .filter_map(|(_, upstreams)| upstreams.as_ref())
        .flatten()
        .cloned()
        .collect();
    let heads =
        Arc::new(state::current_heads(state::with_paths(ws, all_upstreams), jobs, max_bytes).await);

    run_parallel(
        prepared,
        jobs,
        |(repo, upstreams)| {
            let root = root.clone();
            let heads = heads.clone();
            async move {
                if let Err(e) = git::check_is_repo(&repo.path) {
                    return Outcome::Err(repo.name, e);
                }
                let stale_deps = upstreams.map(|names| {
                    let record = state::read(&root, &repo.name);
                    state::deps_drift(&names, record.as_ref(), &heads)
                });
                let status = match git::status(&repo.path, max_bytes).await {
                    Ok(s) => s,
                    Err(e) => return Outcome::Err(repo.name, e),
                };

                let mut line = BriefLine {
                    repo: repo.name.clone(),
                    path: repo.path.display().to_string(),
                    branch: status.branch,
                    head: status.head.clone(),
                    state: status.state.as_str(),
                    ahead: status.ahead,
                    behind: status.behind,
                    stale_deps,
                    new_commits: None,
                    commits: None,
                    truncated: None,
                    delta_unavailable: None,
                };

                // Unborn branch (no commit yet): snapshot only, nothing to baseline.
                if status.head == "(initial)" {
                    return Outcome::Ok(Box::new(line), status.state, None);
                }

                // Full sha drives the baseline + range; the serialized `head` is short.
                let full_head = match git::head_sha(&repo.path, max_bytes).await {
                    Ok(h) => h,
                    Err(e) => return Outcome::Err(repo.name, e),
                };

                // 5-step delta policy (shared verbatim with `changed --since`).
                match state::read_brief(&root, &repo.name) {
                    None => {} // first run: omit delta, still record baseline below
                    Some(baseline) if baseline.head == full_head => {
                        line.new_commits = Some(0);
                        line.commits = Some(Vec::new());
                        line.truncated = Some(false);
                    }
                    Some(baseline) => {
                        // Degrade (don't fail) if the baseline was rewritten/gc'd or
                        // any delta-engine call errors.
                        let reachable = git::rev_exists(&repo.path, &baseline.head)
                            .await
                            .unwrap_or(false);
                        let range = format!("{}..HEAD", baseline.head);
                        let delta = if reachable {
                            git::commit_range(&repo.path, &range, MAX_COMMITS, max_bytes)
                                .await
                                .ok()
                        } else {
                            None
                        };
                        match delta {
                            Some(cr) => {
                                line.new_commits = Some(cr.total);
                                line.commits = Some(cr.commits);
                                line.truncated = Some(cr.truncated);
                            }
                            None => line.delta_unavailable = Some("baseline_unreachable"),
                        }
                    }
                }

                Outcome::Ok(Box::new(line), status.state, Some(full_head))
            }
        },
        |outcome| match outcome {
            Outcome::Ok(line, tree_state, to_record) => {
                // Only repos we actually display advance their baseline. A
                // `--dirty` brief must not consume the delta of a clean repo it
                // never showed — that repo's commits would vanish from the
                // stream. This mirrors how --repo/--group only touch the repos
                // they select: you only consume a delta you were shown.
                if dirty_only && !matches!(tree_state, TreeState::Dirty | TreeState::Conflicted) {
                    return;
                }
                if !no_record {
                    if let Some(full) = to_record {
                        state::write_brief(&root_cb, &line.repo, full);
                    }
                }
                repos_count += 1;
                if line.new_commits.unwrap_or(0) > 0 {
                    with_new_commits += 1;
                }
                let row = vec![
                    line.repo.clone(),
                    line.branch.clone().unwrap_or_else(|| "-".to_string()),
                    line.head.clone(),
                    line.state.to_string(),
                    opt(line.ahead),
                    opt(line.behind),
                    line.stale_deps
                        .clone()
                        .map_or("-".to_string(), |d| d.join(",")),
                    opt(line.new_commits),
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
                    "-".into(),
                    "-".into(),
                    "-".into(),
                    "-".into(),
                ];
                emitter.emit(&ErrorLine { repo, error }, row);
            }
        },
    )
    .await;

    let summary = BriefSummary {
        r#type: "summary",
        repos: repos_count,
        with_new_commits,
        failed,
    };
    let human = format!("repos {repos_count} with-new {with_new_commits} failed {failed}");
    emitter.emit_summary(&summary, human);
    emitter.finish();
    aggregate_exit(any_failure, false)
}

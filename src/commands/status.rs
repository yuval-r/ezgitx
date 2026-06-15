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

#[derive(Serialize)]
struct StatusLine {
    repo: String,
    path: String,
    branch: Option<String>,
    head: String,
    state: &'static str,
    ahead: Option<i64>,
    behind: Option<i64>,
    /// V2 (PRD §9.3): present only for repos that declare dependencies.
    #[serde(skip_serializing_if = "Option::is_none")]
    stale_deps: Option<Vec<String>>,
}

#[derive(Serialize)]
struct ErrorLine {
    repo: String,
    error: ErrorInfo,
}

enum Outcome {
    Ok(Box<StatusLine>, TreeState),
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
];

pub async fn run(
    ws: &Workspace,
    repos: Vec<Repo>,
    dirty_only: bool,
    jobs: usize,
    max_bytes: usize,
    human: bool,
) -> i32 {
    let mut emitter = Emitter::new(human, HEADERS);
    let mut any_failure = false;

    // Resolve each repo's transitive upstream names up front (cheap, no git).
    let root = ws.root.clone();
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

    // Probe every distinct upstream's HEAD once, up front (honoring the global
    // `jobs` cap), then share the snapshot across the per-repo tasks. Probing
    // inside each task would multiply concurrency (jobs x jobs) and re-probe
    // upstreams shared by multiple repos.
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
                match git::status(&repo.path, max_bytes).await {
                    Ok(s) => Outcome::Ok(
                        Box::new(StatusLine {
                            repo: repo.name,
                            path: repo.path.display().to_string(),
                            branch: s.branch,
                            head: s.head,
                            state: s.state.as_str(),
                            ahead: s.ahead,
                            behind: s.behind,
                            stale_deps,
                        }),
                        s.state,
                    ),
                    Err(e) => Outcome::Err(repo.name, e),
                }
            }
        },
        |outcome| match outcome {
            Outcome::Ok(line, tree_state) => {
                if dirty_only && !matches!(tree_state, TreeState::Dirty | TreeState::Conflicted) {
                    return;
                }
                let opt = |v: Option<i64>| v.map_or("-".to_string(), |n| n.to_string());
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
                ];
                emitter.emit(&line, row);
            }
            Outcome::Err(repo, error) => {
                any_failure = true;
                let row = vec![
                    repo.clone(),
                    "-".into(),
                    "-".into(),
                    format!("error: {}", error.code.as_str()),
                    "-".into(),
                    "-".into(),
                    "-".into(),
                ];
                emitter.emit(&ErrorLine { repo, error }, row);
            }
        },
    )
    .await;

    emitter.finish();
    aggregate_exit(any_failure, false)
}

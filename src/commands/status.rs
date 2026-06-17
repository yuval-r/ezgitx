use std::collections::BTreeSet;

use serde::Serialize;

use crate::errors::{ErrorInfo, aggregate_exit};
use crate::exec::run_parallel;
use crate::git::{self, TreeState};
use crate::output::Emitter;
use crate::state;
use crate::workspace::{Repo, Workspace};

/// A repo paired with its precomputed build freshness and stale-deps list.
type PreparedRepo = (Repo, &'static str, Option<Vec<String>>);

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
    /// Build freshness vs. the last recorded green build (`state::is_stale`):
    /// `"stale"` when there is no record, HEAD has moved, or an upstream drifted.
    build: &'static str,
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
    "BUILD",
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
    let with_upstreams: Vec<(Repo, Option<BTreeSet<String>>)> = repos
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

    // Probe HEADs once for every repo we report on AND every distinct upstream
    // (honoring the global `jobs` cap). The repo's own HEAD is needed to judge
    // build freshness; upstream HEADs feed the stale-deps manifest check.
    let mut probe: BTreeSet<String> = BTreeSet::new();
    for (repo, upstreams) in &with_upstreams {
        probe.insert(repo.name.clone());
        if let Some(names) = upstreams {
            probe.extend(names.iter().cloned());
        }
    }
    let heads = state::current_heads(state::with_paths(ws, probe), jobs, max_bytes).await;

    // Compute freshness before spawning: is_stale/deps_drift need `ws` and the
    // shared heads snapshot, and run_parallel tasks must be 'static (they cannot
    // borrow `ws`). Reads each repo's state record once.
    let prepared: Vec<PreparedRepo> = with_upstreams
        .into_iter()
        .map(|(repo, upstreams)| {
            let record = state::read(&root, &repo.name);
            let build = if state::is_stale(ws, &repo.name, record.as_ref(), &heads) {
                "stale"
            } else {
                "fresh"
            };
            let stale_deps =
                upstreams.map(|names| state::deps_drift(&names, record.as_ref(), &heads));
            (repo, build, stale_deps)
        })
        .collect();

    run_parallel(
        prepared,
        jobs,
        |(repo, build, stale_deps)| async move {
            if let Err(e) = git::check_is_repo(&repo.path) {
                return Outcome::Err(repo.name, e);
            }
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
                        build,
                    }),
                    s.state,
                ),
                Err(e) => Outcome::Err(repo.name, e),
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
                    line.build.to_string(),
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

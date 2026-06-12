use serde::Serialize;

use crate::errors::{ErrorInfo, aggregate_exit};
use crate::exec::run_parallel;
use crate::git::{self, TreeState};
use crate::output::Emitter;
use crate::state;
use crate::workspace::{Repo, Workspace};

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

    // stale_deps needs &Workspace (not Send into tasks cheaply); compute it
    // up front per repo — it is a few rev-parse calls at most.
    let mut prepared: Vec<(Repo, Option<Vec<String>>)> = Vec::new();
    for repo in repos {
        let stale = if repo.depends_on.is_empty() {
            None
        } else {
            Some(state::stale_upstreams(ws, &repo.name, max_bytes).await)
        };
        prepared.push((repo, stale));
    }

    run_parallel(
        prepared,
        jobs,
        |(repo, stale_deps)| async move {
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
                ];
                emitter.emit(&line, row);
            }
            Outcome::Err(repo, error) => {
                any_failure = true;
                let code = serde_json::to_value(error.code).unwrap();
                let row = vec![
                    repo.clone(),
                    "-".into(),
                    "-".into(),
                    format!("error: {}", code.as_str().unwrap()),
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

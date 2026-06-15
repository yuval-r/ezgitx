use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use serde::Serialize;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::errors::{ErrorCode, ErrorInfo};
use crate::git;
use crate::output::{Emitter, cap_tail};
use crate::state;
use crate::workspace::{Repo, Workspace};

/// Run `make_task(item)` for every item with at most `jobs` in flight,
/// delivering results in completion order so no repo blocks another's output
/// (PRD §3.5).
pub async fn run_parallel<I, T, F, Fut>(
    items: Vec<I>,
    jobs: usize,
    make_task: F,
    mut on_result: impl FnMut(T),
) where
    I: Send + 'static,
    T: Send + 'static,
    F: Fn(I) -> Fut,
    Fut: Future<Output = T> + Send + 'static,
{
    let semaphore = Arc::new(Semaphore::new(jobs.max(1)));
    let mut set = JoinSet::new();
    for item in items {
        let permit = semaphore.clone();
        let task = make_task(item);
        set.spawn(async move {
            let _permit = permit.acquire_owned().await.expect("semaphore closed");
            task.await
        });
    }
    while let Some(result) = set.join_next().await {
        on_result(result.expect("repo task panicked"));
    }
}

/// The `run`-shaped result line (PRD §5.3), also reused by `check-impact
/// --check` and `upstream_failed` placeholders — one schema, no variants.
#[derive(Serialize, Debug)]
pub struct RunLine {
    pub repo: String,
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
    pub stdout_tail: String,
    pub stderr_tail: String,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorInfo>,
}

impl RunLine {
    pub fn failed(&self) -> bool {
        self.exit_code != Some(0)
    }

    pub fn from_error(repo: &str, error: ErrorInfo) -> Self {
        RunLine {
            repo: repo.to_string(),
            exit_code: None,
            duration_ms: 0,
            stdout_tail: String::new(),
            stderr_tail: String::new(),
            truncated: false,
            error: Some(error),
        }
    }

    pub fn human_row(&self) -> Vec<String> {
        let status = match (&self.error, self.exit_code) {
            (Some(e), _) => e.code.as_str().to_string(),
            (None, Some(0)) => "ok".to_string(),
            (None, Some(code)) => format!("exit {code}"),
            (None, None) => "?".to_string(),
        };
        vec![self.repo.clone(), status, format!("{}ms", self.duration_ms)]
    }
}

pub const RUN_HEADERS: &[&str] = &["REPO", "RESULT", "DURATION"];

/// Spawn `cmd` via the user's shell in `dir` (PRD §5.3), tails byte-capped.
pub async fn shell_in_repo(repo: &str, dir: &Path, cmd: &str, max_bytes: usize) -> RunLine {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
    let started = Instant::now();
    let output = tokio::process::Command::new(&shell)
        .arg("-c")
        .arg(cmd)
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .await;
    let duration_ms = started.elapsed().as_millis() as u64;
    match output {
        Ok(out) => {
            let (stdout_tail, out_capped) = cap_tail(&out.stdout, max_bytes);
            let (stderr_tail, err_capped) = cap_tail(&out.stderr, max_bytes);
            RunLine {
                repo: repo.to_string(),
                exit_code: out.status.code(),
                duration_ms,
                stdout_tail,
                stderr_tail,
                truncated: out_capped || err_capped,
                error: None,
            }
        }
        Err(e) => {
            let mut line = RunLine::from_error(
                repo,
                ErrorInfo::new(ErrorCode::SpawnFailed, format!("cannot spawn {shell}: {e}")),
            );
            line.duration_ms = duration_ms;
            line
        }
    }
}

/// Execute commands across topological waves (PRD §9.4/§9.5): parallel within
/// a wave, ordered across waves. When a repo fails, every downstream repo in
/// later waves emits `upstream_failed` instead of running. `command_for`
/// returns the command to run, or an ErrorInfo (e.g. `no_default_cmd`).
/// Successful repos get a freshness record when `record` is set.
#[allow(clippy::too_many_arguments)]
pub async fn execute_waves(
    ws: &Workspace,
    waves: Vec<Vec<String>>,
    command_for: impl Fn(&Repo) -> Result<String, ErrorInfo>,
    jobs: usize,
    max_bytes: usize,
    record: bool,
    heads: &BTreeMap<String, String>,
    emitter: &mut Emitter,
) -> (u64, u64) {
    let mut failed_repos: BTreeSet<String> = BTreeSet::new();
    let (mut passed, mut failed) = (0u64, 0u64);

    for wave in waves {
        let mut runnable: Vec<(Repo, String, BTreeMap<String, String>)> = Vec::new();
        for name in wave {
            let repo = ws.repos[&name].clone();
            // Upstreams may have failed through repos outside the executed
            // set (fresh intermediates), so the check must be transitive.
            let upstreams = crate::graph::transitive_upstreams(ws, &name);
            if let Some(bad) = upstreams.iter().find(|d| failed_repos.contains(*d)) {
                let line = RunLine::from_error(
                    &name,
                    ErrorInfo::new(
                        ErrorCode::UpstreamFailed,
                        format!("skipped: upstream {bad} failed"),
                    ),
                );
                emitter.emit(&line, line.human_row());
                failed_repos.insert(name);
                failed += 1;
                continue;
            }
            match git::check_is_repo(&repo.path).and_then(|()| command_for(&repo)) {
                Ok(cmd) => {
                    let deps: BTreeMap<String, String> = upstreams
                        .iter()
                        .filter_map(|u| heads.get(u).map(|h| (u.clone(), h.clone())))
                        .collect();
                    runnable.push((repo, cmd, deps));
                }
                Err(e) => {
                    let line = RunLine::from_error(&name, e);
                    emitter.emit(&line, line.human_row());
                    failed_repos.insert(name);
                    failed += 1;
                }
            }
        }

        let root = ws.root.clone();
        run_parallel(
            runnable,
            jobs,
            |(repo, cmd, deps)| {
                let root = root.clone();
                async move {
                    let line = shell_in_repo(&repo.name, &repo.path, &cmd, max_bytes).await;
                    if record && !line.failed() {
                        if let Ok(head) = git::head_sha(&repo.path, max_bytes).await {
                            state::record_success(&root, &repo.name, head, &cmd, deps);
                        }
                    }
                    line
                }
            },
            |line| {
                if line.failed() {
                    failed_repos.insert(line.repo.clone());
                    failed += 1;
                } else {
                    passed += 1;
                }
                emitter.emit(&line, line.human_row());
            },
        )
        .await;
    }
    (passed, failed)
}

#[derive(Serialize)]
pub struct RunSummary {
    pub r#type: &'static str,
    pub total: u64,
    pub passed: u64,
    pub failed: u64,
    pub duration_ms: u64,
}

impl RunSummary {
    pub fn new(passed: u64, failed: u64, duration_ms: u64) -> Self {
        RunSummary {
            r#type: "summary",
            total: passed + failed,
            passed,
            failed,
            duration_ms,
        }
    }

    pub fn human(&self) -> String {
        format!(
            "total {} passed {} failed {} in {}ms",
            self.total, self.passed, self.failed, self.duration_ms
        )
    }
}

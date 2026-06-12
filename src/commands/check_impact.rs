use std::collections::BTreeSet;
use std::path::Path;
use std::time::Instant;

use serde::Serialize;

use crate::errors::{EXIT_OK, EXIT_USAGE, ErrorCode, ErrorInfo, aggregate_exit};
use crate::exec::{self, RUN_HEADERS, RunSummary};
use crate::graph;
use crate::output::Emitter;
use crate::workspace::Workspace;

#[derive(Serialize)]
struct ImpactSummary {
    r#type: &'static str,
    changed: String,
    affected: usize,
}

/// `ezgitx check-impact` (PRD §9.5): list the transitive downstream closure;
/// `--check` additionally runs each affected repo's check_cmd (fallback
/// default_cmd) in topological waves.
pub async fn run(
    ws: &Workspace,
    cwd: &Path,
    repo_flag: Option<String>,
    check: bool,
    jobs: usize,
    max_bytes: usize,
    human: bool,
) -> i32 {
    let changed = match repo_flag {
        Some(name) => {
            if !ws.repos.contains_key(&name) {
                crate::errors::print_top_level(&ErrorInfo::new(
                    ErrorCode::ConfigInvalid,
                    format!("unknown repo {name:?}"),
                ));
                return EXIT_USAGE;
            }
            name
        }
        None => match ws.current_repo(cwd) {
            Some(repo) => repo.name.clone(),
            None => {
                crate::errors::print_top_level(&ErrorInfo::new(
                    ErrorCode::ConfigInvalid,
                    "not inside a member repo; pass --repo <name>",
                ));
                return EXIT_USAGE;
            }
        },
    };

    let affected = graph::downstream_closure(ws, &changed);

    let mut emitter = Emitter::new(human, &["REPO", "DEPTH", "VIA"]);
    for entry in &affected {
        let row = vec![
            entry.repo.clone(),
            entry.depth.to_string(),
            entry.via.join(" -> "),
        ];
        emitter.emit(entry, row);
    }
    let summary = ImpactSummary {
        r#type: "summary",
        changed: changed.clone(),
        affected: affected.len(),
    };
    emitter.emit_summary(
        &summary,
        format!("{} affected downstream of {}", affected.len(), changed),
    );
    emitter.finish();

    if !check {
        return EXIT_OK;
    }

    // --check: execute over the affected set only (the changed repo itself
    // is not re-validated), standard run-shaped lines, run exit semantics.
    let started = Instant::now();
    let set: BTreeSet<String> = affected.iter().map(|a| a.repo.clone()).collect();
    let waves = graph::topo_waves(ws, &set);

    let mut emitter = Emitter::new(human, RUN_HEADERS);
    let command_for = |repo: &crate::workspace::Repo| -> Result<String, ErrorInfo> {
        repo.check_cmd
            .clone()
            .or_else(|| repo.default_cmd.clone())
            .ok_or_else(|| {
                ErrorInfo::new(
                    ErrorCode::NoDefaultCmd,
                    format!("repo {:?} has neither check_cmd nor default_cmd", repo.name),
                )
            })
    };
    let (passed, failed) =
        exec::execute_waves(ws, waves, command_for, jobs, max_bytes, false, &mut emitter).await;

    let run_summary = RunSummary::new(passed, failed, started.elapsed().as_millis() as u64);
    emitter.emit_summary(&run_summary, run_summary.human());
    emitter.finish();
    aggregate_exit(failed > 0, false)
}

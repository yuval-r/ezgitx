use std::collections::BTreeSet;
use std::time::Instant;

use crate::errors::{ErrorCode, ErrorInfo, aggregate_exit};
use crate::exec::{self, RUN_HEADERS, RunSummary};
use crate::output::Emitter;
use crate::state;
use crate::workspace::{Repo, Workspace};

/// `ezgitx run` (PRD §5.3, §9.4). Takes no locks (§7): commands are
/// user-supplied and arbitrarily long.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    ws: &Workspace,
    targets: Vec<Repo>,
    cmd: Option<String>,
    with_deps: bool,
    with_dependents: bool,
    jobs: usize,
    max_bytes: usize,
    human: bool,
) -> i32 {
    let started = Instant::now();
    let target_names: BTreeSet<String> = targets.iter().map(|r| r.name.clone()).collect();

    // --with-deps expands the targets with their *stale* transitive upstreams
    // (PRD §9.4); fresh upstreams are skipped. Targets always execute, so only
    // non-targets are candidates.
    let mut candidates: BTreeSet<String> = BTreeSet::new();
    if with_deps {
        for name in &target_names {
            candidates.extend(crate::graph::transitive_upstreams(ws, name));
        }
    }
    if with_dependents {
        for name in &target_names {
            candidates.extend(
                crate::graph::downstream_closure(ws, name)
                    .into_iter()
                    .map(|a| a.repo),
            );
        }
    }
    candidates.retain(|c| !target_names.contains(c));

    // One HEAD probe over everything we judge for staleness or record against:
    // targets + candidates + all their transitive upstreams. Refs don't move
    // during a build, so this snapshot is valid at record time too.
    let mut universe: BTreeSet<String> = target_names.clone();
    universe.extend(candidates.iter().cloned());
    let upstreams: Vec<String> = universe
        .iter()
        .flat_map(|name| crate::graph::transitive_upstreams(ws, name))
        .collect();
    universe.extend(upstreams);
    let heads = state::current_heads(state::with_paths(ws, universe), jobs, max_bytes).await;

    // A candidate joins the run only if it is stale under the manifest model.
    let mut set = target_names.clone();
    set.extend(
        candidates
            .into_iter()
            .filter(|c| state::is_stale(ws, c, state::read(&ws.root, c).as_ref(), &heads)),
    );

    // With dependency flags the set runs in topological waves; a plain run is
    // a single unordered wave (staleness never changes what executes).
    let waves: Vec<Vec<String>> = if with_deps || with_dependents {
        crate::graph::topo_waves(ws, &set)
    } else {
        vec![target_names.iter().cloned().collect()]
    };

    let mut emitter = Emitter::new(human, RUN_HEADERS);
    let command_for = |repo: &Repo| -> Result<String, ErrorInfo> {
        // Expanded upstreams always run their default_cmd (§9.4); explicit
        // targets use the given command when present.
        if let Some(c) = &cmd {
            if target_names.contains(&repo.name) {
                return Ok(c.clone());
            }
        }
        repo.default_cmd.clone().ok_or_else(|| {
            ErrorInfo::new(
                ErrorCode::NoDefaultCmd,
                format!("repo {:?} has no default_cmd", repo.name),
            )
        })
    };

    let tally = exec::execute_waves(
        ws,
        waves,
        command_for,
        jobs,
        max_bytes,
        true,
        &heads,
        &mut emitter,
    )
    .await;

    let summary = RunSummary::new(
        tally.passed,
        tally.failed,
        started.elapsed().as_millis() as u64,
    );
    emitter.emit_summary(&summary, summary.human());
    emitter.finish();
    aggregate_exit(tally.failed > 0, false)
}

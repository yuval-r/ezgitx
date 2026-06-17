use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use serde::Serialize;

use crate::errors::aggregate_exit;
use crate::exec::{self, RUN_HEADERS};
use crate::git;
use crate::output::Emitter;
use crate::workspace::{Repo, Workspace};

/// The cross-repo "definition of done" verdict (a single line after the per-repo
/// run lines). `verdict` is `"pass"` only when nothing failed.
#[derive(Serialize)]
struct Verdict {
    r#type: &'static str,
    verdict: &'static str,
    checked: usize,
    failed: Vec<String>,
    duration_ms: u64,
}

/// `ezgitx verify`: the cross-repo done-gate. Scans the whole workspace for
/// uncommitted changes, then runs each repo's `check_cmd` (fallback
/// `default_cmd`) across every dirty repo **plus its transitive downstream
/// closure**, in dependency-ordered waves. Per-repo `RunLine`s stream first
/// (so an agent sees which repo failed and why), then a single pass/fail
/// verdict. It is a gate, not a build: it never records freshness.
pub async fn run(ws: &Workspace, jobs: usize, max_bytes: usize, human: bool) -> i32 {
    let started = Instant::now();

    // 1. Find every repo with uncommitted changes. A repo whose status can't be
    //    read is kept (treated as needing verification) so the problem surfaces,
    //    mirroring the `--dirty` filter in main::select_filtered.
    // Only name + path are needed for the dirty check; mapping to a slim tuple
    // avoids cloning each Repo's command/depends_on fields per repo.
    let all: Vec<(String, std::path::PathBuf)> = ws
        .repos
        .values()
        .map(|r| (r.name.clone(), r.path.clone()))
        .collect();
    let mut dirty: BTreeSet<String> = BTreeSet::new();
    exec::run_parallel(
        all,
        jobs,
        |(name, path)| async move {
            let keep = git::is_dirty_or_unreadable(&path, max_bytes).await;
            (name, keep)
        },
        |(name, keep)| {
            if keep {
                dirty.insert(name);
            }
        },
    )
    .await;

    // 2. Verify set = the dirty repos ∪ everything downstream of a dirty repo
    //    (downstream_closure excludes the changed repo itself, so re-add it).
    let mut set: BTreeSet<String> = dirty.clone();
    for d in &dirty {
        set.extend(
            crate::graph::downstream_closure(ws, d)
                .into_iter()
                .map(|a| a.repo),
        );
    }

    // 3. Run check_cmd (fallback default_cmd) over the set in topological waves.
    let waves = crate::graph::topo_waves(ws, &set);
    let mut emitter = Emitter::new(human, RUN_HEADERS);
    let heads = BTreeMap::new(); // gate, not a build: never records freshness
    let tally = exec::execute_waves(
        ws,
        waves,
        |repo: &Repo| repo.check_command(),
        jobs,
        max_bytes,
        false,
        &heads,
        &mut emitter,
    )
    .await;

    // 4. A single pass/fail verdict naming the failures.
    let pass = tally.failed == 0;
    let verdict = Verdict {
        r#type: "verdict",
        verdict: if pass { "pass" } else { "fail" },
        checked: (tally.passed + tally.failed) as usize,
        failed: tally.failed_repos,
        duration_ms: started.elapsed().as_millis() as u64,
    };
    let human_text = if pass {
        format!("PASS ({} checked)", verdict.checked)
    } else {
        format!("FAIL: {}", verdict.failed.join(", "))
    };
    emitter.emit_summary(&verdict, human_text);
    emitter.finish();
    aggregate_exit(tally.failed > 0, false)
}

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::git;
use crate::workspace::Workspace;

/// Freshness record (PRD §9.3): written after every successful `ezgitx run`
/// in a repo. Per-repo files avoid write contention between sessions.
#[derive(Serialize, Deserialize, Debug)]
pub struct Record {
    pub head: String,
    pub cmd: String,
    pub finished_at: String,
}

fn state_path(root: &Path, repo: &str) -> PathBuf {
    root.join(".ezgitx")
        .join("state")
        .join(format!("{repo}.json"))
}

pub fn read(root: &Path, repo: &str) -> Option<Record> {
    let text = std::fs::read_to_string(state_path(root, repo)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Monotonic in-process discriminator for staging-file names. The pid keeps
/// concurrent *sessions* apart; this keeps concurrent *tasks* in one process
/// apart, so the uniqueness invariant doesn't rest on call-pattern luck.
pub fn unique_suffix() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// Atomic write: pid+counter-suffixed tmp file + rename.
pub fn write(root: &Path, repo: &str, record: &Record) -> std::io::Result<()> {
    let path = state_path(root, repo);
    let dir = path.parent().unwrap();
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(
        ".{repo}.{}.{}.tmp",
        std::process::id(),
        unique_suffix()
    ));
    std::fs::write(&tmp, serde_json::to_vec(record)?)?;
    std::fs::rename(&tmp, &path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

pub fn record_success(root: &Path, repo: &str, head: String, cmd: &str) {
    let record = Record {
        head,
        cmd: cmd.to_string(),
        finished_at: jiff::Timestamp::now().to_string(),
    };
    if let Err(e) = write(root, repo, &record) {
        eprintln!("ezgitx: failed to record freshness for {repo}: {e}");
    }
}

/// A repo is stale when its current HEAD differs from the recorded one or no
/// record exists (PRD §9.3). Unreadable HEAD also counts as stale — the model
/// only ever degrades toward redundant rebuilds, never falsely-fresh.
async fn check_stale(root: PathBuf, repo: String, path: PathBuf, max_bytes: usize) -> bool {
    let Some(record) = read(&root, &repo) else {
        return true;
    };
    match git::head_sha(&path, max_bytes).await {
        Ok(head) => head != record.head,
        Err(_) => true,
    }
}

/// Resolve repo names to `(name, path)` pairs for owned-data probing.
/// Cheap and synchronous — do this before moving work into spawned tasks.
pub fn with_paths(
    ws: &Workspace,
    names: impl IntoIterator<Item = String>,
) -> Vec<(String, PathBuf)> {
    names
        .into_iter()
        .filter_map(|n| ws.repos.get(&n).map(|r| (n, r.path.clone())))
        .collect()
}

/// Concurrently filter `(name, path)` pairs down to the stale ones, sorted.
/// Each check spawns a `git rev-parse`; probing them in one parallel wave
/// avoids both sequential bottlenecks and re-probing shared dependencies.
pub async fn filter_stale_paths(
    root: &Path,
    repos: Vec<(String, PathBuf)>,
    max_bytes: usize,
) -> Vec<String> {
    let mut set = tokio::task::JoinSet::new();
    for (name, path) in repos {
        let root = root.to_path_buf();
        set.spawn(async move {
            let stale = check_stale(root, name.clone(), path, max_bytes).await;
            (name, stale)
        });
    }
    let mut stale = Vec::new();
    while let Some(result) = set.join_next().await {
        if let Ok((name, true)) = result {
            stale.push(name);
        }
    }
    stale.sort();
    stale
}

/// The stale subset of `names`, probed concurrently in a single wave.
pub async fn filter_stale(
    ws: &Workspace,
    names: &BTreeSet<String>,
    max_bytes: usize,
) -> Vec<String> {
    filter_stale_paths(&ws.root, with_paths(ws, names.iter().cloned()), max_bytes).await
}

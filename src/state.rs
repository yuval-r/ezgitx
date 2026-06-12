use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::git;
use crate::graph;
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

/// Atomic write: tmp file (pid-suffixed against concurrent sessions) + rename.
pub fn write(root: &Path, repo: &str, record: &Record) -> std::io::Result<()> {
    let path = state_path(root, repo);
    let dir = path.parent().unwrap();
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".{repo}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, serde_json::to_vec(record)?)?;
    std::fs::rename(&tmp, &path)
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

/// Transitive upstream dependencies of `repo` that are stale, sorted. Each
/// check spawns a `git rev-parse`, so they run concurrently — sequential
/// probing would bottleneck on deep dependency trees.
pub async fn stale_upstreams(ws: &Workspace, repo: &str, max_bytes: usize) -> Vec<String> {
    let mut set = tokio::task::JoinSet::new();
    for upstream in graph::transitive_upstreams(ws, repo) {
        let Some(entry) = ws.repos.get(&upstream) else {
            continue;
        };
        let root = ws.root.clone();
        let path = entry.path.clone();
        set.spawn(async move {
            let stale = check_stale(root, upstream.clone(), path, max_bytes).await;
            (upstream, stale)
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

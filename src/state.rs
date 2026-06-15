use std::collections::{BTreeMap, BTreeSet};
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
    /// Heads of this repo's transitive upstreams at the time it was built
    /// (V2 manifest model). Absent in pre-manifest records → reads as empty.
    #[serde(default)]
    pub deps: BTreeMap<String, String>,
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

pub fn record_success(
    root: &Path,
    repo: &str,
    head: String,
    cmd: &str,
    deps: BTreeMap<String, String>,
) {
    let record = Record {
        head,
        cmd: cmd.to_string(),
        finished_at: jiff::Timestamp::now().to_string(),
        deps,
    };
    if let Err(e) = write(root, repo, &record) {
        eprintln!("ezgitx: failed to record freshness for {repo}: {e}");
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

/// Current HEAD of each `(name, path)` pair, probed concurrently (capped at
/// `jobs`, like `run_parallel`). Unreadable repos are omitted from the map
/// (callers treat "absent" as "moved", which only ever errs toward a rebuild).
pub async fn current_heads(
    repos: Vec<(String, PathBuf)>,
    jobs: usize,
    max_bytes: usize,
) -> BTreeMap<String, String> {
    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(jobs.max(1)));
    let mut set = tokio::task::JoinSet::new();
    for (name, path) in repos {
        let permit = semaphore.clone();
        set.spawn(async move {
            let _permit = permit.acquire_owned().await.expect("semaphore closed");
            (name, git::head_sha(&path, max_bytes).await.ok())
        });
    }
    let mut heads = BTreeMap::new();
    while let Some(result) = set.join_next().await {
        let (name, head) = result.expect("current_heads task panicked");
        if let Some(head) = head {
            heads.insert(name, head);
        }
    }
    heads
}

/// Upstreams of a repo that sit at a commit other than the one the repo's
/// record was built against. With no record (or a pre-manifest one), every
/// readable upstream drifts. `heads` must hold the repo's transitive upstreams.
pub fn deps_drift(
    upstreams: &BTreeSet<String>,
    record: Option<&Record>,
    heads: &BTreeMap<String, String>,
) -> Vec<String> {
    upstreams
        .iter()
        .filter(|u| heads.get(*u) != record.and_then(|r| r.deps.get(*u)))
        .cloned()
        .collect()
}

/// A repo needs (re)building when it has no record, its own HEAD moved or is
/// unreadable, or any transitive upstream drifted from its manifest. `heads`
/// must hold the repo and its transitive upstreams.
pub fn is_stale(
    ws: &Workspace,
    repo: &str,
    record: Option<&Record>,
    heads: &BTreeMap<String, String>,
) -> bool {
    let Some(record) = record else {
        return true;
    };
    if heads.get(repo) != Some(&record.head) {
        return true;
    }
    let upstreams = crate::graph::transitive_upstreams(ws, repo);
    !deps_drift(&upstreams, Some(record), heads).is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::Repo;
    use std::path::PathBuf;

    fn ws(edges: &[(&str, &[&str])]) -> Workspace {
        let repos = edges
            .iter()
            .map(|(name, deps)| {
                (
                    name.to_string(),
                    Repo {
                        name: name.to_string(),
                        path: PathBuf::from(format!("/w/{name}")),
                        default_cmd: None,
                        check_cmd: None,
                        depends_on: deps.iter().map(|d| d.to_string()).collect(),
                    },
                )
            })
            .collect();
        Workspace {
            root: PathBuf::from("/w"),
            repos,
            groups: BTreeMap::new(),
        }
    }

    fn rec(head: &str, deps: &[(&str, &str)]) -> Record {
        Record {
            head: head.to_string(),
            cmd: "c".to_string(),
            finished_at: "t".to_string(),
            deps: deps
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    fn heads(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn upstreams(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn deps_drift_flags_moved_upstream() {
        let r = rec("APP", &[("core", "C1")]);
        let h = heads(&[("core", "C2")]);
        assert_eq!(
            deps_drift(&upstreams(&["core"]), Some(&r), &h),
            vec!["core".to_string()]
        );
    }

    #[test]
    fn deps_drift_clean_when_matching() {
        let r = rec("APP", &[("core", "C1")]);
        let h = heads(&[("core", "C1")]);
        assert!(deps_drift(&upstreams(&["core"]), Some(&r), &h).is_empty());
    }

    #[test]
    fn deps_drift_no_record_flags_all_sorted() {
        let h = heads(&[("core", "C1"), ("lib", "L1")]);
        assert_eq!(
            deps_drift(&upstreams(&["core", "lib"]), None, &h),
            vec!["core".to_string(), "lib".to_string()]
        );
    }

    #[test]
    fn deps_drift_legacy_record_without_deps_flags_all() {
        let r = rec("APP", &[]); // pre-manifest record: deps empty
        let h = heads(&[("core", "C1")]);
        assert_eq!(
            deps_drift(&upstreams(&["core"]), Some(&r), &h),
            vec!["core".to_string()]
        );
    }

    #[test]
    fn deps_drift_both_absent_is_not_drift() {
        // Upstream "ghost" is declared but unreadable (absent from heads) and was
        // never recorded (absent from deps): both sides None → not drift. The
        // rebuild obligation is covered by own-staleness of "ghost" itself.
        let r = rec("APP", &[]);
        let h = heads(&[]);
        assert!(deps_drift(&upstreams(&["ghost"]), Some(&r), &h).is_empty());
    }

    #[test]
    fn is_stale_false_when_all_fresh() {
        let w = ws(&[("app", &["core"]), ("core", &[])]);
        let r = rec("APP", &[("core", "C1")]);
        let h = heads(&[("app", "APP"), ("core", "C1")]);
        assert!(!is_stale(&w, "app", Some(&r), &h));
    }

    #[test]
    fn is_stale_true_when_upstream_moved() {
        let w = ws(&[("app", &["core"]), ("core", &[])]);
        let r = rec("APP", &[("core", "C1")]);
        let h = heads(&[("app", "APP"), ("core", "C2")]);
        assert!(is_stale(&w, "app", Some(&r), &h));
    }

    #[test]
    fn is_stale_true_when_own_head_moved() {
        let w = ws(&[("app", &["core"]), ("core", &[])]);
        let r = rec("APP_OLD", &[("core", "C1")]);
        let h = heads(&[("app", "APP_NEW"), ("core", "C1")]);
        assert!(is_stale(&w, "app", Some(&r), &h));
    }

    #[test]
    fn is_stale_true_when_no_record() {
        let w = ws(&[("app", &["core"]), ("core", &[])]);
        assert!(is_stale(&w, "app", None, &BTreeMap::new()));
    }
}

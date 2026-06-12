use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::errors::{ErrorCode, ErrorInfo};

pub const DEFAULT_TTL: Duration = Duration::from_secs(600);
const POLL_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Serialize, Deserialize, Debug)]
pub struct LockInfo {
    pub pid: u32,
    pub hostname: String,
    pub started_at: String,
    pub op: String,
}

/// Held advisory lock; the file is removed on drop.
#[derive(Debug)]
pub struct LockGuard {
    path: PathBuf,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn locks_dir(root: &Path) -> PathBuf {
    root.join(".ezgitx").join("locks")
}

pub fn repo_lock_path(root: &Path, repo: &str) -> PathBuf {
    locks_dir(root).join(format!("repo-{repo}.lock"))
}

pub fn workspace_lock_path(root: &Path) -> PathBuf {
    locks_dir(root).join("workspace.lock")
}

fn hostname() -> String {
    gethostname::gethostname().to_string_lossy().into_owned()
}

/// A lock is stale when its PID is dead on the same host, its age exceeds
/// the TTL, or its content is unreadable (PRD §7).
fn is_stale(info: Option<&LockInfo>, ttl: Duration) -> bool {
    let Some(info) = info else {
        return true; // unparseable lock file
    };
    if info.hostname == hostname() {
        // Lock content is external input: a huge u32 pid would wrap negative,
        // and kill(-1, 0) / kill(0, 0) probe process *groups*, reporting a
        // garbage lock as held. Non-positive pids cannot be a live holder.
        let pid = info.pid as libc::pid_t;
        let alive = pid > 0
            && (unsafe { libc::kill(pid, 0) } == 0
                || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM));
        if !alive {
            return true;
        }
    }
    match info.started_at.parse::<jiff::Timestamp>() {
        Ok(started) => match jiff::Timestamp::now().since(started) {
            // A negative age means started_at is in the future — cross-host
            // clock skew, not expiry. Comparing in i64 keeps it "fresh";
            // casting to u64 would wrap it huge and break a live lock.
            Ok(span) => span.get_seconds() > ttl.as_secs() as i64,
            Err(_) => true,
        },
        Err(_) => true,
    }
}

/// One non-blocking acquisition attempt. `Err(holder)` reports the live
/// holder when readable.
///
/// The lock is published *atomically with its content*: the JSON is fully
/// written to a private pid-suffixed tmp file, then `hard_link` makes it
/// appear at the lock path in one step. A reader can therefore never observe
/// an empty or half-written lock — which it would have to classify as stale
/// and break, racing a live holder.
fn try_acquire(path: &Path, op: &str, ttl: Duration) -> Result<LockGuard, Option<LockInfo>> {
    let Some(dir) = path.parent() else {
        return Err(None);
    };
    let _ = fs::create_dir_all(dir);

    let info = LockInfo {
        pid: std::process::id(),
        hostname: hostname(),
        started_at: jiff::Timestamp::now().to_string(),
        op: op.to_string(),
    };
    let Ok(payload) = serde_json::to_vec(&info) else {
        return Err(None);
    };
    let tmp = dir.join(format!(
        ".{}.{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id()
    ));
    if fs::write(&tmp, payload).is_err() {
        let _ = fs::remove_file(&tmp);
        return Err(None);
    }

    let result = loop {
        match fs::hard_link(&tmp, path) {
            Ok(()) => {
                break Ok(LockGuard {
                    path: path.to_path_buf(),
                });
            }
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                let content = match fs::read_to_string(path) {
                    Ok(s) => s,
                    // Holder released between the link attempt and the
                    // read: retry immediately, no false "stale lock" notice.
                    Err(e) if e.kind() == ErrorKind::NotFound => continue,
                    Err(_) => String::new(), // unreadable → stale path below
                };
                let holder: Option<LockInfo> = serde_json::from_str(&content).ok();
                if is_stale(holder.as_ref(), ttl) {
                    eprintln!("ezgitx: breaking stale lock {}", path.display());
                    let _ = fs::remove_file(path);
                    // Loop: if we raced another breaker, hard_link decides.
                    continue;
                }
                break Err(holder);
            }
            Err(_) => break Err(None),
        }
    };
    let _ = fs::remove_file(&tmp);
    result
}

fn held_error(path: &Path, holder: Option<LockInfo>) -> ErrorInfo {
    let detail = holder
        .map(|h| format!("held by pid {} ({}) since {}", h.pid, h.op, h.started_at))
        .unwrap_or_else(|| "held by another process".to_string());
    ErrorInfo::new(
        ErrorCode::LockHeld,
        format!(
            "lock {} {detail}",
            path.file_name().unwrap_or_default().to_string_lossy()
        ),
    )
}

/// Acquire a lock file. Contention fails instantly with `lock_held`
/// (zero-interaction, PRD §7); `wait_secs` opts into bounded polling.
pub async fn acquire(
    path: &Path,
    op: &str,
    wait_secs: Option<u64>,
) -> Result<LockGuard, ErrorInfo> {
    let deadline = wait_secs.map(|s| std::time::Instant::now() + Duration::from_secs(s));
    loop {
        match try_acquire(path, op, DEFAULT_TTL) {
            Ok(guard) => return Ok(guard),
            Err(holder) => match deadline {
                Some(d) if std::time::Instant::now() < d => {
                    tokio::time::sleep(POLL_INTERVAL).await;
                }
                _ => return Err(held_error(path, holder)),
            },
        }
    }
}

/// Mutating ops check the global workspace lock before proceeding (PRD §7)
/// without taking it. Returns the `lock_held` error when a live holder exists.
pub fn check_workspace_lock(root: &Path) -> Result<(), ErrorInfo> {
    let path = workspace_lock_path(root);
    let content = match fs::read_to_string(&path) {
        Ok(s) => s,
        // Absent (or released mid-check) means no holder — not a stale lock.
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(()),
        Err(_) => String::new(), // unreadable → stale path below
    };
    let holder: Option<LockInfo> = serde_json::from_str(&content).ok();
    if is_stale(holder.as_ref(), DEFAULT_TTL) {
        eprintln!("ezgitx: breaking stale lock {}", path.display());
        let _ = fs::remove_file(&path);
        return Ok(());
    }
    Err(held_error(&path, holder))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn acquire_and_release() {
        let dir = tempfile::tempdir().unwrap();
        let path = repo_lock_path(dir.path(), "a");
        let guard = acquire(&path, "pull", None).await.unwrap();
        assert!(path.exists());
        drop(guard);
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn contention_fails_instantly() {
        let dir = tempfile::tempdir().unwrap();
        let path = repo_lock_path(dir.path(), "a");
        let _guard = acquire(&path, "pull", None).await.unwrap();
        let err = acquire(&path, "pull", None).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::LockHeld);
    }

    #[tokio::test]
    async fn dead_pid_lock_is_broken() {
        let dir = tempfile::tempdir().unwrap();
        let path = repo_lock_path(dir.path(), "a");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        // PID 999999999 is far above any real pid_max.
        let stale = LockInfo {
            pid: 999_999_999,
            hostname: hostname(),
            started_at: jiff::Timestamp::now().to_string(),
            op: "pull".to_string(),
        };
        fs::write(&path, serde_json::to_string(&stale).unwrap()).unwrap();
        let guard = acquire(&path, "pull", None).await;
        assert!(guard.is_ok());
    }

    #[tokio::test]
    async fn expired_ttl_lock_is_broken() {
        let dir = tempfile::tempdir().unwrap();
        let path = repo_lock_path(dir.path(), "a");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        // Live PID (ours) but on another "host", so only the TTL applies.
        let stale = LockInfo {
            pid: std::process::id(),
            hostname: "elsewhere".to_string(),
            started_at: "2020-01-01T00:00:00Z".to_string(),
            op: "pull".to_string(),
        };
        fs::write(&path, serde_json::to_string(&stale).unwrap()).unwrap();
        assert!(acquire(&path, "pull", None).await.is_ok());
    }

    #[tokio::test]
    async fn lock_publishes_full_content_and_leaves_no_tmp() {
        let dir = tempfile::tempdir().unwrap();
        let path = repo_lock_path(dir.path(), "a");
        let guard = acquire(&path, "pull", None).await.unwrap();
        // The published lock is complete, parseable JSON the moment it
        // exists, and the tmp staging file is gone while the lock is held.
        let info: LockInfo = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(info.pid, std::process::id());
        assert_eq!(info.op, "pull");
        let leftovers: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path() != path)
            .collect();
        assert!(leftovers.is_empty(), "staging leftovers: {leftovers:?}");
        drop(guard);
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn ttl_uses_total_elapsed_seconds() {
        // Pins jiff semantics: Timestamp::since defaults to seconds as the
        // largest unit, so get_seconds() is the TOTAL age (660 for 11 min),
        // not a 0-59 component. Foreign hostname skips the PID check, so
        // only the TTL decides: 11 min > 10 min TTL → stale; 9 min → held.
        let dir = tempfile::tempdir().unwrap();
        let lock_for_age = |minutes: i64| LockInfo {
            pid: 12345,
            hostname: "elsewhere".to_string(),
            started_at: (jiff::Timestamp::now() - jiff::ToSpan::minutes(minutes)).to_string(),
            op: "pull".to_string(),
        };

        let expired = repo_lock_path(dir.path(), "expired");
        fs::create_dir_all(expired.parent().unwrap()).unwrap();
        fs::write(&expired, serde_json::to_string(&lock_for_age(11)).unwrap()).unwrap();
        assert!(acquire(&expired, "pull", None).await.is_ok());

        let held = repo_lock_path(dir.path(), "held");
        fs::write(&held, serde_json::to_string(&lock_for_age(9)).unwrap()).unwrap();
        let err = acquire(&held, "pull", None).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::LockHeld);
    }

    #[tokio::test]
    async fn future_dated_lock_is_not_stale() {
        let dir = tempfile::tempdir().unwrap();
        let path = repo_lock_path(dir.path(), "a");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        // Another host whose clock runs ahead of ours: started_at is in the
        // future. Only the TTL applies (foreign hostname skips the PID
        // check), and a negative age must read as fresh, not expired.
        let skewed = jiff::Timestamp::now() + jiff::ToSpan::seconds(120);
        let holder = LockInfo {
            pid: 12345,
            hostname: "elsewhere".to_string(),
            started_at: skewed.to_string(),
            op: "pull".to_string(),
        };
        fs::write(&path, serde_json::to_string(&holder).unwrap()).unwrap();
        let err = acquire(&path, "pull", None).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::LockHeld);
    }

    #[tokio::test]
    async fn unparseable_lock_is_broken() {
        let dir = tempfile::tempdir().unwrap();
        let path = repo_lock_path(dir.path(), "a");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "garbage").unwrap();
        assert!(acquire(&path, "pull", None).await.is_ok());
    }

    #[tokio::test]
    async fn wait_succeeds_when_released() {
        let dir = tempfile::tempdir().unwrap();
        let path = repo_lock_path(dir.path(), "a");
        let guard = acquire(&path, "pull", None).await.unwrap();
        let path2 = path.clone();
        let waiter = tokio::spawn(async move { acquire(&path2, "pull", Some(5)).await });
        tokio::time::sleep(Duration::from_millis(400)).await;
        drop(guard);
        assert!(waiter.await.unwrap().is_ok());
    }

    #[test]
    fn workspace_lock_check() {
        let dir = tempfile::tempdir().unwrap();
        assert!(check_workspace_lock(dir.path()).is_ok());
        let path = workspace_lock_path(dir.path());
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let live = LockInfo {
            pid: std::process::id(),
            hostname: hostname(),
            started_at: jiff::Timestamp::now().to_string(),
            op: "sync".to_string(),
        };
        fs::write(&path, serde_json::to_string(&live).unwrap()).unwrap();
        assert_eq!(
            check_workspace_lock(dir.path()).unwrap_err().code,
            ErrorCode::LockHeld
        );
    }
}

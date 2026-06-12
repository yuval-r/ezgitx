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
        let alive = unsafe { libc::kill(info.pid as libc::pid_t, 0) } == 0
            || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
        if !alive {
            return true;
        }
    }
    match info.started_at.parse::<jiff::Timestamp>() {
        Ok(started) => {
            let age = jiff::Timestamp::now().since(started).ok();
            match age {
                Some(span) => span.get_seconds() as u64 > ttl.as_secs(),
                None => true,
            }
        }
        Err(_) => true,
    }
}

/// One non-blocking acquisition attempt. `Err(holder)` reports the live
/// holder when readable.
fn try_acquire(path: &Path, op: &str, ttl: Duration) -> Result<LockGuard, Option<LockInfo>> {
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    loop {
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
        {
            Ok(file) => {
                let info = LockInfo {
                    pid: std::process::id(),
                    hostname: hostname(),
                    started_at: jiff::Timestamp::now().to_string(),
                    op: op.to_string(),
                };
                serde_json::to_writer(&file, &info).map_err(|_| None)?;
                return Ok(LockGuard {
                    path: path.to_path_buf(),
                });
            }
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                let holder: Option<LockInfo> = fs::read_to_string(path)
                    .ok()
                    .and_then(|s| serde_json::from_str(&s).ok());
                if is_stale(holder.as_ref(), ttl) {
                    eprintln!("ezgitx: breaking stale lock {}", path.display());
                    let _ = fs::remove_file(path);
                    // Loop: if we raced another breaker, create_new decides.
                    continue;
                }
                return Err(holder);
            }
            Err(_) => return Err(None),
        }
    }
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
    if !path.exists() {
        return Ok(());
    }
    let holder: Option<LockInfo> = fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok());
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

use serde::Serialize;

use crate::errors::EXIT_OK;
use crate::lock;
use crate::output::Emitter;
use crate::workspace::Workspace;

/// One active advisory lock, identified by its holding process (pid@host) and
/// the operation it is running.
#[derive(Serialize)]
struct SessionLine {
    /// Lock file stem, e.g. `repo-app` or `workspace`.
    lock: String,
    scope: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    repo: Option<String>,
    pid: u32,
    host: String,
    op: String,
    since: String,
}

const HEADERS: &[&str] = &["REPO", "SCOPE", "PID", "HOST", "OP", "SINCE"];

/// `ezgitx sessions`: read-only listing of active advisory locks under
/// `.ezgitx/locks/`: who holds what, for multi-agent workspace visibility.
/// Stale locks (dead pid / expired TTL) are omitted; unlike acquisition this
/// never breaks them.
pub fn run(ws: &Workspace, human: bool) -> i32 {
    let mut emitter = Emitter::new(human, HEADERS);
    for s in lock::active_sessions(&ws.root) {
        let line = SessionLine {
            lock: s.lock,
            scope: s.scope,
            repo: s.repo,
            pid: s.info.pid,
            host: s.info.hostname,
            op: s.info.op,
            since: s.info.started_at,
        };
        let row = vec![
            line.repo.as_deref().unwrap_or("-").to_string(),
            line.scope.to_string(),
            line.pid.to_string(),
            line.host.clone(),
            line.op.clone(),
            line.since.clone(),
        ];
        emitter.emit(&line, row);
    }
    emitter.finish();
    EXIT_OK
}

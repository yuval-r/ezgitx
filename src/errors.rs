use serde::Serialize;

/// Stable error-code enum (PRD §6.2). Additions are minor-version changes;
/// renames or removals are breaking.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    DirtyTree,
    Diverged,
    Detached,
    LockHeld,
    NotARepo,
    NoDefaultCmd,
    GitFailed,
    SpawnFailed,
    ConfigInvalid,
    DependencyCycle,
    UpstreamFailed,
}

impl ErrorCode {
    /// The wire name of this code, for human-readable rendering. Must match
    /// the serde snake_case serialization (enforced by test).
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::DirtyTree => "dirty_tree",
            ErrorCode::Diverged => "diverged",
            ErrorCode::Detached => "detached",
            ErrorCode::LockHeld => "lock_held",
            ErrorCode::NotARepo => "not_a_repo",
            ErrorCode::NoDefaultCmd => "no_default_cmd",
            ErrorCode::GitFailed => "git_failed",
            ErrorCode::SpawnFailed => "spawn_failed",
            ErrorCode::ConfigInvalid => "config_invalid",
            ErrorCode::DependencyCycle => "dependency_cycle",
            ErrorCode::UpstreamFailed => "upstream_failed",
        }
    }
}

#[derive(Serialize, Clone, Debug)]
pub struct ErrorInfo {
    pub code: ErrorCode,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
}

impl ErrorInfo {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            snippet: None,
        }
    }

    /// Attach a byte-capped snippet (PRD §3.4): first `max` bytes, lossy UTF-8.
    pub fn with_snippet(mut self, raw: &[u8], max: usize) -> Self {
        let trimmed = String::from_utf8_lossy(&raw[..raw.len().min(max)])
            .trim()
            .to_string();
        if !trimmed.is_empty() {
            self.snippet = Some(trimmed);
        }
        self
    }
}

pub const EXIT_OK: i32 = 0;
pub const EXIT_REPO_FAILURE: i32 = 1;
pub const EXIT_USAGE: i32 = 2;
pub const EXIT_LOCK: i32 = 3;

/// Exit-code precedence for mixed per-repo outcomes: lock contention (3)
/// outranks ordinary repo failures (1). Usage/config errors (2) never mix
/// with per-repo results — they abort before any repo work starts.
pub fn aggregate_exit(any_failure: bool, any_lock_held: bool) -> i32 {
    if any_lock_held {
        EXIT_LOCK
    } else if any_failure {
        EXIT_REPO_FAILURE
    } else {
        EXIT_OK
    }
}

/// Top-level (non-per-repo) error line: `{"error": {...}}` on stdout.
pub fn print_top_level(err: &ErrorInfo) {
    #[derive(Serialize)]
    struct Line<'a> {
        error: &'a ErrorInfo,
    }
    crate::output::print_json_line(&Line { error: err });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn as_str_matches_serde_wire_names() {
        const ALL: [ErrorCode; 11] = [
            ErrorCode::DirtyTree,
            ErrorCode::Diverged,
            ErrorCode::Detached,
            ErrorCode::LockHeld,
            ErrorCode::NotARepo,
            ErrorCode::NoDefaultCmd,
            ErrorCode::GitFailed,
            ErrorCode::SpawnFailed,
            ErrorCode::ConfigInvalid,
            ErrorCode::DependencyCycle,
            ErrorCode::UpstreamFailed,
        ];
        for code in ALL {
            let wire = serde_json::to_value(code).unwrap();
            assert_eq!(wire.as_str(), Some(code.as_str()), "drift for {code:?}");
        }
    }
}

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
    println!("{}", serde_json::to_string(&Line { error: err }).unwrap());
}

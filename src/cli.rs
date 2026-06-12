use clap::{Args, Parser, Subcommand};

use crate::output::DEFAULT_MAX_BYTES;
use crate::workspace::Targeting;

/// Agent-native multi-repo git CLI. JSONL on stdout by default; logs and
/// progress on stderr; never prompts.
#[derive(Parser, Debug)]
#[command(name = "ezgitx", version, about)]
pub struct Cli {
    /// Human-readable tables instead of JSONL
    #[arg(long, global = true)]
    pub human: bool,

    /// Max parallel repo operations (default: logical CPU count)
    #[arg(long, global = true)]
    pub jobs: Option<usize>,

    /// Byte cap for output tails and error snippets
    #[arg(long = "max-bytes", global = true, default_value_t = DEFAULT_MAX_BYTES)]
    pub max_bytes: usize,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Args, Debug, Default)]
pub struct TargetArgs {
    /// Every repo in the config
    #[arg(long)]
    pub all: bool,

    /// Repos in the named group (repeatable)
    #[arg(long)]
    pub group: Vec<String>,

    /// A single repo by directory name (repeatable)
    #[arg(long)]
    pub repo: Vec<String>,

    /// Filter the selection to repos with uncommitted changes
    #[arg(long)]
    pub dirty: bool,
}

impl TargetArgs {
    pub fn targeting(&self) -> Targeting {
        Targeting {
            all: self.all,
            groups: self.group.clone(),
            repos: self.repo.clone(),
        }
    }
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Local working-tree + sync state per repo (never fetches)
    Status {
        #[command(flatten)]
        target: TargetArgs,
    },
    /// Concurrent fetch + ff-only merge per repo
    Pull {
        #[command(flatten)]
        target: TargetArgs,
        /// Wait up to N seconds for contended locks instead of failing
        #[arg(long, value_name = "SECS")]
        wait: Option<u64>,
    },
    /// Run a command in each target repo in parallel
    Run {
        /// Command to run; omit to use each repo's default_cmd
        cmd: Option<String>,
        /// Also run stale upstream dependencies first, in dependency order
        #[arg(long = "with-deps")]
        with_deps: bool,
        #[command(flatten)]
        target: TargetArgs,
    },
    /// Generate the agent-facing SKILL.md at the workspace root
    InitSkill,
    /// List (and optionally validate) downstream dependents of a change
    CheckImpact {
        /// The changed repo (default: the repo containing the current directory)
        #[arg(long)]
        repo: Option<String>,
        /// Also execute each affected repo's check_cmd in dependency order
        #[arg(long)]
        check: bool,
    },
}

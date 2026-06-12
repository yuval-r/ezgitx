# Product Requirements Document: `ezgitx`

**An agent-native multi-repo CLI** — repo and binary: `ezgitx`

## 1. Vision & Objective

`ezgitx` is a Rust CLI for reading state, pulling updates, and running commands across many git repositories concurrently. Its differentiator is not multi-repo plumbing (prior art: `mani`, `gita`, `myrepos`, `git-xargs`) — it is the **agent I/O contract**: machine-first JSONL output, zero interactivity, deterministic truncation, stable error and exit-code schemas, and self-installing agent instructions via `init-skill`.

The primary deployment target is a flat workspace of sibling git repos (the reference workspace contains 18) where multiple AI coding agents (Claude Code sessions) operate simultaneously — some at the workspace root, some inside individual repos.

## 2. Target Users

* **Primary:** AI coding agents (Claude Code and similar) executing `ezgitx` through a shell tool. They need parseable output, no prompts, and bounded payload sizes.
* **Secondary:** The human developer who owns the workspace, spot-checking state with `--human`.

## 3. Design Principles

1. **Zero-interaction.** Never prompt. Set `GIT_TERMINAL_PROMPT=0` on all child git processes. If an operation would require input, fail instantly with a structured error.
2. **Agent-first I/O.** JSONL on stdout is the **default**; `--human` opts into tables. Human-readable progress and logs always go to stderr, never stdout. One format convention across all commands — there is no `--json`/`--jsonl`/`--format` zoo.
3. **Deterministic truncation.** No token-budget heuristics. Every free-text field has a hard byte cap (output tails default 2 KB, error snippets default 2 KB), a `truncated: true` marker when capped, and a `--max-bytes <N>` override.
4. **Bounded concurrency.** Operations across repos run in parallel via `tokio` with a job limit (default = logical CPU count, `--jobs <N>` override). No operation on one repo ever blocks output for another; results stream as JSONL lines when each repo completes.
5. **Shell out to system `git`.** All git operations spawn the `git` binary (`status --porcelain=v2 --branch`, `fetch`, `merge --ff-only`). Credentials, SSH agents, and helpers work exactly as in the user's terminal. No libgit2.

## 4. Workspace Model

### 4.1 Configuration: `.ezgitx.yml`

Lives at the workspace root. Defines logical groups of repos.

```yaml
version: 1
groups:
  saas-core:
    - path: ./hipster
      default_cmd: "bun run build"   # optional; used when `run` is given no command
    - path: ./billing
  data-pipelines:
    - path: ./spider
      default_cmd: "cargo build --release"
  agent-tools:
    - path: ./servers
```

* `path` is required, relative to the workspace root, and must contain a `.git` directory (validated lazily, reported as `not_a_repo` errors).
* `default_cmd` is optional.
* A repo may appear in multiple groups.
* Unknown keys are rejected (`config_invalid`, exit 2) so schema drift fails loudly.

### 4.2 Workspace Discovery

Every command traverses **upward** from the current directory until it finds `.ezgitx.yml`. This means an agent session running deep inside a single repo can target any sibling (`ezgitx run --repo spider "cargo test"`) without knowing the workspace layout — cross-repo validation falls out of discovery for free.

### 4.3 Targeting Flags (shared by all commands)

| Flag | Meaning |
|---|---|
| `--all` | every repo in the config |
| `--group <name>` | repos in the named group (repeatable) |
| `--repo <name>` | a single repo by directory name (repeatable) |
| `--dirty` | filter the selection to repos with uncommitted changes |

No targeting flag at the workspace root = `--all`. Inside a member repo with no flag = that repo only.

## 5. V1 Commands

### 5.1 `ezgitx status`

Reads local state per repo via `git status --porcelain=v2 --branch`. **Never fetches** — ahead/behind counts reflect the last fetch.

One JSONL line per repo:

| Field | Type | Notes |
|---|---|---|
| `repo` | string | directory name |
| `path` | string | absolute path |
| `branch` | string\|null | null when detached |
| `head` | string | short SHA |
| `state` | enum | `clean` \| `dirty` \| `detached` \| `conflicted` |
| `ahead` / `behind` | int\|null | vs upstream; null if no upstream |

### 5.2 `ezgitx pull`

Concurrent `git fetch` then `git merge --ff-only` per repo, holding that repo's lock. **Never creates merge commits.**

One JSONL line per repo:

| Field | Type | Notes |
|---|---|---|
| `repo` | string | |
| `status` | enum | `updated` \| `up_to_date` \| `skipped_dirty` \| `diverged` \| `detached` \| `error` |
| `commits_pulled` | int | 0 unless `updated` |
| `head` | string | post-operation short SHA |
| `error` | object\|absent | error schema (§6.2) |

Dirty working tree → `skipped_dirty` (no fetch attempted against the tree; fetch still runs so `behind` stays accurate). Diverged branch → `diverged`. Detached HEAD → `detached`. All three are per-repo failures (contribute to exit 1) but never stop other repos.

### 5.3 `ezgitx run [<cmd>]`

Spawns `<cmd>` via the user's shell in each target repo's directory, in parallel. With no `<cmd>`, uses each repo's `default_cmd` and errors (`no_default_cmd`) for repos without one.

One JSONL line per repo:

| Field | Type | Notes |
|---|---|---|
| `repo` | string | |
| `exit_code` | int\|null | null if the process could not spawn |
| `duration_ms` | int | |
| `stdout_tail` / `stderr_tail` | string | last N bytes (default 2 KB cap) |
| `truncated` | bool | true if either tail was capped |
| `error` | object\|absent | spawn-level failures only |

Final line: `{"type": "summary", "total": n, "passed": n, "failed": n, "duration_ms": n}`.

### 5.4 `ezgitx init-skill`

Generates `.claude/skills/ezgitx/SKILL.md` at the workspace root, teaching agents:

* Output is JSONL by default — never pass format flags.
* Target narrowly (`--group`, `--repo`, `--dirty`) instead of `--all` scans.
* Read `error.snippet` instead of requesting full logs.
* On exit code 3 (`lock_held`), wait briefly and retry; or pass `--wait <secs>`.
* The exit-code contract (§6.1) so failures are interpreted without parsing prose.

Idempotent: re-running overwrites the generated file.

## 6. Contracts

### 6.1 Exit Codes

| Code | Meaning |
|---|---|
| 0 | all targeted repos succeeded |
| 1 | ≥ 1 repo-level operation failed (others may have succeeded) |
| 2 | usage or config error (bad flags, invalid/missing `.ezgitx.yml`) |
| 3 | lock contention (lock held and `--wait` not given or expired) |

### 6.2 Error Schema

Every error is structured, on stdout, in-line with results:

```json
{"repo": "spider", "error": {"code": "dirty_tree", "message": "uncommitted changes block ff-only pull", "snippet": "M src/main.rs"}}
```

`code` is a stable enum: `dirty_tree`, `diverged`, `detached`, `lock_held`, `not_a_repo`, `no_default_cmd`, `git_failed`, `spawn_failed`, `config_invalid`. `snippet` is byte-capped (§3.3).

## 7. Locking (Two-Tier Advisory Locks)

Multiple agent sessions mutate the workspace concurrently; locks prevent conflicting mutations without serializing unrelated work.

* **Per-repo locks** — `.ezgitx/locks/repo-<name>.lock`. Taken by `pull` (and future mutating ops) for the repos they touch. An agent pulling in `hipster` never blocks an agent testing in `spider`.
* **Global lock** — `.ezgitx/locks/workspace.lock`. Reserved for workspace-level mutations (config rewrites, future `sync`/clone). Mutating ops also check it before proceeding.
* **Lock file content:** `{"pid": 123, "hostname": "host", "started_at": "<iso8601>", "op": "pull"}`.
* **Staleness:** a lock is stale when its PID is dead (same host) or `started_at` exceeds the TTL (default 10 min). Stale locks are broken automatically with a notice on stderr.
* **Contention behavior:** fail **instantly** with `lock_held` / exit 3 (zero-interaction principle). `--wait <secs>` opts into bounded blocking.
* `ezgitx run` takes **no locks** — commands are user-supplied and arbitrarily long; serializing them would deadlock the mesh use case. The generated skill documents this.

## 8. Technical Stack

* Rust 2021 edition
* `clap` (derive) — CLI parsing
* `tokio` + `tokio::process` — bounded-parallel child processes, streamed output
* `serde` / `serde_json` / `serde_yaml` — config and JSONL
* System `git` binary — all git operations (no `git2`/libgit2: credential handling would need reimplementation, the API is blocking, and its performance does not beat the git binary for these workloads)

## 9. V2 Backlog (staged, intentionally not designed yet)

* `ezgitx env` — expose workspace root, session context, and boundaries as JSON.
* `depends_on` in config + topologically ordered `run` (cut from V1: nothing consumed it).
* `ezgitx sync` — clone repos that exist in config but not on disk (takes the global lock).
* `gix` (gitoxide) fast-path for read-only status at larger repo counts.
* Compact/summary output modes if real agent payloads prove too large despite byte caps.
* MCP server mode, if shell invocation proves limiting.

## 10. Effort & Risk Assessment

* **V1 estimate:** 2–3 weeks with TDD (porcelain-v2 parser, lock staleness, config validation, and integration tests against temp git repos are the test surface).
* **Highest-confidence pieces:** `status`, `run`, `init-skill` — pure subprocess + parsing.
* **Riskiest piece:** `pull` edge cases (upstream config variance, fetch failures vs merge failures) — mitigated by ff-only semantics and the explicit per-repo failure enum.
* **Deliberately rejected complexity:** libgit2, token-budget heuristics (`context_warning`), global-only locking, dependency graphs in V1.

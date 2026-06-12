# Product Requirements Document: `ezgitx`

**An agent-native multi-repo CLI** — repo and binary: `ezgitx`

## 1. Vision & Objective

`ezgitx` is a Rust CLI for reading state, pulling updates, and running commands across many git repositories concurrently. Its differentiator is not multi-repo plumbing (prior art: `mani`, `gita`, `myrepos`, `git-xargs`) — it is the **agent I/O contract**: machine-first JSONL output, zero interactivity, deterministic truncation, stable error and exit-code schemas, self-installing agent instructions via `init-skill`, and (V2) cross-repo dependency awareness that eliminates agent blindness to upstream changes.

The primary deployment target is a flat workspace of sibling git repos (the reference workspace contains 18) where multiple AI coding agents (Claude Code sessions) operate simultaneously — some at the workspace root, some inside individual repos.

## 2. Target Users

* **Primary:** AI coding agents (Claude Code and similar) executing `ezgitx` through a shell tool. They need parseable output, no prompts, and bounded payload sizes.
* **Secondary:** The human developer who owns the workspace, spot-checking state with `--human`.

## 3. Design Principles

1. **Zero-interaction.** Never prompt. Set `GIT_TERMINAL_PROMPT=0` on all child git processes. If an operation would require input, fail instantly with a structured error.
2. **Agent-first I/O.** JSONL on stdout is the **default**; `--human` opts into tables. Human-readable progress and logs always go to stderr, never stdout. One format convention across all commands — there is no `--json`/`--jsonl`/`--format` zoo.
3. **No implicit side effects.** The tool never does work the caller didn't name. Staleness, drift, and impact are *reported* by default; *acting* on them requires an explicit flag (`--with-deps`, `--check`). Agents operate under tool timeouts and parse output against fixed schemas — surprise work breaks both.
4. **Deterministic truncation.** No token-budget heuristics. Every free-text field has a hard byte cap (output tails default 2 KB, error snippets default 2 KB), a `truncated: true` marker when capped, and a `--max-bytes <N>` override.
5. **Bounded concurrency.** Operations across repos run in parallel via `tokio` with a job limit (default = logical CPU count, `--jobs <N>` override). No operation on one repo ever blocks output for another; results stream as JSONL lines when each repo completes. Dependency-ordered execution (V2) runs in topological *waves*, parallel within each wave.
6. **Shell out to system `git`.** All git operations spawn the `git` binary (`status --porcelain=v2 --branch`, `fetch`, `merge --ff-only`). Credentials, SSH agents, and helpers work exactly as in the user's terminal. No libgit2.

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
* V2 adds two optional per-repo keys: `depends_on` and `check_cmd` (§9.2).

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

## 5. Commands

| Command | Stage | Purpose |
|---|---|---|
| `status` | V1 | local working-tree + sync state per repo, no fetch |
| `pull` | V1 | concurrent fetch + ff-only merge |
| `run` | V1 | parallel command execution across repos (`--with-deps`: V2) |
| `init-skill` | V1 | generate the agent-facing SKILL.md |
| `check-impact` | V2 | list (and optionally validate) downstream dependents of a change |
| `env`, `sync` | backlog | §10 |

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
| `stale_deps` | string[] | **V2** — upstream dependencies with unbuilt changes (§9.3); omitted in V1 |

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

Dirty working tree → `skipped_dirty` (fetch still runs so `behind` stays accurate). Diverged branch → `diverged`. Detached HEAD → `detached`. All three are per-repo failures (contribute to exit 1) but never stop other repos.

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
* **V2:** after modifying shared/upstream code, run `check-impact`; if a build fails and `status` shows `stale_deps`, retry with `run --with-deps`.

Idempotent: re-running overwrites the generated file.

## 6. Contracts

### 6.1 Exit Codes

| Code | Meaning |
|---|---|
| 0 | all targeted repos succeeded |
| 1 | ≥ 1 repo-level operation failed (others may have succeeded) |
| 2 | usage or config error (bad flags, invalid/missing `.ezgitx.yml`, dependency cycle) |
| 3 | lock contention (lock held and `--wait` not given or expired) |

### 6.2 Error Schema

Every error is structured, on stdout, in-line with results:

```json
{"repo": "spider", "error": {"code": "dirty_tree", "message": "uncommitted changes block ff-only pull", "snippet": "M src/main.rs"}}
```

`code` is a stable enum: `dirty_tree`, `diverged`, `detached`, `lock_held`, `not_a_repo`, `no_default_cmd`, `git_failed`, `spawn_failed`, `config_invalid`, and (V2) `dependency_cycle`, `upstream_failed`. `snippet` is byte-capped (§3.4).

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

## 9. V2: Cross-Repo Dependency Graph & Impact Analysis

### 9.1 Problem: Agent Blindness

An agent working in a downstream repo (a frontend) has no way to know that an upstream sibling (a data pipeline it consumes) has changed since it was last built. Failures surface as confusing downstream build/test errors that cost tokens to misdiagnose. Symmetrically, an agent that edits a shared upstream package cannot cheaply answer *"what must I re-verify?"* — today the answer is "run everything."

V2 solves both with a declared dependency graph, a commit-hash freshness model, and two explicit levers. Per principle §3.3, the tool **reports** staleness and impact by default and **acts** only on explicit request — never implicit upstream builds. (Rationale: agents run under tool timeouts — Claude Code's Bash default is 2 minutes — and parse output against fixed schemas; silently prepending a 5-minute upstream build to a 10-second downstream one produces an agent-visible timeout and result lines for repos the agent never targeted.)

### 9.2 Config Additions

```yaml
groups:
  saas-core:
    - path: ./hipster
      default_cmd: "bun run build"
      check_cmd: "bun run typecheck && bun test"  # optional; used by check-impact, falls back to default_cmd
      depends_on: ["spider"]                       # upstream repos, by directory name
```

* `depends_on` entries must name configured repos; the resulting graph must be a DAG. Cycles fail at config load with `dependency_cycle`, exit 2.
* The graph is built in memory per invocation (trivial at ≤ ~100 repos; no persistence needed).

### 9.3 Freshness Model: Recorded Commit Hashes

* After every **successful** `ezgitx run` execution in a repo, its HEAD is recorded atomically (tmp + rename) to `.ezgitx/state/<repo>.json`: `{"head": "<sha>", "cmd": "<cmd>", "finished_at": "<iso8601>"}`. Per-repo files avoid write contention between concurrent agent sessions.
* An upstream repo is **stale** when its current HEAD differs from its recorded head, or no record exists.
* **Explicitly rejected:** file-modification-timestamp freshness — mtimes are corrupted by checkouts, clock skew, and build artifacts.
* **Accepted limitation:** builds performed outside `ezgitx` (e.g., raw `cargo build`) don't update the record. The model degrades only toward *redundant* rebuilds, never toward falsely-fresh state — the safe direction.
* `status` surfaces the result as `stale_deps: [...]` per repo (§5.1), giving agents one-line visibility at zero extra cost.

### 9.4 `ezgitx run --with-deps` (explicit dependency-ordered execution)

* Expands the target set with each target's transitive upstream dependencies **that are stale** (fresh upstreams are skipped — no redundant work).
* Executes in topological waves: repos within a wave run in parallel (bounded by `--jobs`), waves run in dependency order. Upstream repos run their `default_cmd`.
* If an upstream fails, its dependents emit `{"repo": "...", "error": {"code": "upstream_failed", "message": "skipped: upstream spider failed"}}` with `exit_code: null` — the standard `run` line shape, no new schema.
* Without `--with-deps`, `run` behaves exactly as V1 — staleness never changes what executes.

### 9.5 `ezgitx check-impact [--repo <name>]`

Computes the transitive **downstream** closure (the reverse of `depends_on`) of the current or named repo.

* **Default: list only.** Cheap, no execution — one JSONL line per affected repo:

  | Field | Type | Notes |
  |---|---|---|
  | `repo` | string | affected downstream repo |
  | `depth` | int | 1 = direct dependent |
  | `via` | string[] | dependency path from the changed repo |

  Final line: `{"type": "summary", "changed": "<repo>", "affected": n}`. The agent decides what to do with the information — e.g., validate only depth-1 dependents.
* **`--check`:** additionally executes each affected repo's `check_cmd` (fallback: `default_cmd`) in topological waves, emitting standard `run`-shaped result lines after the listing. Exit codes per §6.1.

### 9.6 Non-Goals

File/path-level impact granularity, artifact caching, and remote build orchestration are explicitly out of scope — that is Bazel/Nx/Turborepo territory, designed for *within*-monorepo use. `ezgitx` does coarse repo-level analysis *across* repos, which is the right resolution for a ~20-repo workspace.

## 10. Backlog (staged, intentionally not designed yet)

* `ezgitx env` — expose workspace root, session context, and boundaries as JSON.
* `ezgitx sync` — clone repos that exist in config but not on disk (takes the global lock).
* `gix` (gitoxide) fast-path for read-only status at larger repo counts.
* Compact/summary output modes if real agent payloads prove too large despite byte caps.
* MCP server mode, if shell invocation proves limiting.
* Path-scoped `depends_on` filters (only treat upstream as stale if specific paths changed) — only if repo-level granularity proves too noisy in practice.

## 11. Effort & Risk Assessment

* **V1 estimate:** 2–3 weeks with TDD (porcelain-v2 parser, lock staleness, config validation, and integration tests against temp git repos are the test surface). V1 ships without any V2 machinery; the V2 config keys are additive and require no V1 rework.
* **V2 estimate:** +1–1.5 weeks. Toposort and cycle detection are trivial; the freshness state recording and `check-impact` execution path are the bulk.
* **Highest-confidence pieces:** `status`, `run`, `init-skill` — pure subprocess + parsing.
* **Riskiest V1 piece:** `pull` edge cases (upstream config variance, fetch failures vs merge failures) — mitigated by ff-only semantics and the explicit per-repo failure enum.
* **Riskiest V2 piece:** freshness-record correctness under concurrent sessions — mitigated by per-repo atomic state files and the fail-safe staleness direction (§9.3).
* **Deliberately rejected complexity:** libgit2, token-budget heuristics, global-only locking, implicit upstream auto-builds, mtime freshness, file-level impact analysis.

# ezgitx

**An agent-native multi-repo git CLI.** Read state, pull updates, and run
commands across many sibling git repositories concurrently — with output
designed for AI coding agents, not humans.

What makes it different from `mani`, `gita`, `myrepos`, or `git-xargs` is the
**agent I/O contract**:

- **JSONL on stdout by default** — one JSON object per repo, streamed as each
  repo completes. `--human` opts into tables; there is no format-flag zoo.
- **Zero interactivity** — never prompts (`GIT_TERMINAL_PROMPT=0` on every
  child git process); anything that would need input fails instantly with a
  structured error.
- **Deterministic truncation** — every free-text field is byte-capped
  (default 2 KB) with a `"truncated": true` marker and a `--max-bytes`
  override. No token-budget heuristics.
- **Stable contracts** — fixed error-code enum, fixed exit codes
  (0 ok / 1 repo failure / 2 usage or config / 3 lock contention). The output
  *is* the API: breaking a schema is a major version.
- **Cross-repo dependency awareness** — a declared dependency DAG, commit-hash
  freshness tracking, `run --with-deps`, and `check-impact` eliminate agent
  blindness to upstream changes.
- **Self-installing agent instructions** — `ezgitx init-skill` generates a
  Claude Code skill that teaches agents the contract.

## Install

```sh
cargo install ezgitx
```

Requires the system `git` binary. macOS and Linux; Rust 1.85+ to build.

## Setup

Put `.ezgitx.yml` at your workspace root (the directory containing your
repos):

```yaml
version: 1
groups:
  saas-core:
    - path: ./hipster
      default_cmd: "bun run build"          # used by `run` with no command
      check_cmd: "bun run typecheck"        # used by `check-impact --check`
      depends_on: ["spider"]                 # upstream repos, by directory name
    - path: ./billing
  data-pipelines:
    - path: ./spider
      default_cmd: "cargo build --release"
```

Then, if agents work in this workspace:

```sh
ezgitx init-skill   # writes .claude/skills/ezgitx/SKILL.md
```

## Commands

```sh
ezgitx status                  # working-tree + sync state per repo (never fetches)
ezgitx pull                    # concurrent fetch + ff-only merge (never merge commits)
ezgitx run "cargo test"        # run a command in each repo, in parallel
ezgitx run                     # run each repo's default_cmd
ezgitx run --with-deps         # build stale upstream deps first, in dependency order
ezgitx check-impact            # what's downstream of the current repo?
ezgitx check-impact --check    # ...and run each affected repo's check_cmd
ezgitx init-skill              # generate the agent-facing skill file
```

Every command works from anywhere inside the workspace (discovery walks
upward to `.ezgitx.yml`) and accepts targeting flags: `--all`,
`--group <name>`, `--repo <name>`, `--dirty`. With no flags you get all repos
at the workspace root, or just the enclosing repo when inside one.

Example output (`ezgitx pull`):

```jsonl
{"repo":"billing","status":"up_to_date","commits_pulled":0,"head":"a1b2c3d"}
{"repo":"hipster","status":"updated","commits_pulled":3,"head":"9f8e7d6"}
{"repo":"spider","status":"skipped_dirty","commits_pulled":0,"head":"4c5d6e7","error":{"code":"dirty_tree","message":"uncommitted changes block ff-only pull","snippet":"1 .M N... ... src/main.rs"}}
```

## Concurrency & locking

Operations run in parallel (bounded by `--jobs`, default = logical CPUs);
results stream as repos finish. Multiple agent sessions can work in the same
workspace concurrently: `pull` takes per-repo advisory locks
(`.ezgitx/locks/`), `run` takes none. Lock contention fails instantly with
exit code 3; `--wait <secs>` opts into bounded blocking. Stale locks (dead
process, or older than 10 minutes) are broken automatically.

## Versioning

The JSONL schemas, error-code enum, and exit-code contract are the public
interface. Breaking any of them requires a major version; new fields and new
error codes are additive (minor). Consumers must tolerate unknown fields.

MSRV is 1.85, verified in CI; MSRV bumps are minor-version changes, noted in
the changelog.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at
your option.

# ezgitx

**An agent-native multi-repo git CLI.** Read state, pull updates, and run
commands across many sibling git repositories concurrently — with output
designed for AI coding agents, not humans.

## What is this, in plain words?

If your work lives in several project folders side by side — say a frontend,
a backend, and a shared library, each its own git repo — you've probably
watched your AI coding agent struggle with that. It pulls repos one at a
time, builds things in the wrong order, and has no idea that the change it
just made to your shared library breaks the app sitting right next to it.

ezgitx is a small command-line tool that fixes exactly that. It gives your
agent (and you) one clean way to:

- **see** the state of every repo at once — `ezgitx status`
- **update** them all in one go — `ezgitx pull`
- **install/build/test across all of them**, in parallel, in the right
  order — `ezgitx run --all --with-deps`
- **know what breaks what** — change a shared library, then
  `ezgitx check-impact` lists everything downstream that needs re-checking

And it's built *for* agents: it never stops to ask questions, its output is
machine-readable, and it can install its own instructions into your
workspace (`ezgitx init-skill`) so agents like Claude Code discover and use
it on their own — you never have to explain it to them.

### The problem it solves

AI agents are great inside one repo. The moment your work spans several
sibling repos, they go blind: they can't tell which repos changed upstream,
which builds are stale, or what depends on what. You end up burning time and
tokens watching the agent rediscover your workspace every session — or
debugging a failure that was really just a stale build of a sibling repo it
didn't know about. ezgitx turns your workspace's structure into something an
agent can simply read.

### Get started in two minutes

1. **Install it** (needs Rust's `cargo` and `git`):

   ```sh
   cargo install --git https://github.com/yuval-r/ezgitx
   # (cargo install ezgitx — once the crate is published to crates.io)
   ```

2. **Let your agent set it up.** Open a Claude Code session in the folder
   that *contains* your repos and paste the prompt from
   [Quick start](#quick-start-let-your-coding-agent-generate-the-config)
   below. The agent writes the config, detects the dependencies between your
   repos, and installs its own instructions.

3. **Talk naturally from then on.** In any session in that workspace:
   *"pull everything"*, *"build the workspace"*, *"I changed the shared
   lib — what do I need to re-test?"* — the agent reaches for ezgitx on its
   own.

---

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
cargo install --git https://github.com/yuval-r/ezgitx
# (cargo install ezgitx — once the crate is published to crates.io)
```

Requires the system `git` binary. macOS and Linux; Rust 1.85+ to build.

## Setup

### Quick start: let your coding agent generate the config

The fastest way to adopt ezgitx — including dependency detection — is to let
a coding agent build `.ezgitx.yml` for you. Start a Claude Code (or similar)
session **at your workspace root** (the directory containing your repos) and
paste:

```text
I'm adopting ezgitx (an agent-native multi-repo CLI) in this workspace.
Generate .ezgitx.yml at the workspace root. Work evidence-first:

1. SURVEY — every direct subdirectory that is a git repository is a candidate
   repo; its directory name becomes its ezgitx name.
2. COMMANDS — read each repo's real build manifests (package.json scripts,
   Cargo.toml, pyproject.toml, Makefile, go.mod) and derive:
   - default_cmd: the real install+build command. Check the lockfile to pick
     the right tool (npm vs pnpm vs bun vs yarn). Don't invent script names.
   - check_cmd: the fastest meaningful verification (typecheck, lint, or a
     quick test target) if one exists; omit the key otherwise.
3. GROUPS — group repos the way I'd target them together (toolchain or
   domain). Groups may overlap; entries for the same repo merge, but
   conflicting field values are a config error, so define commands once.
4. DEPENDENCIES — declare depends_on ONLY where you find concrete evidence
   that one repo consumes another FROM THIS WORKSPACE (path dependencies,
   workspace references, file:/link: specifiers, cross-repo relative
   imports) — not merely a shared dependency from a public registry. List
   the edges you considered but rejected so I can promote any I want
   tracked anyway. The graph must be a DAG.
5. SCHEMA — top level is `version: 1` (integer) plus `groups:`, a mapping
   of group name to a LIST of repo entries. Per-repo keys are exactly:
   path (string, required, relative to the workspace root), default_cmd
   (string, optional), check_cmd (string, optional), depends_on (list of
   strings — repo directory names — optional). Unknown keys are rejected
   at load.
6. VALIDATE — run `ezgitx status` (config errors and dependency cycles fail
   loudly with exit 2), then `ezgitx run --all "git rev-parse --short HEAD"`
   as a cheap dry-run proving every repo resolves. Then run
   `ezgitx init-skill`.
7. REPORT — show me the final config with one line of justification per
   command and per dependency edge.
```

The agent surveys the repos, derives real build commands from the manifests,
detects workspace-local dependency edges, validates the result against the
binary, and installs the agent-facing skill — one paste, done.

### Manual setup

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

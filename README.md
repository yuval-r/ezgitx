# ezgitx

[![crates.io](https://img.shields.io/crates/v/ezgitx.svg)](https://crates.io/crates/ezgitx)
[![CI](https://github.com/yuval-r/ezgitx/actions/workflows/ci.yml/badge.svg)](https://github.com/yuval-r/ezgitx/actions/workflows/ci.yml)
[![license](https://img.shields.io/crates/l/ezgitx.svg)](#license)

**An agent-native multi-repo git CLI.** Read state, pull updates, and run
commands across many sibling git repositories at once, with output designed
for AI coding agents rather than humans.

## What is this?

Say your work lives in a few project folders side by side: a frontend, a
backend, a shared library. Each one is its own git repo. AI coding agents
handle that layout badly. They pull repos one at a time and build things in
the wrong order. Worse, they don't notice that the change they just made to
the shared library breaks the app sitting right next to it.

ezgitx is a small command-line tool that fixes this. It gives your agent
(and you) one way to:

- see the state of every repo at once (`ezgitx status`)
- update them all in one go (`ezgitx pull`)
- build or test everything in parallel, in dependency order
  (`ezgitx run --all --with-deps`)
- find out what breaks what: change a shared library and
  `ezgitx check-impact` lists everything downstream that needs re-checking

It never stops to ask questions, its output is machine readable, and it can
install its own instructions into your workspace (`ezgitx init-skill`) so
agents like Claude Code find it and use it without being told.

### The problem it solves

Agents are fine inside one repo. Across several sibling repos they go blind.
They can't tell which repos changed upstream, which builds are stale, or
what depends on what. So you burn time (and tokens) watching the agent
rediscover your workspace every session. Or you debug a failure that turns
out to be nothing more than a stale build of a repo the agent didn't know
mattered. ezgitx turns the structure of your workspace into something an
agent can just read.

### Get started in two minutes

1. Install it (no Rust toolchain needed):

   ```sh
   curl -LsSf https://github.com/yuval-r/ezgitx/releases/latest/download/ezgitx-installer.sh | sh
   ```

   Or, if you have cargo: `cargo install ezgitx`

2. Set it up. Walk through the [Quickstart](#quickstart) below, or let your
   agent write the config: open a Claude Code session in the folder that
   *contains* your repos and paste the
   [setup prompt](#or-let-your-agent-write-the-config).

3. From then on, just talk: "pull everything", "build the workspace", "I
   changed the shared lib, what do I need to re-test?". The agent reaches
   for ezgitx on its own.

---

What makes it different from `mani`, `gita`, `myrepos`, or `git-xargs` is
the agent I/O contract:

- JSONL on stdout by default: one JSON object per repo, streamed as each
  repo completes. `--human` opts into tables. There is no format-flag zoo.
- Zero interactivity. It never prompts (`GIT_TERMINAL_PROMPT=0` on every
  child git process); anything that would need input fails instantly with a
  structured error.
- Deterministic truncation. Every free-text field is byte-capped (default
  2 KB) with a `"truncated": true` marker and a `--max-bytes` override. No
  token-budget heuristics.
- Stable contracts: a fixed error-code enum and fixed exit codes
  (0 ok / 1 repo failure / 2 usage or config / 3 lock contention). The
  output is the API; breaking a schema means a major version.
- Cross-repo dependency awareness: a declared dependency DAG, commit-hash
  freshness tracking, `run --with-deps`, and `check-impact`, so agents
  aren't blind to upstream changes.
- Self-installing agent instructions: `ezgitx init-skill` generates a
  Claude Code skill that teaches agents the contract.

## Install

Prebuilt binaries (macOS and Linux, both architectures), no Rust needed:

```sh
curl -LsSf https://github.com/yuval-r/ezgitx/releases/latest/download/ezgitx-installer.sh | sh
```

With cargo:

```sh
cargo install ezgitx
# or build the latest main:
cargo install --git https://github.com/yuval-r/ezgitx
```

Requires the system `git` binary. macOS and Linux; Rust 1.85+ to build from
source.

## Quickstart

Up and running in about a minute. Say your repos sit side by side in one
folder:

```sh
$ cd ~/my-workspace
$ ls
backend  frontend  shared
```

Install ezgitx:

```sh
$ cargo install ezgitx
```

Add a `.ezgitx.yml` in that folder describing your repos. Here `backend` and
`frontend` both build on top of `shared`, so they declare it with
`depends_on`:

```yaml
version: 1
groups:
  app:
    - path: ./shared
      default_cmd: "npm install && npm run build"
    - path: ./backend
      default_cmd: "npm install && npm run build"
      depends_on: ["shared"]
    - path: ./frontend
      default_cmd: "npm install && npm run build"
      depends_on: ["shared"]
```

See the state of every repo at once, one JSON line each:

```sh
$ ezgitx status
{"repo":"backend","path":"/Users/you/my-workspace/backend","branch":"main","head":"a1b2c3d","state":"clean","ahead":0,"behind":0,"stale_deps":["shared"]}
{"repo":"frontend","path":"/Users/you/my-workspace/frontend","branch":"main","head":"9f8e7d6","state":"dirty","ahead":0,"behind":0,"stale_deps":["shared"]}
{"repo":"shared","path":"/Users/you/my-workspace/shared","branch":"main","head":"4c5d6e7","state":"clean","ahead":0,"behind":0}
```

Pull all of them (fetch plus fast-forward, never a merge commit):

```sh
$ ezgitx pull
{"repo":"backend","status":"up_to_date","commits_pulled":0,"head":"a1b2c3d"}
{"repo":"frontend","status":"updated","commits_pulled":2,"head":"7e8f9a0"}
{"repo":"shared","status":"up_to_date","commits_pulled":0,"head":"4c5d6e7"}
```

Build everything in dependency order. `shared` builds first, then the two
repos that depend on it, with a summary line at the end:

```sh
$ ezgitx run --all --with-deps
{"repo":"shared","exit_code":0,"duration_ms":3412,"stdout_tail":"...","stderr_tail":"","truncated":false}
{"repo":"backend","exit_code":0,"duration_ms":5104,"stdout_tail":"...","stderr_tail":"","truncated":false}
{"repo":"frontend","exit_code":0,"duration_ms":4880,"stdout_tail":"...","stderr_tail":"","truncated":false}
{"type":"summary","total":3,"passed":3,"failed":0,"duration_ms":9220}
```

Last step, teach your AI agent about the tool:

```sh
$ ezgitx init-skill
{"path":"/Users/you/my-workspace/.claude/skills/ezgitx/SKILL.md","status":"written"}
```

That's it. From now on a fresh Claude Code session in this folder finds the
skill and runs ezgitx on its own. You never have to explain your layout
again.

### Or let your agent write the config

Don't want to write the YAML by hand? Start a Claude Code (or similar)
session **at your workspace root** (the directory containing your repos) and
paste this. The agent reads your repos, works out the build commands and
dependencies, and writes `.ezgitx.yml` for you:

```text
I'm adopting ezgitx (an agent-native multi-repo CLI,
https://github.com/yuval-r/ezgitx) in this workspace. Generate .ezgitx.yml
at the workspace root. Work evidence-first:

1. SURVEY: every direct subdirectory that is a git repository is a candidate
   repo; its directory name becomes its ezgitx name.
2. COMMANDS: read each repo's real build manifests (package.json scripts,
   Cargo.toml, pyproject.toml, Makefile, go.mod) and derive:
   - default_cmd: the real install+build command. Check the lockfile to pick
     the right tool (npm vs pnpm vs bun vs yarn). Don't invent script names.
   - check_cmd: the fastest meaningful verification (typecheck, lint, or a
     quick test target) if one exists; omit the key otherwise.
3. GROUPS: group repos the way I'd target them together (toolchain or
   domain). Groups may overlap; entries for the same repo merge, but
   conflicting field values are a config error, so define commands once.
4. DEPENDENCIES: declare depends_on ONLY where you find concrete evidence
   that one repo consumes another FROM THIS WORKSPACE (path dependencies,
   workspace references, file:/link: specifiers, cross-repo relative
   imports), not merely a shared dependency from a public registry. List
   the edges you considered but rejected so I can promote any I want
   tracked anyway. The graph must be a DAG.
5. SCHEMA: top level is `version: 1` (integer) plus `groups:`, a mapping
   of group name to a LIST of repo entries. Per-repo keys are exactly:
   path (string, required, relative to the workspace root), default_cmd
   (string, optional), check_cmd (string, optional), depends_on (list of
   strings, repo directory names, optional). Unknown keys are rejected
   at load.
6. VALIDATE: run `ezgitx status` (config errors and dependency cycles fail
   loudly with exit 2), then `ezgitx run --all "git rev-parse --short HEAD"`
   as a cheap dry-run proving every repo resolves. Then run
   `ezgitx init-skill`.
7. REPORT: show me the final config with one line of justification per
   command and per dependency edge.
```

One paste. The agent does the survey, writes the config, validates it
against the binary, and installs the skill.

### Config reference

A repo can belong to more than one group, and most keys are optional. This
fuller example uses two groups, a `check_cmd`, and a `depends_on` edge:

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

## Examples

For a complete, runnable walkthrough, see [EXAMPLES.md](EXAMPLES.md): it clones
a five-repo workspace, builds it in dependency order, then changes one library
and watches ezgitx flag and rebuild only what's downstream. It frames the
`aiohttp` library stack as a stand-in for a private multi-repo project, so you
can run the whole thing yourself.

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

## Why this exists

I'm Yuval Roth. I'm building [EZBunny](https://ezbunny.com), AI-native
compliance training for small healthcare practices, and its workspace is a
pile of sibling repos that my AI coding agents kept mishandling: wrong build
order, stale shared libraries, no idea what depended on what. ezgitx is the
tool I built so they'd stop. It's open source because the problem clearly
isn't mine alone.

Found it useful? A star helps other people find it. I'm
[@yuval-r](https://github.com/yuval-r) on GitHub.

## Feedback

Found a bug, want a feature, or hit a workspace layout that trips it up? Open
an issue: https://github.com/yuval-r/ezgitx/issues. Pull requests are welcome
too. If something in this README was confusing, that counts as a bug, so tell
me.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at
your option.

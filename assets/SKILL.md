---
name: ezgitx
description: Multi-repo git operations across this workspace. Use when catching up at the start of a session on what changed since you last looked, reading repo status, pulling updates, running commands across sibling repos, checking the cross-repo impact of a change, gating a multi-repo change with a cross-repo definition-of-done check, or seeing which sessions hold repo locks.
---

# ezgitx — multi-repo operations for this workspace

`ezgitx` operates on the git repos defined in `.ezgitx.yml` at the workspace
root. It works from any directory inside the workspace, including from deep
inside a single repo.

## Output contract

- Output is **JSONL on stdout by default** — one JSON object per line, one
  line per repo, streamed as each repo completes. Never pass format flags;
  there are none. (`--human` exists for humans; do not use it.)
- Progress and logs go to stderr; only parseable results go to stdout.
- Tolerate unknown fields: new fields are added in minor versions.
- Free-text fields (`stdout_tail`, `stderr_tail`, `error.snippet`) are
  byte-capped (default 2 KB) with `"truncated": true` when capped. Read
  `error.snippet` instead of re-running commands for full logs. If you truly
  need more context, increase the cap with `--max-bytes <N>`.

## Exit codes

| Code | Meaning | What to do |
|---|---|---|
| 0 | all targeted repos succeeded | proceed |
| 1 | ≥ 1 repo-level operation failed (others may have succeeded) | read the per-repo `error` objects |
| 2 | usage or config error (bad flags, invalid `.ezgitx.yml`, dependency cycle) | fix the invocation or config |
| 3 | lock contention | wait briefly and retry, or pass `--wait <secs>` |

Per-repo errors are structured: `{"repo": "...", "error": {"code": "...",
"message": "...", "snippet": "..."}}`. Stable codes: `dirty_tree`,
`diverged`, `detached`, `lock_held`, `not_a_repo`, `no_default_cmd`,
`git_failed`, `spawn_failed`, `config_invalid`, `dependency_cycle`,
`upstream_failed`.

## Targeting

Target narrowly instead of scanning everything:

- `--repo <name>` — one repo by directory name (repeatable)
- `--group <name>` — a configured group (repeatable)
- `--dirty` — filter the selection to repos with uncommitted changes
- no flag: all repos when at the workspace root, the current repo when inside one

## Commands

- `ezgitx brief` — **run this first, at the start of a session.** A snapshot of
  every repo (same fields as `status`) plus, per repo, `new_commits` (count) and
  a `commits` list of `{sha, subject}` (newest-first, capped) added locally since
  you last ran `brief`. Offline; never fetches. The first run in a workspace has
  no delta (it just records a baseline); later runs show only what's newer. Pass
  `--no-record` to peek without advancing that baseline. With `--dirty`, only the
  repos it shows advance their baseline, so commits in a clean repo it filtered
  out still surface in the next unfiltered `brief` (never silently skipped). Ends
  with a `{"type": "summary", ...}` line. Prefer this over re-scanning the
  workspace by hand each session.
- `ezgitx changed --since <ref|last-brief>` — the standalone delta: per repo, the
  `files` ({status `A`/`M`/`D`/`R`, path}) and `commits` ({sha, subject}) that moved
  since `<ref>` (any git ref) or, by default, since your last `brief`. Both lists are
  capped (`truncated` flags it). Offline. A repo where the ref or baseline is missing
  degrades to a `delta_unavailable` reason (`no_baseline` / `ref_not_found` /
  `baseline_unreachable` / `unborn`) at exit 0, not a failure, so a ref only some repos
  share still works. Ends with a `{"type": "summary", ...}` line.
- `ezgitx status` — working-tree + sync state per repo. Never fetches;
  `ahead`/`behind` reflect the last fetch. `state` is `clean` | `dirty` |
  `detached` | `conflicted`. Each repo also carries `build`: `fresh` | `stale`.
  `stale` when there is no recorded green build, HEAD moved since one, or an
  upstream drifted (the same freshness model surfaced by `stale_deps`).
- `ezgitx pull` — concurrent fetch + ff-only merge. Never creates merge
  commits. Dirty repos are reported as `skipped_dirty` (the fetch still ran),
  diverged branches as `diverged` — resolve those manually.
- `ezgitx run "<cmd>"` — run a shell command in each target repo in
  parallel. With no command, each repo's configured `default_cmd` runs.
  Ends with a `{"type": "summary", ...}` line. `run` takes **no locks** —
  it is safe to run concurrently with other sessions, including pulls.
- `ezgitx run --with-deps` — dependency-ordered execution: stale upstream
  dependencies build first (their `default_cmd`), in topological order,
  then your targets. **Prefer this over manually sequencing builds across
  repos** — e.g. `ezgitx run --all --with-deps` builds the whole workspace
  in the right order in one command. Fresh upstreams are skipped.
- `ezgitx run --with-dependents` — the forward counterpart: rebuild the
  targets plus every **dependent** (downstream) repo that is stale against
  them, in dependency order. Use after changing a shared/upstream repo to
  propagate the change to everything built on top of it. Unlike
  `check-impact --check` (which runs `check_cmd` as a dry run and records
  nothing), this runs each repo's `default_cmd` and updates freshness.
- `ezgitx check-impact` — list downstream repos affected by changes in the
  current (or `--repo <name>`) repo, with `depth` and dependency path `via`.
  Add `--check` to also run each affected repo's check command in
  dependency order.
- `ezgitx verify`: the cross-repo **definition-of-done gate**. Scans the whole
  workspace for uncommitted changes, then runs each repo's `check_cmd` (fallback
  `default_cmd`) across every dirty repo **plus everything downstream of a dirty
  repo**, in dependency order. Streams the per-repo run lines, then one verdict
  line `{"type": "verdict", "verdict": "pass"|"fail", "checked": N, "failed":
  [...]}`. Exit 0 only when every checked repo passed. **Run this before claiming
  a multi-repo change is done**: it catches the downstream repo a change quietly
  broke. It is a gate, not a build: it records no freshness.
- `ezgitx sessions`: read-only list of **active advisory locks** under
  `.ezgitx/locks/`: one line per live lock with `scope` (`repo` | `workspace`),
  `repo` (for repo locks), `pid`, `host`, `op`, and `since`. Use it in a
  multi-agent workspace to see who is touching what. Stale locks (dead process or
  expired TTL) are omitted; it never breaks them.

## Cross-repo dependency workflow

- After modifying a shared/upstream repo, run `ezgitx check-impact` to see
  what is affected. To actually rebuild that downstream set, use
  `ezgitx run --repo <changed> --with-dependents`; to only validate it, use
  `check-impact --check`.
- `status` reports `stale_deps` per repo: upstream dependencies sitting at a
  commit other than the one **this** repo was last built against through
  `ezgitx run`. The flag is per-consumer — rebuilding a shared upstream for
  one repo does not clear it on another repo that has not itself been rebuilt.
- To build a target with its prerequisites, `ezgitx run --with-deps` rebuilds
  stale upstreams first, in dependency order, then the target. `--with-deps`
  (upstreams) and `--with-dependents` (downstreams) may be combined.
- Staleness only ever errs toward redundant rebuilds, never toward stale
  artifacts being treated as fresh.

## Creating or extending `.ezgitx.yml`

When asked to add repos to this workspace or rebuild the config, work
evidence-first:

- Candidate repos are direct subdirectories of the workspace root that
  contain `.git`; the directory name becomes the repo name.
- Derive `default_cmd` from the repo's real manifests (package.json scripts,
  Cargo.toml, pyproject.toml, Makefile, go.mod). Check the lockfile to pick
  the right tool (npm vs pnpm vs bun vs yarn). Never invent script names.
- `check_cmd` is the fastest meaningful verification (typecheck, lint, or a
  quick test target); omit the key when none exists.
- Groups may overlap. Entries for the same repo merge across groups, but
  conflicting field values are a `config_invalid` error — define each repo's
  commands in one place.
- Declare `depends_on` ONLY with evidence that the repo consumes a sibling
  FROM THIS WORKSPACE: path dependencies, workspace references, `file:` /
  `link:` specifiers, relative imports across repo boundaries. Both repos
  merely depending on the same published registry package is NOT an edge.
  The graph must be a DAG — cycles fail at config load with exit 2.
- Validate before declaring done: `ezgitx status` (config errors exit 2),
  then a cheap dry-run `ezgitx run --all "git rev-parse --short HEAD"` to
  prove every repo resolves and executes. (`init-skill` writes a static
  template — config changes don't alter it; re-run it only after upgrading
  ezgitx itself.)

## Locking

`pull` takes per-repo locks; unrelated repos are never serialized. On exit
code 3 (`lock_held`), another session holds the lock: wait a few seconds and
retry, or pass `--wait <secs>` to block boundedly. Stale locks (dead process
or older than 10 minutes) are broken automatically.

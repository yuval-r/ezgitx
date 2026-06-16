#!/usr/bin/env bash
#
# End-to-end test for `ezgitx brief`, run against the real built binary.
#
# Unlike `cargo test` (which exercises units and per-command cases), this drives
# a full agent-session narrative across a multi-repo workspace with a dependency
# graph and a real (file-based) remote: first brief, local commits, pull, run,
# peek, plus every degrade path. It asserts on the JSONL contract and exit codes.
#
# Usage:
#   bash scripts/e2e-brief.sh            # builds target/debug/ezgitx, then runs
#   EZGITX_BIN=/path/to/ezgitx bash scripts/e2e-brief.sh   # use a prebuilt binary
#
# Requires: bash, git, python3 (for JSON assertions). Offline; no network.
# Exit status: 0 if all assertions pass, 1 otherwise.

set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

command -v git >/dev/null   || { echo "FATAL: git not found"; exit 1; }
command -v python3 >/dev/null || { echo "FATAL: python3 not found (needed for JSON assertions)"; exit 1; }

# --- locate / build the binary ------------------------------------------------
BIN="${EZGITX_BIN:-$REPO_ROOT/target/debug/ezgitx}"
if [ -z "${EZGITX_BIN:-}" ]; then
  echo "building ezgitx (debug)…"
  ( cd "$REPO_ROOT" && cargo build -q ) || { echo "FATAL: cargo build failed"; exit 1; }
fi
[ -x "$BIN" ] || { echo "FATAL: binary not executable: $BIN"; exit 1; }

# --- isolated, deterministic git environment ---------------------------------
export GIT_AUTHOR_NAME=e2e GIT_AUTHOR_EMAIL=e2e@example.com
export GIT_COMMITTER_NAME=e2e GIT_COMMITTER_EMAIL=e2e@example.com
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null
export GIT_TERMINAL_PROMPT=0 NO_COLOR=1

# --- scratch workspaces, cleaned on exit -------------------------------------
TMPDIRS=()
cleanup() { for d in "${TMPDIRS[@]:-}"; do [ -n "$d" ] && rm -rf "$d"; done; }
trap cleanup EXIT
new_ws() { local d; d="$(mktemp -d)"; TMPDIRS+=("$d"); echo "$d"; }

# --- assertion harness -------------------------------------------------------
PASS=0; FAIL=0
OUT=""; ERR=""; RC=0
ok() { PASS=$((PASS+1)); printf '  PASS  %s\n' "$1"; }
ko() {
  FAIL=$((FAIL+1)); printf '  FAIL  %s\n' "$1"
  [ -n "${2:-}" ] && printf '        %s\n' "$2"
  [ -n "${3:-}" ] && { printf '        --- output ---\n'; sed 's/^/        /' "$3"; }
}
eq() { # actual expected label
  if [ "$1" = "$2" ]; then ok "$3"; else ko "$3" "got [$1] want [$2]" "$OUT"; fi
}
exits() { eq "$RC" "$1" "$2"; }
contains() { if grep -qF "$2" "$1"; then ok "$3"; else ko "$3" "missing: $2" "$1"; fi; }
absent()  { if grep -qF "$2" "$1"; then ko "$3" "unexpected: $2" "$1"; else ok "$3"; fi; }

# Run the binary, capturing stdout in $OUT, stderr in $ERR, exit code in $RC.
ezg() { "$BIN" "$@" >"$OUT" 2>"$ERR"; RC=$?; }

# JSON field of a repo line (prints __MISSING__ if the key is absent, __NOREPO__
# if the repo has no line).
field() {
  python3 - "$OUT" "$1" "$2" <<'PY'
import json,sys
path,repo,key=sys.argv[1],sys.argv[2],sys.argv[3]
for ln in open(path):
    ln=ln.strip()
    if not ln: continue
    try: o=json.loads(ln)
    except Exception: continue
    if o.get("repo")==repo:
        if key not in o: print("__MISSING__")
        else:
            v=o[key]; print(v if isinstance(v,str) else json.dumps(v))
        break
else:
    print("__NOREPO__")
PY
}
sfield() { # summary field
  python3 - "$OUT" "$1" <<'PY'
import json,sys
path,key=sys.argv[1],sys.argv[2]
for ln in open(path):
    ln=ln.strip()
    if not ln: continue
    o=json.loads(ln)
    if o.get("type")=="summary": print(json.dumps(o.get(key))); break
PY
}
subjects() { # newline-joined commit subjects for a repo, in emitted order
  python3 - "$OUT" "$1" <<'PY'
import json,sys
path,repo=sys.argv[1],sys.argv[2]
for ln in open(path):
    ln=ln.strip()
    if not ln: continue
    o=json.loads(ln)
    if o.get("repo")==repo:
        print("\n".join(c["subject"] for c in o.get("commits",[]))); break
PY
}

# --- workspace builders ------------------------------------------------------
WS=""; OUT=""; ERR=""
init_main_ws() {
  WS="$(new_ws)"; OUT="$WS/.stdout"; ERR="$WS/.stderr"
  mk_local "$WS" libcore
  mk_remote "$WS" api          # api has a real origin so we can pull into it
  mk_local "$WS" web
  cat > "$WS/.ezgitx.yml" <<YML
version: 1
groups:
  g:
    - path: ./libcore
      default_cmd: "true"
    - path: ./api
      default_cmd: "true"
      depends_on: ["libcore"]
    - path: ./web
      default_cmd: "true"
YML
}
mk_local() { # ws name
  local ws="$1" name="$2"
  mkdir -p "$ws/$name"; git -C "$ws/$name" init -q -b main
  echo init > "$ws/$name/README.md"; git -C "$ws/$name" add .
  git -C "$ws/$name" commit -q -m "initial commit"
}
mk_remote() { # ws name  — bare origin + workspace clone (pull-capable)
  local ws="$1" name="$2"
  local bare="$ws/.remotes/$name.git" writer="$ws/.writers/$name"
  mkdir -p "$bare"; git -C "$bare" init -q --bare -b main
  mkdir -p "$writer"; git -C "$writer" init -q -b main
  git -C "$writer" remote add origin "$bare"
  echo init > "$writer/README.md"; git -C "$writer" add .
  git -C "$writer" commit -q -m "initial commit"
  git -C "$writer" push -q -u origin main
  git -C "$ws" clone -q "$bare" "$ws/$name"
}
commit_in()    { local ws="$1" n="$2" f="$3" m="$4"; echo "$RANDOM-$f" > "$ws/$n/$f"; git -C "$ws/$n" add .; git -C "$ws/$n" commit -q -m "$m"; }
push_upstream(){ local ws="$1" n="$2" f="$3" m="$4"; echo "$RANDOM-$f" > "$ws/.writers/$n/$f"; git -C "$ws/.writers/$n" add .; git -C "$ws/.writers/$n" commit -q -m "$m"; git -C "$ws/.writers/$n" push -q origin main; }

echo "=== ezgitx brief — end-to-end ==="
echo "binary: $BIN"

# =============================================================================
echo; echo "## Phase 1 — first brief in a fresh workspace (baseline, no delta)"
init_main_ws
( cd "$WS" && "$BIN" brief >"$OUT" 2>"$ERR" ); RC=$?
exits 0 "first brief exits 0"
eq "$(field libcore new_commits)" "__MISSING__" "libcore: no delta on first run"
eq "$(field api new_commits)"     "__MISSING__" "api: no delta on first run"
eq "$(field api stale_deps)"      '["libcore"]' "api: stale_deps lists libcore (no build record yet)"
eq "$(sfield repos)" "3" "summary: repos=3"
eq "$(sfield with_new_commits)" "0" "summary: with_new_commits=0"
[ -f "$WS/.ezgitx/state/api.brief.json" ] && [ -f "$WS/.ezgitx/state/web.brief.json" ] \
  && ok "baselines written for every repo" || ko "baselines written for every repo" "missing .brief.json"

# =============================================================================
echo; echo "## Phase 2 — local commits surface as a newest-first delta"
commit_in "$WS" libcore one.rs "libcore change one"
commit_in "$WS" libcore two.rs "libcore change two"
( cd "$WS" && "$BIN" brief >"$OUT" 2>"$ERR" ); RC=$?
exits 0 "brief exits 0"
eq "$(field libcore new_commits)" "2" "libcore: 2 new commits"
eq "$(field libcore truncated)" "false" "libcore: not truncated"
eq "$(subjects libcore)" $'libcore change two\nlibcore change one' "libcore: subjects newest-first"
eq "$(field web new_commits)" "0" "web: 0 new commits"
eq "$(sfield with_new_commits)" "1" "summary: with_new_commits=1"

# =============================================================================
echo; echo "## Phase 3 — pull brings in an upstream commit; brief reports it"
# Phase-2 brief advanced api's baseline to its current HEAD. A pull then moves
# api's HEAD forward, and the next brief shows exactly that delta.
push_upstream "$WS" api upstream.rs "upstream api fix"
( cd "$WS" && "$BIN" pull --repo api >"$OUT" 2>"$ERR" ); RC=$?
exits 0 "pull --repo api exits 0"
( cd "$WS" && "$BIN" brief --repo api >"$OUT" 2>"$ERR" ); RC=$?
exits 0 "brief --repo api exits 0"
eq "$(field api new_commits)" "1" "api: the pulled commit shows as 1 new commit"
eq "$(subjects api)" "upstream api fix" "api: delta subject is the pulled commit"

# =============================================================================
echo; echo "## Phase 4 — brief composes with run (no state-file contention)"
# `run` writes freshness records (<repo>.json); brief uses <repo>.brief.json.
# They must not interfere.
( cd "$WS" && "$BIN" run --all --with-deps >"$OUT" 2>"$ERR" ); RC=$?
exits 0 "run --all --with-deps exits 0"
( cd "$WS" && "$BIN" brief --repo api >"$OUT" 2>"$ERR" ); RC=$?
exits 0 "brief after run exits 0"
eq "$(field api new_commits)" "0" "api: run did not move HEAD → 0 new"
[ -f "$WS/.ezgitx/state/api.json" ] && [ -f "$WS/.ezgitx/state/api.brief.json" ] \
  && ok "freshness record and brief baseline coexist" || ko "freshness record and brief baseline coexist"

# =============================================================================
echo; echo "## Phase 5 — --no-record peeks without advancing the baseline"
commit_in "$WS" web feat.js "web local work"
( cd "$WS" && "$BIN" brief --repo web --no-record >"$OUT" 2>"$ERR" ); RC=$?
eq "$(field web new_commits)" "1" "peek #1: shows 1 new"
( cd "$WS" && "$BIN" brief --repo web --no-record >"$OUT" 2>"$ERR" ); RC=$?
eq "$(field web new_commits)" "1" "peek #2: still 1 (baseline not advanced)"
( cd "$WS" && "$BIN" brief --repo web >"$OUT" 2>"$ERR" )   # advance
( cd "$WS" && "$BIN" brief --repo web >"$OUT" 2>"$ERR" ); RC=$?
eq "$(field web new_commits)" "0" "after a recording brief: 0 new"

# =============================================================================
echo; echo "## Phase 5b — --dirty only consumes (baselines) repos it displays"
# A clean repo hidden by --dirty must keep its baseline, so a commit made to it
# still surfaces in the next unfiltered brief (never silently swallowed).
DW="$(new_ws)"; mk_local "$DW" dirtyrepo; mk_local "$DW" cleanrepo
printf 'version: 1\ngroups:\n  g:\n    - path: ./dirtyrepo\n    - path: ./cleanrepo\n' > "$DW/.ezgitx.yml"
( cd "$DW" && "$BIN" brief >/dev/null 2>&1 )                 # baseline both (steady state)
commit_in "$DW" cleanrepo feat.txt "clean repo commit"      # commit in the clean repo
echo wip > "$DW/dirtyrepo/wip.txt"                          # make the other one dirty
( cd "$DW" && "$BIN" brief --dirty >"$OUT" 2>"$ERR" ); RC=$?
exits 0 "--dirty exits 0"
eq "$(field cleanrepo new_commits)" "__NOREPO__" "--dirty hides the clean repo"
( cd "$DW" && "$BIN" brief --repo cleanrepo >"$OUT" 2>"$ERR" ); RC=$?
eq "$(field cleanrepo new_commits)" "1" "hidden repo's commit still surfaces (not consumed)"

# =============================================================================
echo; echo "## Phase 6 — --human renders an aligned table, no JSON"
( cd "$WS" && "$BIN" brief --human >"$OUT" 2>"$ERR" ); RC=$?
exits 0 "brief --human exits 0"
contains "$OUT" "NEW" "human: NEW column present"
contains "$OUT" "REPO" "human: header present"
absent   "$OUT" "{" "human: no JSON braces"

# =============================================================================
echo; echo "## Phase 7 — degrade paths (all stay exit 0, no false failures)"
# detached HEAD still deltas
commit_in "$WS" libcore three.rs "libcore change three"
git -C "$WS/libcore" checkout -q --detach
( cd "$WS" && "$BIN" brief --repo libcore >"$OUT" 2>"$ERR" ); RC=$?
exits 0 "detached: exits 0"
eq "$(field libcore state)" "detached" "detached: state=detached"
eq "$(field libcore branch)" "null" "detached: branch=null"
git -C "$WS/libcore" checkout -q main

# unreachable baseline → degrade, then re-baseline
printf '{"head":"dddddddddddddddddddddddddddddddddddddddd","recorded_at":"t"}' > "$WS/.ezgitx/state/web.brief.json"
( cd "$WS" && "$BIN" brief --repo web >"$OUT" 2>"$ERR" ); RC=$?
exits 0 "unreachable baseline: exits 0"
eq "$(field web delta_unavailable)" "baseline_unreachable" "unreachable: delta_unavailable set"
eq "$(field web new_commits)" "__MISSING__" "unreachable: no new_commits field"
( cd "$WS" && "$BIN" brief --repo web >"$OUT" 2>"$ERR" ); RC=$?
eq "$(field web new_commits)" "0" "unreachable: re-baselined, next run normal"

# unborn HEAD (git init, no commit) — separate workspace
UB="$(new_ws)"; mkdir -p "$UB/fresh"; git -C "$UB/fresh" init -q -b main
printf 'version: 1\ngroups:\n  g:\n    - path: ./fresh\n' > "$UB/.ezgitx.yml"
( cd "$UB" && "$BIN" brief >"$OUT" 2>"$ERR" ); RC=$?
exits 0 "unborn: exits 0"
eq "$(field fresh head)" "(initial)" "unborn: head=(initial)"
eq "$(field fresh new_commits)" "__MISSING__" "unborn: no delta"
[ -e "$UB/.ezgitx/state/fresh.brief.json" ] && ko "unborn: nothing recorded" "baseline was written" \
  || ok "unborn: nothing recorded (no baseline)"

# not-a-repo → per-repo error, exit 1
NR="$(new_ws)"; mk_local "$NR" good; mkdir -p "$NR/bad"
printf 'version: 1\ngroups:\n  g:\n    - path: ./good\n    - path: ./bad\n' > "$NR/.ezgitx.yml"
( cd "$NR" && "$BIN" brief >"$OUT" 2>"$ERR" ); RC=$?
exits 1 "not-a-repo: exit 1"
contains "$OUT" '"error":{"code":"not_a_repo"' "not-a-repo: structured error code"
contains "$OUT" 'is not a git repository' "not-a-repo: error message"
eq "$(field good state)" "clean" "not-a-repo: sibling still reported"
eq "$(sfield failed)" "1" "summary: failed=1"

# =============================================================================
echo; echo "## Phase 8 — usage/config errors abort with exit 2"
( cd "$WS" && "$BIN" brief --repo ghost  >"$OUT" 2>"$ERR" ); RC=$?
exits 2 "unknown --repo: exit 2"
( cd "$WS" && "$BIN" brief --group nope  >"$OUT" 2>"$ERR" ); RC=$?
exits 2 "unknown --group: exit 2"
( cd "$WS" && "$BIN" brief --frobnicate  >"$OUT" 2>"$ERR" ); RC=$?
exits 2 "unknown flag: exit 2 (clap usage)"

# =============================================================================
echo
echo "=============================================="
echo "  PASS: $PASS    FAIL: $FAIL"
echo "=============================================="
[ "$FAIL" -eq 0 ] || exit 1

# Security policy

## Trust model

ezgitx runs commands across the git repositories in your workspace. Two inputs
decide what runs, and **both are trusted to the same degree as a shell script
you wrote yourself**:

- **`.ezgitx.yml`**: its `default_cmd` and `check_cmd` fields are executed
  verbatim through your shell when you run `ezgitx run` or
  `ezgitx check-impact --check`. A config can also point a repo `path` outside
  the workspace root (e.g. `../sibling` or an absolute path), in which case
  commands run in that directory.
- **command arguments** you pass on the command line.

This is the same trust level as `make`, `npm run`, `direnv`, or a `Justfile`:
opening a workspace and running these commands executes whatever the config
author put there.

**So: only run ezgitx in a workspace whose `.ezgitx.yml` you trust.** Do not
point it at a repository or workspace you cloned from an untrusted source and
then run `ezgitx run` without reading the config first. If an AI agent operates
your workspace, the same rule applies to any config the agent did not write
from your own instructions.

Read-only commands are safe to run anywhere: `ezgitx brief` (offline session
snapshot; never fetches), `ezgitx changed` (offline cross-repo delta), `ezgitx
status`, `ezgitx pull` (fetch + fast-forward
only, never a merge commit), and the default `ezgitx check-impact` listing never
execute configured commands. Only `run` and `check-impact --check` do, and only
on the repos you target.

## What ezgitx does not do

- It never prompts or runs anything interactively (`GIT_TERMINAL_PROMPT=0` on
  every child process).
- It does not send your code, config, or git state anywhere. It shells out to
  your local `git` and your shell, nothing else.
- It stores no secrets. Lock files (`.ezgitx/locks/`) and freshness / brief
  session records (`.ezgitx/state/`) hold only a PID, hostname, timestamp,
  command string, and commit SHA.

## Supported versions

Security fixes land on the latest `0.x` release. Pre-`1.0`, only the most
recent published version is supported.

## Reporting a vulnerability

Please report security issues privately rather than opening a public issue.
Use GitHub's [private vulnerability reporting](https://github.com/yuval-r/ezgitx/security/advisories/new)
(Security tab → Report a vulnerability).

Please include the version, your platform, and a minimal reproduction. I aim to
acknowledge within 72 hours. Once a fix is released, I'm happy to credit you in
the advisory unless you'd prefer to stay anonymous.

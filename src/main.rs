mod cli;
mod commands;
mod config;
mod errors;
mod exec;
mod git;
mod graph;
mod lock;
mod output;
mod state;
mod workspace;

use std::path::Path;

use clap::Parser;

use cli::{Cli, Command};
use errors::EXIT_USAGE;
use workspace::{Repo, Workspace};

fn main() {
    let cli = Cli::parse();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to start tokio runtime");
    let code = runtime.block_on(dispatch(cli));
    std::process::exit(code);
}

async fn dispatch(cli: Cli) -> i32 {
    let cwd = match std::env::current_dir() {
        Ok(d) => d,
        Err(e) => {
            errors::print_top_level(&errors::ErrorInfo::new(
                errors::ErrorCode::ConfigInvalid,
                format!("cannot determine current directory: {e}"),
            ));
            return EXIT_USAGE;
        }
    };
    let ws = match workspace::load(&cwd) {
        Ok(ws) => ws,
        Err(e) => {
            errors::print_top_level(&e);
            return EXIT_USAGE;
        }
    };
    let jobs = cli.jobs.unwrap_or_else(num_cpus::get);
    let max_bytes = cli.max_bytes;
    let human = cli.human;

    match cli.command {
        Command::Brief { target, no_record } => match ws.select(&target.targeting(), &cwd) {
            Ok(repos) => {
                commands::brief::run(&ws, repos, target.dirty, jobs, max_bytes, no_record, human)
                    .await
            }
            Err(e) => usage(e),
        },
        Command::Status { target } => match ws.select(&target.targeting(), &cwd) {
            Ok(repos) => {
                commands::status::run(&ws, repos, target.dirty, jobs, max_bytes, human).await
            }
            Err(e) => usage(e),
        },
        Command::Pull { target, wait } => {
            match select_filtered(&ws, &target, &cwd, jobs, max_bytes).await {
                Ok(repos) => commands::pull::run(&ws, repos, wait, jobs, max_bytes, human).await,
                Err(e) => usage(e),
            }
        }
        Command::Run {
            cmd,
            with_deps,
            with_dependents,
            target,
        } => match select_filtered(&ws, &target, &cwd, jobs, max_bytes).await {
            Ok(repos) => {
                commands::run::run(
                    &ws,
                    repos,
                    cmd,
                    with_deps,
                    with_dependents,
                    jobs,
                    max_bytes,
                    human,
                )
                .await
            }
            Err(e) => usage(e),
        },
        Command::InitSkill => commands::init_skill::run(&ws, human),
        Command::CheckImpact { repo, check } => {
            commands::check_impact::run(&ws, &cwd, repo, check, jobs, max_bytes, human).await
        }
    }
}

fn usage(e: errors::ErrorInfo) -> i32 {
    errors::print_top_level(&e);
    EXIT_USAGE
}

/// Resolve targeting and apply the `--dirty` filter (PRD §4.3). Repos whose
/// status cannot be read stay selected so the command itself reports the
/// structured error (lazy validation).
async fn select_filtered(
    ws: &Workspace,
    target: &cli::TargetArgs,
    cwd: &Path,
    jobs: usize,
    max_bytes: usize,
) -> Result<Vec<Repo>, errors::ErrorInfo> {
    let repos = ws.select(&target.targeting(), cwd)?;
    if !target.dirty {
        return Ok(repos);
    }
    let mut kept = Vec::new();
    exec::run_parallel(
        repos,
        jobs,
        |repo| async move {
            let keep = match git::check_is_repo(&repo.path) {
                Err(_) => true,
                Ok(()) => match git::status(&repo.path, max_bytes).await {
                    Ok(s) => matches!(s.state, git::TreeState::Dirty | git::TreeState::Conflicted),
                    Err(_) => true,
                },
            };
            (repo, keep)
        },
        |(repo, keep)| {
            if keep {
                kept.push(repo);
            }
        },
    )
    .await;
    kept.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(kept)
}

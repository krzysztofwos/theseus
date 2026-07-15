//! The live goal-corpus runner.
//!
//! The corpus in `evals/README.md` is the record; running it has been manual.
//! This binary makes it a command: `evals list` shows the goals, `evals show
//! <id>` prints a goal's prompt, and `evals run <id>` checks a goal's
//! deterministic acceptance with no API key — the part CI can hold. With
//! `--live`, it drives the agent against an isolated root, captures the trace
//! under `evals/runs/`, and records a result row. Live runs stay operator-driven
//! (a real model, budgeted turns); deterministic acceptance does not.

mod record;
mod registry;
mod run;

use std::process::ExitCode;

use registry::{Acceptance, Kind, goal, goals};

fn main() -> ExitCode {
    match dispatch() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn dispatch() -> anyhow::Result<ExitCode> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("list") => {
            list();
            Ok(ExitCode::SUCCESS)
        }
        Some("show") => {
            let id = parse_id(args.get(1))?;
            show(id)?;
            Ok(ExitCode::SUCCESS)
        }
        Some("run") => {
            let id = parse_id(args.get(1))?;
            let live = args.iter().any(|a| a == "--live");
            let allow_writes = args.iter().any(|a| a == "--allow-writes");
            run::run(id, live, allow_writes)
        }
        _ => {
            eprintln!(
                "usage:\n  \
                 evals list\n  \
                 evals show <id>\n  \
                 evals run <id> [--live] [--allow-writes]"
            );
            Ok(ExitCode::FAILURE)
        }
    }
}

fn parse_id(arg: Option<&String>) -> anyhow::Result<u8> {
    let id = arg
        .ok_or_else(|| anyhow::anyhow!("this command takes a goal id; see `evals list`"))?
        .parse::<u8>()
        .map_err(|_| anyhow::anyhow!("a goal id is a number; see `evals list`"))?;
    anyhow::ensure!(goal(id).is_some(), "no goal {id}; see `evals list`");
    Ok(id)
}

fn list() {
    println!("{:>3}  {:<8}  {:<10}  title", "id", "kind", "accept");
    for goal in goals() {
        let accept = match goal.acceptance {
            Acceptance::OperationPresent(_) => "op",
            Acceptance::CargoTest { .. } => "test",
            Acceptance::LiveOnly => "live-only",
        };
        println!(
            "{:>3}  {:<8}  {:<10}  {}",
            goal.id,
            goal.kind.label(),
            accept,
            goal.title
        );
    }
}

fn show(id: u8) -> anyhow::Result<()> {
    let goal = goal(id).expect("id validated");
    println!("goal {}: {}", goal.id, goal.title);
    println!("proves: {}", goal.proves);
    println!("kind:   {}", goal.kind.label());
    if goal.kind == Kind::Foreign {
        println!("(run rooted in an isolated project the runner seeds)");
    }
    println!("\nprompt:\n{}", goal.prompt);
    Ok(())
}

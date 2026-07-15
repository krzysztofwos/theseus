//! Running a goal: the deterministic acceptance every run performs, and the
//! live orchestration that drives the agent and records the result.

use std::{
    path::{Path, PathBuf},
    process::{Command, ExitCode},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::Context;

use crate::record::{RunResult, parse_trace};
use crate::registry::{Acceptance, Goal, Kind, goal};

/// Run goal `id`. Without `live`, only its deterministic acceptance is checked,
/// which needs no API key. With `live`, the agent drives the goal first, its
/// trace is recorded, and then acceptance runs against the result.
pub fn run(id: u8, live: bool, allow_writes: bool) -> anyhow::Result<ExitCode> {
    let goal = goal(id).expect("id validated");
    let root = workspace_root()?;

    if live {
        let outcome = drive_live(&goal, &root, allow_writes)?;
        println!("live run recorded: {}", outcome.display());
    }

    match check_acceptance(&goal, &root)? {
        Verdict::Pass(detail) => {
            println!("goal {} acceptance: PASS — {detail}", goal.id);
            Ok(ExitCode::SUCCESS)
        }
        Verdict::LiveOnly => {
            println!(
                "goal {} acceptance: live-only — no deterministic artifact to check",
                goal.id
            );
            Ok(ExitCode::SUCCESS)
        }
        Verdict::Fail(detail) => {
            println!("goal {} acceptance: FAIL — {detail}", goal.id);
            Ok(ExitCode::FAILURE)
        }
    }
}

/// A deterministic acceptance verdict.
enum Verdict {
    Pass(String),
    Fail(String),
    LiveOnly,
}

/// Check every goal's deterministic acceptance, printing a line per goal, and
/// fail if any goal regressed. The corpus's CI entrypoint: no API key, and it
/// proves both the runner and that each recorded goal's artifact survives.
pub fn check_all() -> anyhow::Result<ExitCode> {
    let root = workspace_root()?;
    let mut failures = 0;
    for goal in crate::registry::goals() {
        let line = match check_acceptance(&goal, &root)? {
            Verdict::Pass(detail) => format!("PASS       goal {}: {detail}", goal.id),
            Verdict::LiveOnly => format!("live-only  goal {}: {}", goal.id, goal.title),
            Verdict::Fail(detail) => {
                failures += 1;
                format!("FAIL       goal {}: {detail}", goal.id)
            }
        };
        println!("{line}");
    }
    if failures == 0 {
        println!("\nthe corpus holds: every deterministic acceptance passed");
        Ok(ExitCode::SUCCESS)
    } else {
        println!("\n{failures} goal(s) regressed");
        Ok(ExitCode::FAILURE)
    }
}

/// Check a goal's deterministic acceptance against the workspace at `root`.
fn check_acceptance(goal: &Goal, root: &Path) -> anyhow::Result<Verdict> {
    match &goal.acceptance {
        Acceptance::LiveOnly => Ok(Verdict::LiveOnly),
        Acceptance::OperationPresent(name) => {
            let present = model_has_operation(root, name)?;
            Ok(if present {
                Verdict::Pass(format!("the model still exposes `{name}`"))
            } else {
                Verdict::Fail(format!("the model no longer exposes `{name}`"))
            })
        }
        Acceptance::CargoTest { package, test } => {
            let mut command = Command::new("cargo");
            command.args(["test", "-p", package]);
            if let Some(test) = test {
                command.args(["--test", test]);
            }
            command.current_dir(root);
            let status = command
                .status()
                .with_context(|| format!("running the acceptance test for goal {}", goal.id))?;
            let target = test
                .map(|t| format!("{package} --test {t}"))
                .unwrap_or_else(|| package.to_string());
            Ok(if status.success() {
                Verdict::Pass(format!("`cargo test -p {target}` passes"))
            } else {
                Verdict::Fail(format!("`cargo test -p {target}` failed"))
            })
        }
    }
}

/// Whether the Theseus model still exposes an operation named `name`.
fn model_has_operation(root: &Path, name: &str) -> anyhow::Result<bool> {
    let output = Command::new("cargo")
        .args(["run", "-q", "-p", "theseus-cli", "--", "model"])
        .current_dir(root)
        .output()
        .context("reading the model to check an operation is present")?;
    anyhow::ensure!(
        output.status.success(),
        "`theseus model` failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).context("parsing the model JSON")?;
    let model = value.get("model").unwrap_or(&value);
    let present = model
        .get("services")
        .and_then(|s| s.as_array())
        .into_iter()
        .flatten()
        .filter_map(|service| service.get("operations")?.as_array())
        .flatten()
        .filter_map(|op| op.get("name")?.as_str())
        .any(|op| op == name);
    Ok(present)
}

/// Drive a goal live: seed an isolated root for a foreign goal, run the agent
/// with the trace captured, record the result row, and return its path.
fn drive_live(goal: &Goal, root: &Path, allow_writes: bool) -> anyhow::Result<PathBuf> {
    let runs = root.join("evals/runs");
    std::fs::create_dir_all(&runs).context("creating evals/runs")?;
    let stamp = unix_time()?;

    let project = match goal.kind {
        Kind::SelfMod => None,
        Kind::Foreign => Some(seed_foreign_project(root, goal, stamp)?),
    };

    let trace_path = runs.join(format!("goal-{}-{stamp}.trace", goal.id));
    let mut command = Command::new("cargo");
    command.args(["run", "-q", "-p", "theseus-agent", "--"]);
    if let Some(project) = &project {
        command.arg("--project").arg(project);
    }
    if allow_writes {
        command.arg("--allow-writes");
    }
    command.arg(goal.prompt).current_dir(root);
    command.env("AGENT_TRACE", "1");
    let output = command
        .output()
        .with_context(|| format!("driving goal {} live", goal.id))?;
    let trace = String::from_utf8_lossy(&output.stdout).into_owned()
        + &String::from_utf8_lossy(&output.stderr);
    std::fs::write(&trace_path, &trace).context("writing the trace log")?;

    let (turns, tool_calls) = parse_trace(&trace);
    let result = RunResult {
        goal_id: goal.id,
        goal_title: goal.title.to_string(),
        unix_time: stamp,
        commit: commit(root),
        turns,
        tool_calls,
        acceptance: "pending — checked next".to_string(),
        trace_path: trace_path
            .strip_prefix(root)
            .unwrap_or(&trace_path)
            .display()
            .to_string(),
    };
    let result_path = runs.join(format!("goal-{}-{stamp}.json", goal.id));
    std::fs::write(&result_path, serde_json::to_string_pretty(&result)?)
        .context("writing the result row")?;
    Ok(result_path)
}

/// Seed an isolated foreign project for a live foreign goal: a fresh Git
/// repository the runner initializes, so a foreign run never touches the
/// developer's tree.
fn seed_foreign_project(root: &Path, goal: &Goal, stamp: u64) -> anyhow::Result<PathBuf> {
    let project = root.join(format!("evals/runs/project-{}-{stamp}", goal.id));
    std::fs::create_dir_all(&project).context("creating the foreign project root")?;
    run_ok(
        Command::new("git")
            .args(["init", "-q"])
            .current_dir(&project),
    )?;
    run_ok(
        Command::new("cargo")
            .args(["run", "-q", "-p", "theseus-cli", "--"])
            .arg("--project")
            .arg(&project)
            .args(["init", "--id", "eval-app", "--modeling-path"])
            .arg(root.join("rust/modeling"))
            .current_dir(root),
    )?;
    Ok(project)
}

fn run_ok(command: &mut Command) -> anyhow::Result<()> {
    let status = command.status().context("running a setup command")?;
    anyhow::ensure!(status.success(), "a setup command failed");
    Ok(())
}

/// The repository root, the directory holding `rust/` and `evals/`.
fn workspace_root() -> anyhow::Result<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .context("the evals crate lives at <root>/rust/evals")?;
    Ok(root.to_path_buf())
}

fn unix_time() -> anyhow::Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("reading the clock")?
        .as_secs())
}

/// The short commit the run drives, or `unknown` outside a repository.
fn commit(root: &Path) -> String {
    Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(root)
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

//! Theseus's command-line interface (L5) — the Cli inbound adapter.
//!
//! The `theseus` crate holds the service: the [`TheseusService`] contract, the
//! request types, the outbound port traits, and the [`Ctx`] composition root. The
//! generated [`generated`] module here supplies this inbound's surface — the
//! command, the parsed [`Invocation`](generated::Invocation), and the `dispatch`.
//! This entry point wires adapters into the composition, renders bespoke output
//! for the operations that need it — an exit code, per-file lines, a follow-up
//! notice — and delegates the rest to the generated `dispatch`.
//!
//! The composition is the CLI's to choose: `--remote <URL>` drives a remote
//! Theseus over HTTP through the generated client, every subcommand unchanged,
//! and `--calculator <ENDPOINT>` reaches the calculator over gRPC through its
//! generated client where the in-process adapter would stand.

mod generated;

use clap::Arg;
use generated::Invocation;
use theseus::{CargoToolchain, Ctx, FsWorkspace, GitCheckpoint, TheseusService};
use theseus_calculator::CalculatorService;
use theseus_calculator_grpc_client::GrpcCalculatorClient;
use theseus_http_client::HttpTheseusClient;
use theseus_model::theseus_model;

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    // `arg_required_else_help(true)` in the generated surface means a bare
    // invocation prints help and exits, so there is always a subcommand to parse.
    let matches = generated::command()
        .arg(
            Arg::new("remote")
                .long("remote")
                .global(true)
                .value_name("URL")
                .help("Drive a remote Theseus over HTTP at this base URL."),
        )
        .arg(
            Arg::new("calculator")
                .long("calculator")
                .global(true)
                .value_name("ENDPOINT")
                .help("Reach the calculator over gRPC at this endpoint."),
        )
        .get_matches();
    let invocation = Invocation::from_matches(&matches)?;

    // A remote composition: the generated HTTP client stands where the local
    // composition root would, and every subcommand drives the remote instance.
    // The remote instance owns its own calculator wiring, so the two flags name
    // conflicting compositions.
    if let Some(url) = matches.get_one::<String>("remote") {
        anyhow::ensure!(
            matches.get_one::<String>("calculator").is_none(),
            "--remote drives a whole remote instance; --calculator wires a local composition. Pass one or the other"
        );
        return run(&HttpTheseusClient::new(url.clone()), invocation).await;
    }

    let model = theseus_model();
    let workspace = FsWorkspace::at_repo_root();
    let toolchain = CargoToolchain;
    // The calculator port takes the in-process adapter, or the generated gRPC
    // client when an endpoint names a remote calculator — the same port either
    // way.
    let local_calculator;
    let remote_calculator;
    let calculator: &dyn CalculatorService = match matches.get_one::<String>("calculator") {
        Some(endpoint) => {
            remote_calculator = GrpcCalculatorClient::connect(endpoint.clone()).await?;
            &remote_calculator
        }
        None => {
            local_calculator = theseus_calculator::Calculator;
            &local_calculator
        }
    };
    let checkpoint = GitCheckpoint::at_repo_root();
    let ctx = Ctx {
        model: &model,
        workspace: &workspace,
        checkpoint: &checkpoint,
        calculator,
        toolchain: &toolchain,
    };
    run(&ctx, invocation).await
}

// ============================================================================
// Authored output — the CLI inbound adapter's response side. Each arm overrides
// the default for an operation that needs bespoke output: an exit code, per-file
// lines, a follow-up notice. Every other operation falls to the generated
// `dispatch`, so a new operation surfaces as text or JSON without a change here.
// ============================================================================

/// Run a parsed invocation against the service and write the result.
async fn run(service: &impl TheseusService, invocation: Invocation) -> anyhow::Result<()> {
    match invocation {
        Invocation::Verify => {
            let report = service.verify().await?;
            println!("{}", report.render());
            if !report.conformant {
                std::process::exit(1);
            }
        }
        Invocation::Generate => {
            for file in service.generate().await? {
                println!("wrote {}", file.path);
            }
        }
        Invocation::Scaffold => {
            let written = service.scaffold().await?;
            if written.is_empty() {
                println!("every library service crate is already scaffolded");
            }
            for file in &written {
                println!("scaffolded {}", file.path);
            }
        }
        Invocation::Patch(request) => {
            let writing = request.write;
            let outcome = service.patch(request).await?;
            println!("{}", serde_json::to_string_pretty(&outcome)?);
            if writing && outcome.ok {
                println!(
                    "applied and reprojected. Rebuild, then `coverage` shows any handler left to author"
                );
            }
            if !outcome.ok {
                std::process::exit(1);
            }
        }
        Invocation::Check => print_check_report(service.check().await?),
        Invocation::Test => print_check_report(service.test().await?),
        Invocation::Lint => print_check_report(service.lint().await?),
        other => generated::dispatch(service, other).await?,
    }
    Ok(())
}

fn print_check_report(report: theseus::CheckReport) {
    println!("{}", report.detail);
    if !report.ok {
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prune_carries_its_retention_limit_from_the_cli() {
        let matches = generated::command()
            .try_get_matches_from(["theseus", "prune", "--keep", "7"])
            .expect("the prune command parses");
        let invocation = Invocation::from_matches(&matches).expect("the invocation converts");

        let Invocation::Prune(request) = invocation else {
            panic!("prune parsed as another invocation");
        };
        assert_eq!(request.keep, 7);
    }
}

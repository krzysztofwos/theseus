//! Theseus's command-line interface (L4) — the Cli inbound adapter.
//!
//! The `theseus` crate holds the service: the [`TheseusService`] contract, the
//! request types, the outbound port traits, and the [`Ctx`] composition root. The
//! generated [`generated`] module here supplies this inbound's surface — the
//! command, the parsed [`Invocation`](generated::Invocation), and the `dispatch`.
//! This entry point wires real adapters into `Ctx`, renders bespoke output for the
//! operations that need it — an exit code, per-file lines, a follow-up notice —
//! and delegates the rest to the generated `dispatch`.

mod generated;

use generated::Invocation;
use theseus::{CargoToolchain, Ctx, FsWorkspace, TheseusService};
use theseus_model::theseus_model;

fn main() -> anyhow::Result<()> {
    let model = theseus_model();
    let workspace = FsWorkspace::at_repo_root();
    let calculator = theseus_calculator::Calculator;
    let toolchain = CargoToolchain;
    let ctx = Ctx {
        model: &model,
        workspace: &workspace,
        calculator: &calculator,
        toolchain: &toolchain,
    };

    // `arg_required_else_help(true)` in the generated surface means a bare
    // invocation prints help and exits, so there is always a subcommand to parse.
    let matches = generated::command().get_matches();
    run(&ctx, Invocation::from_matches(&matches))
}

// ============================================================================
// Authored output — the CLI inbound adapter's response side. Each arm overrides
// the default for an operation that needs bespoke output: an exit code, per-file
// lines, a follow-up notice. Every other operation falls to the generated
// `dispatch`, so a new operation surfaces as text or JSON without a change here.
// ============================================================================

/// Run a parsed invocation against the service and write the result.
fn run(service: &impl TheseusService, invocation: Invocation) -> anyhow::Result<()> {
    match invocation {
        Invocation::Verify => {
            let report = service.verify()?;
            println!("{}", report.render());
            if !report.conformant {
                std::process::exit(1);
            }
        }
        Invocation::Generate => {
            for file in service.generate()? {
                println!("wrote {}", file.path);
            }
        }
        Invocation::Scaffold => {
            let written = service.scaffold()?;
            if written.is_empty() {
                println!("every library service crate is already scaffolded");
            }
            for file in &written {
                println!("scaffolded {}", file.path);
            }
        }
        Invocation::Patch(request) => {
            let writing = request.write;
            let outcome = service.patch(request)?;
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
        other => generated::dispatch(service, other)?,
    }
    Ok(())
}

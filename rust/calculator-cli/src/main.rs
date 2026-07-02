//! A standalone command-line interface to the Calculator service.
//!
//! The command surface, the parsed invocation, and the dispatch are generated
//! from the service's operations. This entry point backs the contract with the
//! `Calculator` adapter and drives it from the command line.

mod generated;

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let calculator = theseus_calculator::Calculator;
    let matches = generated::command().get_matches();
    generated::dispatch(&calculator, generated::Invocation::from_matches(&matches)?).await
}

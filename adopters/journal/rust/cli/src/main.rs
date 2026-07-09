//! A standalone command-line interface to the Journal service.
//!
//! The command surface, the parsed invocation, and the dispatch are generated
//! from the service's operations. This entry point is the authored composition
//! root: it backs the contract with adapters and drives it from the command line.

mod generated;

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let model = journal_model::journal_model();
    let store = journal::FileStore::from_env();
    let ctx = journal::Ctx {
        model: &model,
        store: &store,
    };
    let matches = generated::command().get_matches();
    generated::dispatch(&ctx, generated::Invocation::from_matches(&matches)?).await
}

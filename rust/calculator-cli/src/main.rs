//! A standalone calculator binary (L2).
//!
//! The command surface, the parsed [`Invocation`](generated::Invocation), and the
//! default presentation are generated from the Calculator service's operations.
//! This entry point is the authored composition root: it backs the service's
//! contract with the `Calculator` adapter and drives it from the command line.

mod generated;

fn main() -> anyhow::Result<()> {
    let calculator = theseus_calculator::Calculator;
    let matches = generated::command().get_matches();
    generated::present(&calculator, generated::Invocation::from_matches(&matches))
}

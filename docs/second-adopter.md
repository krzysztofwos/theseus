# The second adopter

The engine's reusability claim, tested from outside. Everything before this was proven on the model that grew up alongside the engine; `adopters/journal/` is a workspace the Theseus self-model knows nothing about — its own `Cargo.toml`, its own model of record, its own path conventions — consuming `theseus-modeling` as an ordinary dependency.

## What it is

A journal: one service (`add`, `list`, `search`) over a `store` port, a `FileStore` adapter writing one entry per line, and a CLI inbound. The adopter's model of record is a hand-maintained `journal_model()` in `rust/model/src/lib.rs`, and one small binary (`project`, 32 lines) writes the crate skeletons that are missing and every generated file — the adopter's analog of `scaffold` + `generate`, standing only on the engine and the model.

The division of labor came out exactly as the architecture names it. Generated: the service contract with its `Ctx` and `Standalone` roots, the `Store` port trait with its typed defaults and borrowed forwarder, the request structs, and the whole command surface with its parsers and dispatch — 210 lines. Authored: the model of record and the projection binary (136 lines), and the three leaves — the `FileStore` adapter, the handlers on `Ctx`, and the composition root in `main` (85 lines). The adopter's conformance test runs the same ten-check `verify` over its own model and passes.

## What the adopter found

Four engine defects, none visible from inside the Theseus workspace, all fixed at the source:

1. The scaffolded `service.rs` only knew the portless convention (`impl Trait for Adapter`); a ported service's generated `Standalone` delegates through `Ctx`, so the skeleton did not compile. The scaffold now emits `impl Trait for Ctx<'_>` when the service carries ports, and re-exports `Ctx`, `Standalone`, and the port traits from the library root.
2. The scaffolded binary `main` was synchronous, calling an async dispatch. The template predated the async migration and had never been exercised since, because Theseus's own mains are authored. It now renders an async main — the working composition for a portless service, and a compiling authored hole (`todo!`) for a ported one, since adapters cannot be invented.
3. A ported service's `Ctx` carries the engine's `Model`, but the scaffolded manifest never depended on the engine — invisible in-tree, where the path is hand-written. The scaffold now writes `theseus-modeling = { workspace = true }` for ported crates, and each workspace points the name at wherever its engine lives.
4. The scaffolded binary manifest hardcoded a single dependency (the driven service) instead of rendering the crate's modeled dependency edges, so `verify`'s first check failed against the scaffold's own output. It now renders every modeled edge.

The pattern across all four: the scaffold's templates encode conventions, and conventions that the home workspace establishes by hand are exactly the ones a template silently gets wrong. A second adopter is the only test bench that exercises them.

## Running it

```sh
cd adopters/journal
cargo run -p journal-model --bin project   # scaffold what is missing, regenerate everything
cargo test                                  # the ten-check conformance over the journal's model
cargo run -p journal-cli -- add --text "hello"
cargo run -p journal-cli -- list
cargo run -p journal-cli -- search --term hello
```

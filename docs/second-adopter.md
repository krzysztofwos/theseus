# The second adopter

The harness's reusability claim, tested from outside. Everything before this was proven on the model that grew up alongside the engine; `adopters/journal/` is a workspace the Theseus self-model knows nothing about — its own `Cargo.toml`, durable model record, and project identity.

## What it is

A journal: one service (`add`, `list`, `search`) over a `store` port, a `FileStore` adapter writing one entry per line, and a CLI inbound. Its canonical `model.json` is projected with every generated contract and included in checkpoint ownership. `journal-model` exposes the same versioned `RustWorkspaceLayout` data that the runtime freezes into snapshots. The small `project` binary remains a recovery/bootstrap convenience, not a separate policy implementation.

The decisive proof is `rust/theseus/tests/foreign_project.rs`. It copies the adopter into an isolated top-level Git repository, establishes a `ProjectContext`, and first injects a foreign-only compiler failure to prove Cargo is rooted there. It then drives only public `Session` calls: verify, snapshot, add a `count` operation with a durable patch, report the coverage gap, implement the handler, check, test, verify again, and rollback. The test proves that the model record, lockfile, and authored/generated files are byte-exact, an unrelated untracked file survived, and the original adopter was untouched. Finally it reloads the restored JSON record into a cold `ProjectContext` and proves a fresh `StatefulSession` reads and queries the foreign root.

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

From the repository root, the full harness workflow is executable as one regression:

```sh
cargo test -p theseus --test foreign_project
```

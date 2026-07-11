# The second adopter

The harness's reusability claim, tested from outside. Everything before this was proven on the model that grew up alongside the engine; `adopters/journal/` is a workspace the Theseus self-model knows nothing about — its own `Cargo.toml`, durable model record, and project identity.

## What it is

A journal: one service (`add`, `list`, `search`) over a `store` port, a `FileStore` adapter writing one entry per line, and a CLI inbound. Its strict `theseus.json` fixes the stable project ID, versioned `RustWorkspaceLayout`, and canonical `model.json` record used by every launcher. The model record is projected with every generated contract and included in checkpoint ownership. `journal-model` still exposes the compiled layout and a small `project` binary as a recovery/development convenience, but ordinary runtime discovery reads data only: `ProjectContext::open` does not execute the adopter's Rust builder.

The decisive proof is `rust/theseus/tests/foreign_project.rs`. It copies the adopter into an isolated top-level Git repository and cold-opens it through `theseus.json`. It first injects a foreign-only compiler failure to prove Cargo is rooted there. It then drives only public `Session` calls: verify, snapshot, add a `count` operation with a durable patch, report the coverage gap, implement the handler, and read an authored source document with its complete-file revision. A governed insertion of a broken test module fails the all-target compile gate and restores the file; a valid test module commits. The session then checks, tests, verifies, and rolls back.

The test proves that the manifest, model record, lockfile, and authored/generated files are byte-exact, an unrelated untracked file survived, and the original adopter was untouched. Finally it opens the restored manifest and JSON record into a fresh `ProjectContext` and proves a new locked `StatefulSession` reads and queries the foreign root. This covers the durable project path used by `--project` on CLI, agent, MCP, HTTP, and gRPC launchers; it is deterministic integration coverage, not a live-model result.

`rust/theseus/tests/initialized_project.rs` covers the step before journal: starting with an empty top-level Git repository and no `HEAD` commit. The transactional initializer writes a minimal durable model, project manifest, workspace and lockfile, generated projections, and authored leaves; compile-checks the seed; then the test creates a root snapshot, grows and runs an operation through the same public tools, rolls back, and cold-opens the original seed. That initialized project is intentionally smaller than the hand-authored journal adopter.

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
cargo test -p theseus --test initialized_project
cargo run -p theseus-cli -- --project adopters/journal verify
```

A real agent can be rooted in the same durable adopter:

```sh
cargo run -p theseus-agent -- \
  --project adopters/journal --allow-writes \
  "add a count operation, test it, and leave the project conformant"
```

That command is the pending live goal, not a recorded green result. Use a disposable copy or snapshot first.

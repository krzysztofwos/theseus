# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

Theseus is a self-modeling CLI seed, named for the Ship of Theseus: it holds a model of its own architecture and can regenerate its own code from that model. It is a clean-room experiment — a tiny category/functor conformance kernel reimplemented from scratch, with nothing depending on external categorical frameworks.

The whole system is a fixed point: `rust/model/src/self_model.rs` is a value that describes the very tool that holds it.

## Commands

Toolchain is pinned to nightly (`rust-toolchain.toml`), edition 2024.

```bash
cargo build                             # build the workspace
cargo test                              # run all tests
cargo test -p theseus-modeling          # test one crate
cargo test -p theseus-model \
    theseus_conforms_to_its_self_model  # run one test
cargo run -p theseus-cli -- verify      # run the `theseus` binary (subcommands below)
cargo +nightly fmt                      # format (config in .trunk/configs/.rustfmt.toml)
cargo clippy                            # lint
```

Linting is also driven by Trunk (`.trunk/trunk.yaml`): `trunk check` runs clippy, rustfmt, taplo, and security scanners. Trunk auto-formats on pre-commit and checks on pre-push.

CLI subcommands (the self-referential operations): `model` (self-describe + model hash), `verify` (self-conformance), `generate` (regenerate code from the model), `coverage` (the unimplemented-handler worklist), `show` (an operation's current handler source), `implement` (write an operation's handler into `service.rs` — inserting it, or replacing it in place), and the agent protocol `query` / `patch`.

## Architecture

Four layered crates under `rust/`. Each layer may depend only on strictly lower layers — this layering is itself what `verify` checks.

- `theseus-kernel` (L0) — `rust/kernel/`. Finite categories, functors, and the one law: a functor sends every morphism to one with matching endpoints. The structural substrate for all conformance checks. Knows nothing about Theseus.
- `theseus-modeling` (L1) — `rust/modeling/`. The general engine over _any_ model: the `Model` vocabulary + fluent-builder DSL (`dsl.rs`), stable hashing (`hash.rs`), `verify`, `codegen`, the agent `query`/`patch` surface, and source splicing (`source.rs`).
- `theseus-model` (L2) — `rust/model/`. The _adopter_: the concrete `theseus_model()`, the workspace-relative paths Theseus owns, and `generated_files()`. This is the model of record.
- `theseus-cli` (L3) — `rust/cli/`. Thin clap binary named `theseus`. `main.rs` is the composition root plus authored leaves. `generated.rs` is machine-generated.

The framework/adopter split (L1 engine ↔ L2 concrete model) is the central design seam: the engine is reusable. The adopter supplies one model and its owned paths.

### The model → code → verify loop

`Model` is a hex-style vocabulary: `Model { crates, types: Vec<TypeDef>, services: Vec<Service> }`. A `Service` has one `inbound: Transport`, a list of `operations`, and outbound `Port`s. Theseus is modeled as a single `Service` — inbound `Cli`, its subcommands as operations, one filesystem outbound port (`workspace`).

`theseus generate` renders `rust/cli/src/generated.rs` from the model: the command surface, the request structs, the `TheseusService` trait (one method per operation, each defaulting to an `unimplemented` error), the `Invocation` enum, the default `present` function (text for a `String` response, otherwise pretty JSON), the `Workspace` port trait, and the composition root `Ctx`. The request surface (CLI args from request fields) and the response surface (`present`) are both generated. The hand-authored leaves are never touched by regeneration: `main.rs` holds the composition root, the filesystem adapter, and the presenter overrides. `rust/cli/src/service.rs` holds the `impl TheseusService for Ctx` operation handlers.

`theseus verify` runs five checks, all derived from the same model (see `verify.rs`):

1. Required dependencies — every modeled dep edge exists in the real `Cargo.toml`s (a functor from the spec graph into the extracted graph).
2. Dependency direction — every real dep descends through the layer preorder (a layering functor).
3. Type references — every request and response label resolves to a builtin or a defined type.
4. Generated drift — files on disk match a fresh render.
5. Implementation coverage — every operation has an authored handler in `service.rs`. The trait defaults each method to `unimplemented`, so this check holds the gate the compiler once did. `theseus coverage` reports the same worklist with each gap's signature.

### Working on the self-model — the critical workflow

When you change `rust/model/src/self_model.rs` (or anything that affects the rendered output):

1. Run `cargo run -p theseus-cli -- generate` to refresh `rust/cli/src/generated.rs`. Skipping this fails the drift-gate test (`theseus_conforms_to_its_self_model`).
2. If you added an operation, author its handler in `impl TheseusService for Ctx` in `service.rs`. The build stays green — the handler defaults to `unimplemented` and the result surfaces through the generated `present` default — and `coverage` / `verify` report the operation until you author it. Override presentation in `run()` (in `main.rs`) only for bespoke output: an exit code, per-file lines, a follow-up notice.
3. Never hand-edit `generated.rs` (it carries a `// @generated … do not edit by hand` header).

The agent protocol mutates the model from outside. `theseus query` reports a stable handle per node and the model hash. `theseus patch --verb <add|remove|rename|set> --target <handle> --expect-model-hash <hash>` is hash-checked against that hash (stale edits are refused with `PATCH00x` coded diagnostics + repair shapes). With `--write`, the proposed model is reprojected — `self_model.rs` and `generated.rs` re-render together — and the compiler points at the presentation arm left to author.

## Conventions

These are enforced by review in this repo. Two project skills carry the full rules: `rust-style` (errors, newtypes, ownership, module organization) and `rust-hexagonal` (domain/ports/adapters/services). The skills are vendored from a larger project, so ignore their references to files (`docs/…`), crates (`smithkit`, `sqlx`, `axum`), and "Hard Rules" that don't exist here — the _patterns_ apply, the specific paths don't.

- Errors: `thiserror` typed enums in the library crates (kernel/modeling). `anyhow` only in the `theseus-cli` binary. No `Result<T, String>`.
- Comments state what the code IS and DOES, positively. Do not write what the code omits, lacks, or does "instead of" (no "deliberately omits X", "rather than Y"). Do not name the crate behind a concept (say "command surface", not "clap"). Prefer a full stop over a semicolon joining clauses. Keep `// @generated` markers.
- Commit hygiene: new workspace dependencies land just-in-time in their own `Cargo workspace: <category>` commit, right before the crate that first needs it. `Cargo.lock` goes in its own trailing commit, never bundled into a feature commit. Per-crate `Cargo.toml`: workspace deps first (alphabetical), blank line, then path deps. Crates are introduced already-functional. History is curated to read as a clean build-up — before any rewrite, set a backup ref and confirm `git diff <backup> HEAD` is empty.

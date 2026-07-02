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
cargo run -p theseus-agent -- "<goal>"  # the `agent` binary: the internal agent loop
cargo run -p theseus-mcp                # the `mcp-server` binary: MCP over stdio
cargo +nightly fmt                      # format (config in .trunk/configs/.rustfmt.toml)
cargo clippy                            # lint
```

Linting is also driven by Trunk (`.trunk/trunk.yaml`): `trunk check` runs clippy, rustfmt, taplo, and security scanners. Trunk auto-formats on pre-commit and checks on pre-push.

CLI subcommands (the self-referential operations): `model` (self-describe + model hash), `verify` (self-conformance), `generate` (regenerate code from the model), `coverage` (the unimplemented-handler worklist), `scaffold` (write the skeleton of each service crate that lacks one), `show` (an operation's current handler source), `implement` (write an operation's handler into `service.rs` — inserting it, or replacing it in place), `check` (compile-check the workspace through the toolchain port), `calc` (evaluate arithmetic through the calculator service), and the agent protocol `query` / `patch`.

The self-modifying agent loop is not a subcommand. It is a separate `agent` binary, one of the inbounds that drive the Theseus service over the same operations (see Architecture).

## Architecture

Nine crates under `rust/`, layered. Each crate may depend only on strictly lower layers — this layering is itself what `verify` checks.

- `theseus-kernel` (L0) — `rust/kernel/`. Finite categories, functors, and the one law: a functor sends every morphism to one with matching endpoints. The structural substrate for all conformance checks. Knows nothing about Theseus.
- `theseus-modeling` (L1) — `rust/modeling/`. The general engine over _any_ model: the `Model` vocabulary + fluent-builder DSL (`dsl.rs`), stable hashing (`hash.rs`), `verify`, `codegen`, crate scaffolding (`scaffold.rs`), the agent `query`/`patch` surface, and source splicing (`source.rs`).
- `theseus-model` (L2) — `rust/model/`. The _adopter_: the concrete `theseus_model()`, the workspace-relative paths Theseus owns, and `generated_files()`. This is the model of record.
- `theseus` (L3) — `rust/theseus/`. The Theseus service itself. `generated.rs` holds the model-rendered contract: the `TheseusService` trait, the request structs, the outbound port traits, and the composition root `Ctx`. `service.rs` holds the authored `impl TheseusService for Ctx`. `session.rs` holds the shared `Session` and `tool_catalog()` that the agent loop and the MCP server both drive.
- `theseus-cli` (L4) — `rust/cli/`. The `Cli` inbound, the binary `theseus`. `generated.rs` renders the command surface and dispatch. `main.rs` wires concrete adapters into `Ctx` and runs.
- `theseus-agent` (L4) — `rust/agent/`. The `Agent` inbound, the binary `agent`. An LLM drives the service's operations as tools over a `Session` in a loop, behind an `Llm` port — an Anthropic adapter, or an offline stub.
- `theseus-mcp` (L4) — `rust/mcp/`. The `Mcp` inbound, the binary `mcp-server`. A Model Context Protocol server exposing the same `tool_catalog()` over the same `Session` to an external host over stdio.
- `theseus-calculator` (L1) — `rust/calculator/`. A second service, `Calculator` (four arithmetic operations over `Operands`), reached from Theseus through an in-process `calculator` port.
- `theseus-calculator-cli` (L2) — `rust/calculator-cli/`. A standalone `calculator` binary driving that service through its own `Cli` inbound — the worked multi-service example (`docs/building-a-calculator.md`).

The framework/adopter split (L1 engine ↔ L2 concrete model) is the central design seam: the engine is reusable. The adopter supplies one model and its owned paths.

### The model → code → verify loop

`Model` is a hex-style vocabulary: `Model { crates, types: Vec<TypeDef>, services: Vec<Service>, inbounds: Vec<Inbound> }`. A `Service` is transport-neutral — a list of `operations` and outbound `Port`s, in a named crate. An `Inbound` drives a service over a `Transport` (`Cli`, `Http`, `Grpc`, `Agent`, or `Mcp`). A service with no inbound is driven in process through a port. Theseus is a `Service` whose operations are its self-referential capabilities, exposed as CLI subcommands, agent tools, or MCP tools depending on the inbound. Its outbound ports include a filesystem `workspace`, a `toolchain` that compile-checks the workspace, and a `calculator` port targeting the second `Calculator` service. A `Cli` inbound drives each service over the command line, and Theseus additionally carries `Agent` and `Mcp` inbounds — the `agent` loop and the `mcp-server`, each its own binary driving the same operations through a shared `Session`.

`theseus generate` renders a `generated.rs` for each service-owning crate (the contract) and each `Cli`-inbound crate (the surface). The service crate's file renders the request structs, the `TheseusService` trait (one method per operation, each defaulting to an `unimplemented` error), the outbound port traits, and the composition root `Ctx`. A `Cli`-inbound crate's file renders the command surface, the request parsers, the `Invocation` enum, and the `dispatch` function (text for a `String` response, otherwise pretty JSON) — the request surface (arguments from request fields) and the response surface (`dispatch`) both generated. An `Agent` or `Mcp` inbound renders no surface, being an authored binary. The hand-authored leaves are never touched by regeneration: `rust/theseus/src/service.rs` holds the `impl TheseusService for Ctx` operation handlers, and each inbound binary's `main.rs` holds the composition root, the adapters, and any output overrides in `run()`.

`theseus verify` runs seven checks, all derived from the same model (see `verify.rs`):

1. Required dependencies — every modeled dep edge exists in the real `Cargo.toml`s (a functor from the spec graph into the extracted graph).
2. Dependency direction — every real dep descends through the layer preorder (a layering functor).
3. Type references — every request and response label resolves to a builtin or a defined type.
4. Port targets — every service-targeting port resolves to a defined service.
5. Inbound services — every inbound adapter drives a defined service.
6. Generated drift — files on disk match a fresh render.
7. Implementation coverage — every operation has an authored handler in `rust/theseus/src/service.rs`. The trait defaults each method to `unimplemented`, so this check holds the gate the compiler once did. `theseus coverage` reports the same worklist with each gap's signature.

### Working on the self-model — the critical workflow

When you change `rust/model/src/self_model.rs` (or anything that affects the rendered output):

1. Run `cargo run -p theseus-cli -- generate` to refresh the generated code (each crate's `generated.rs` and the canonical `self_model.rs`). Skipping this fails the drift-gate test (`theseus_conforms_to_its_self_model`).
2. If you added an operation, author its handler in `impl TheseusService for Ctx` in `rust/theseus/src/service.rs`. The build stays green — the handler defaults to `unimplemented` and the result surfaces through the generated `dispatch` default — and `coverage` / `verify` report the operation until you author it. Override the output in `run()` (in the CLI's `main.rs`) only for bespoke output: an exit code, per-file lines, a follow-up notice.
3. Never hand-edit `generated.rs` (it carries a `// @generated … do not edit by hand` header).

The agent protocol mutates the model from outside. `theseus query` reports a stable handle per node and the model hash. `theseus patch --edit '<verb>|<target>|<key=value>…'` addresses a node by its handle and is repeatable, applying each edit in order (a refused edit carries a `PATCH00x` coded diagnostic + repair shape). With `--write`, the proposed model is reprojected — `self_model.rs` and `generated.rs` re-render together — and `coverage` reports each new operation's handler as unimplemented until it is authored in `rust/theseus/src/service.rs`.

The agent loop turns this protocol inward. The `agent` binary runs an LLM that drives Theseus's own operations as tools over a `Session` — a working copy of the model — so it edits the model it inspects, with writes to disk gated by `agent --allow-writes`. The `mcp-server` binary exposes the same surface (the shared `tool_catalog()`) over the Model Context Protocol, so an external host drives the same `Session`. Both are covered in `docs/building-the-harness.md`.

## Conventions

These are enforced by review in this repo. Two project skills carry the full rules: `rust-style` (errors, newtypes, ownership, module organization) and `rust-hexagonal` (domain/ports/adapters/services). The skills are vendored from a larger project, so ignore their references to files (`docs/…`), crates (`smithkit`, `sqlx`, `axum`), and "Hard Rules" that don't exist here — the _patterns_ apply, the specific paths don't.

- Errors: `thiserror` typed enums in the engine crates (kernel, modeling). `anyhow` in the service and inbound crates downstream (`theseus`, `theseus-cli`, `theseus-agent`, `theseus-mcp`, and the calculator crates). No `Result<T, String>`.
- Comments state what the code IS and DOES, positively. Do not write what the code omits, lacks, or does "instead of" (no "deliberately omits X", "rather than Y"). Do not name the crate behind a concept (say "command surface", not "clap"). Prefer a full stop over a semicolon joining clauses. Keep `// @generated` markers.
- Commit hygiene: new workspace dependencies land just-in-time in their own `Cargo workspace: <category>` commit, right before the crate that first needs it. `Cargo.lock` goes in its own trailing commit, never bundled into a feature commit. Per-crate `Cargo.toml`: workspace deps first (alphabetical), blank line, then path deps. Crates are introduced already-functional. History is curated to read as a clean build-up — before any rewrite, set a backup ref and confirm `git diff <backup> HEAD` is empty.

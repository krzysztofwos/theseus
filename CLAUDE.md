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
cargo run -p theseus-http               # the `http-server` binary: operations over HTTP
cargo run -p theseus-grpc               # the `grpc-server` binary: operations over gRPC
cargo run -p theseus-calculator-grpc    # the `calculator-grpc` binary: the calculator over gRPC
cargo +nightly fmt                      # format (config in .trunk/configs/.rustfmt.toml)
cargo clippy                            # lint
```

Linting is also driven by Trunk (`.trunk/trunk.yaml`): `trunk check` runs clippy, rustfmt, taplo, and security scanners. Trunk auto-formats on pre-commit and checks on pre-push.

CLI subcommands (the self-referential operations): `model` (self-describe + model hash), `verify` (self-conformance), `generate` (regenerate code from the model), `coverage` (the unimplemented-handler worklist), `scaffold` (write the skeleton of each service crate that lacks one), `show` (an operation's current handler source), `implement` (write an operation's handler into `service.rs`, or with `--port` a port's adapter method into the crate's `lib.rs` — inserting or replacing in place — and compile-check it), `check` (compile-check the workspace through the toolchain port), `calc` (evaluate arithmetic through the calculator service), and the agent protocol `query` / `patch`.

The self-modifying agent loop is not a subcommand. It is a separate `agent` binary, one of the inbounds that drive the Theseus service over the same operations (see Architecture).

## Architecture

Fifteen crates under `rust/`, layered. Each crate may depend only on strictly lower layers — this layering is itself what `verify` checks.

- `theseus-kernel` (L0) — `rust/kernel/`. Finite categories, functors, and the one law: a functor sends every morphism to one with matching endpoints. The structural substrate for all conformance checks. Knows nothing about Theseus.
- `theseus-modeling` (L1) — `rust/modeling/`. The general engine over _any_ model: the `Model` vocabulary + fluent-builder DSL (`dsl.rs`), stable hashing (`hash.rs`), `verify`, `codegen`, crate scaffolding (`scaffold.rs`), the agent `query`/`patch` surface, and source splicing (`source.rs`).
- `theseus-model` (L2) — `rust/model/`. The _adopter_: the concrete `theseus_model()`, the workspace-relative paths Theseus owns, and `generated_files()`. This is the model of record.
- `theseus` (L3) — `rust/theseus/`. The Theseus service itself. `generated.rs` holds the model-rendered contract: the `TheseusService` trait, the request structs, the outbound port traits, the composition roots (the borrowed `Ctx` and the owned `Standalone`), and the `tool_catalog()` and `dispatch_tool()` rendered from each exposed operation. `service.rs` holds the authored `impl TheseusService for Ctx`. `session.rs` holds the shared `Session` — the working model and the gated workspace port over the generated tool dispatch — that the agent loop and the MCP server both drive.
- `theseus-cli` (L5) — `rust/cli/`. The `Cli` inbound, the binary `theseus`. `generated.rs` renders the command surface and dispatch. `main.rs` wires the composition: local adapters into `Ctx` by default, the generated HTTP client in their place under `--remote <URL>` (every subcommand drives a remote instance unchanged), and the generated calculator gRPC client on the `calculator` port under `--calculator <ENDPOINT>`.
- `theseus-agent` (L4) — `rust/agent/`. The `Agent` inbound, the binary `agent`. An LLM drives the service's operations as tools over a `Session` in a loop, behind an `Llm` port — an Anthropic adapter, or an offline stub. A loop-level `restart` tool rebuilds the workspace and re-enters the session in the new binary, over a transcript persisted to `.theseus/session.json` (resumed with `agent --resume`).
- `theseus-mcp` (L4) — `rust/mcp/`. The `Mcp` inbound, the binary `mcp-server`. A Model Context Protocol server exposing the same `tool_catalog()` over the same `Session` to an external host over stdio.
- `theseus-grpc` (L4) — `rust/grpc/`. The `Grpc` inbound, the binary `grpc-server`. The build compiles a model-rendered `proto/theseus.proto` — the `Edit` enum as a `oneof` over its verbs, attribute maps as proto maps, and each foreign-typed response as a message carrying its JSON rendering — and generated glue maps outcomes onto gRPC statuses (UNIMPLEMENTED, PERMISSION_DENIED, INTERNAL).
- `theseus-http` (L4) — `rust/http/`. The `Http` inbound, the binary `http-server`. Every operation over `POST /{operation}` with a JSON body, through generated handlers whose status map derives from the outcome's structure: 200 a result, 400 a body that does not parse, 404 an unknown operation, 501 an operation on its trait default, 403 a write the gate refused, 500 anything else.
- `theseus-http-client` (L4) — `rust/http-client/`. The `Http` client adapter: the `TheseusService` contract carried over HTTP. Each call posts its request as a JSON body, and the reply's status maps back onto the contract's error classes — 501 the typed `Unimplemented`, 403 the typed `Refused` — so the classes survive the wire crossing (proven by round-trip tests over a real socket).
- `theseus-grpc-client` (L4) — `rust/grpc-client/`. The `Grpc` client adapter over the generated stub: requests convert to the proto messages (the `Edit` oneof crosses verb by verb), statuses map back onto the typed error classes, and foreign-typed responses parse from their JSON envelope.
- `theseus-calculator` (L1) — `rust/calculator/`. A second service, `Calculator` (four arithmetic operations over `Operands`), reached from Theseus through an in-process `calculator` port.
- `theseus-calculator-grpc-client` (L2) — `rust/calculator-grpc-client/`. The Calculator contract carried over gRPC — the client the `theseus` CLI wires onto its `calculator` port for a remote composition.
- `theseus-calculator-cli` (L2) — `rust/calculator-cli/`. A standalone `calculator` binary driving that service through its own `Cli` inbound — the worked multi-service example (`docs/building-a-calculator.md`).
- `theseus-calculator-grpc` (L2) — `rust/calculator-grpc/`. The `Grpc` inbound, the binary `calculator-grpc`. The build compiles a model-rendered `proto/calculator.proto` (drift-gated like every generated file) into the wire types and server trait, and generated glue maps outcomes onto gRPC statuses (UNIMPLEMENTED, PERMISSION_DENIED, INTERNAL).

The framework/adopter split (L1 engine ↔ L2 concrete model) is the central design seam: the engine is reusable. The adopter supplies one model and its owned paths.

The contract is async end to end: the generated service and port traits are async (through `async-trait`, so they stay usable as trait objects), the adapters and authored handlers await their ports, and every inbound binary runs on an async runtime. The engine (L1) stays synchronous pure computation. The `theseus` crate also holds the shared adapters: `FsWorkspace`, `CargoToolchain`, and the `GatedWorkspace` write gate, which wraps an owned adapter or a borrowed one.

### The model → code → verify loop

`Model` is a hex-style vocabulary: `Model { crates, types: Vec<TypeDef>, services: Vec<Service>, inbounds: Vec<Inbound>, clients: Vec<Client> }`. A `Service` is transport-neutral — a list of `operations` and outbound `Port`s, in a named crate. An `Inbound` drives a service over a `Transport` (`Cli`, `Http`, `Grpc`, `Agent`, or `Mcp`), and a `Client` is its mirror: an adapter implementing the service's contract over a transport, wired by a composition root where an in-process adapter would stand. A service with no inbound is driven in process through a port. Theseus is a `Service` whose operations are its self-referential capabilities, exposed as CLI subcommands, agent tools, or MCP tools depending on the inbound. Its outbound ports include a filesystem `workspace`, a `toolchain` that compile-checks the workspace, and a `calculator` port targeting the second `Calculator` service. A `Cli` inbound drives each service over the command line, and Theseus additionally carries `Agent` and `Mcp` inbounds — the `agent` loop and the `mcp-server`, each its own binary driving the same operations through a shared `Session`.

`theseus generate` renders a `generated.rs` for each service-owning crate (the contract) and each `Cli`-inbound crate (the surface). The service crate's file renders the request structs, the `TheseusService` trait (one method per operation, each defaulting to an `unimplemented` error), the outbound port traits (whose methods default the same way, so an adapter authors what it implements and a port can grow a method without breaking its adapters), and the composition roots — the borrowed `Ctx` a per-call inbound builds, and the owned `Standalone`, generic over one adapter per port, whose per-operation delegations regenerate with the contract so a new operation reaches every owned composition on the next render. A `Cli`-inbound crate's file renders the command surface, the request parsers, the `Invocation` enum, and the `dispatch` function (text for a `String` response, otherwise pretty JSON) — the request surface (arguments from request fields) and the response surface (`dispatch`) both generated. An `Agent` or `Mcp` inbound's surface — the tool catalog and the tool dispatch — renders with the service crate, while the binary itself stays authored. An `Http` inbound's crate renders the operation handlers with their structural status map, and a `Grpc` inbound's crate renders the proto contract plus the service glue with its status map; each binary's `main.rs` stays the authored composition root. The hand-authored leaves are never touched by regeneration: `rust/theseus/src/service.rs` holds the `impl TheseusService for Ctx` operation handlers, and each inbound binary's `main.rs` holds the composition root, the adapters, and any output overrides in `run()`.

`theseus verify` runs eight checks, all derived from the same model (see `verify.rs`):

1. Required dependencies — every modeled dep edge exists in the real `Cargo.toml`s (a functor from the spec graph into the extracted graph).
2. Dependency direction — every delivered dep (the `[dependencies]` table; test-only deps cross freely) descends through the layer preorder (a layering functor).
3. Type references — every request and response label resolves to a builtin or a defined type.
4. Port targets — every service-targeting port resolves to a defined service.
5. Inbound services — every inbound adapter drives a defined service.
6. Client services — every client adapter reaches a defined service, the mirror of the inbound check.
7. Generated drift — files on disk match a fresh render.
8. Implementation coverage — every operation has an authored handler in `rust/theseus/src/service.rs`. The trait defaults each method to `unimplemented`, so this check holds the gate the compiler once did. `theseus coverage` reports the same worklist with each gap's signature.

### Working on the self-model — the critical workflow

When you change `rust/model/src/self_model.rs` (or anything that affects the rendered output):

1. Run `cargo run -p theseus-cli -- generate` to refresh the generated code (each crate's `generated.rs` and the canonical `self_model.rs`). Skipping this fails the drift-gate test (`theseus_conforms_to_its_self_model`).
2. If you added an operation, author its handler in `impl TheseusService for Ctx` in `rust/theseus/src/service.rs`. The build stays green — the handler defaults to `unimplemented` and the result surfaces through the generated `dispatch` default — and `coverage` / `verify` report the operation until you author it. Override the output in `run()` (in the CLI's `main.rs`) only for bespoke output: an exit code, per-file lines, a follow-up notice.
3. Never hand-edit `generated.rs` (it carries a `// @generated … do not edit by hand` header).
4. An edit that changes a renderer together with authored code consuming the renderer's new output can wedge the build — `generate` runs inside the binary the workspace builds, so the files that would fix the compile are files only the broken build can produce. `cargo run -p theseus-model --bin bootstrap` regenerates from a build that stands only on the engine and the model, restoring a buildable tree.

The agent protocol mutates the model from outside. `theseus query` reports a stable handle per node and the model hash. `theseus patch --edit '<verb>|<target>|<key=value>…'` addresses a node by its handle and is repeatable, applying each edit in order (a refused edit carries a `PATCH00x` coded diagnostic + repair shape). The agent and MCP surfaces take the same edits as structured objects — the `Edit` enum (a foreign-backed rich enum in the model), rendered as a per-verb `oneOf` tool schema — while the CLI keeps the compact pipe form, decoded at its adapter through `Edit`'s `FromStr`. With `--write`, the proposed model is reprojected — `self_model.rs` and `generated.rs` re-render together — and `coverage` reports each new operation's handler as unimplemented until it is authored in `rust/theseus/src/service.rs`. An operation's `tool` attribute is its agent tool description — `patch` sets it (empty withdraws it), and an operation carrying one joins the tool catalog at the next rebuild.

The agent loop turns this protocol inward. The `agent` binary runs an LLM that drives Theseus's own operations as tools over a `Session` — a working copy of the model — so it edits the model it inspects, with writes to disk gated by `agent --allow-writes`. Its `restart` tool, answered by the loop rather than the session, rebuilds the binary and resumes the persisted transcript in it, so applied edits become the running code mid-conversation. The `mcp-server` binary exposes the same surface (the shared `tool_catalog()`) over the Model Context Protocol, so an external host drives the same `Session`. Both are covered in `docs/building-the-harness.md`.

## Conventions

These are enforced by review in this repo. Two project skills carry the full rules: `rust-style` (errors, newtypes, ownership, module organization) and `rust-hexagonal` (domain/ports/adapters/services). The skills are vendored from a larger project, so ignore their references to files (`docs/…`), crates (`smithkit`, `sqlx`, `axum`), and "Hard Rules" that don't exist here — the _patterns_ apply, the specific paths don't.

- Errors: `thiserror` typed enums in the engine crates (kernel, modeling). `anyhow` in the service and inbound crates downstream (`theseus`, `theseus-cli`, `theseus-agent`, `theseus-mcp`, and the calculator crates). No `Result<T, String>`.
- Comments state what the code IS and DOES, positively. Do not write what the code omits, lacks, or does "instead of" (no "deliberately omits X", "rather than Y"). Do not name the crate behind a concept (say "command surface", not "clap"). Prefer a full stop over a semicolon joining clauses. Keep `// @generated` markers.
- Commit hygiene: new workspace dependencies land just-in-time in their own `Cargo workspace: <category>` commit, right before the crate that first needs it. `Cargo.lock` goes in its own trailing commit, never bundled into a feature commit. Per-crate `Cargo.toml`: workspace deps first (alphabetical), blank line, then path deps. Crates are introduced already-functional. History is curated to read as a clean build-up — before any rewrite, set a backup ref and confirm `git diff <backup> HEAD` is empty.

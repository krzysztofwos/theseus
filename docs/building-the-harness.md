# Building the agent harness with Theseus only

An experiment log. The question: can Theseus grow its own agent harness — a `chat` operation whose handler drives an LLM that calls Theseus's own operations as tools — through its protocol alone (`query`/`patch`/`scaffold`/`generate`/`implement`), hand-authoring only the genuine leaves (adapters, composition-root wiring, handler bodies)?

This log records every command and every issue, including the points where the protocol stops and authoring begins.

## Plan

The harness is a second inbound projection over the one Theseus service, plus a `chat` operation that runs the loop and an `llm` outbound port the loop drives. Locked decisions: tool surface is the self-modeling operations only; the LLM provider is an offline scripted stub first; categorical depth (flow conformance) is deferred.

Phase 1 — model the surface through the protocol, then stub the handler:

- types `ChatRequest`, `ChatReply`, `LlmRequest`, `LlmReply`
- operation `chat(ChatRequest) -> ChatReply` on the Theseus service
- outbound port `llm` with method `complete(LlmRequest) -> LlmReply`
- reproject (`patch --write`), then author the `chat` stub

## Protocol surface (from reading the engine)

The batch edit grammar is `verb|target|kind=K|name=N|key=value|…`, repeatable under one hash check, applied in order.

- `add` kinds and keys: `type` (`shape=struct:f=Ty,…` | `foreign:Path` | `newtype:Ty` | `enum:A,B`), `operation`/`port` (attach to `model:<m>` or `service:<m>:<Name>`), `method` (under a `port:` handle), `field` (under a `type:` handle, `ty=`/`doc=`), `inbound`/`crate`/`dep`.
- handles: `type:m:Name`, `field:m:Type.field`, `op:m:name`, `port:m:name`, `method:m:port.method`, `service:m:Name`.
- `set` carries `doc`/`ty` on a field, `summary`/`request`/`response` on an operation or method.

## Log

### Phase 1 — model the surface

Dry-ran, then applied with `--write`, one hash-checked batch of ten edits (`patch --write --expect-model-hash <h> --edit … ×10`): the four types, the `chat` operation, the `llm` port, and the port's `complete` method. The protocol accepted all ten and reprojected `self_model.rs` and `generated.rs` together. So adding the operation, the port, the method, and the types is fully protocol-driven.

`cargo build` then surfaced two errors:

1. `cannot find type LlmRequest in crate theseus_modeling` — the generated `Llm` port trait referenced `theseus_modeling::LlmRequest`.
2. `missing field llm in initializer of Ctx` — `main.rs` constructs the composition root and does not yet pass the new `llm` port.

The second is the expected authored leaf: the protocol grows the `Ctx` slot, authoring wires the adapter.

The first is a genuine engine gap the harness is the first to hit. `rust_type` (codegen.rs) resolves every model-defined struct or enum to `theseus_modeling::<name>`. That holds for the `workspace` port only because its `GeneratedFile` has a real twin in `theseus_modeling`. A port-method type that is a locally-defined struct — like `LlmRequest`, a Theseus-crate type — has no such twin, and port-method struct types are not rendered locally at all (only operation request types are). So today a port method carries builtins or foreign types, not model-defined structs.

Decision: keep phase 1 moving by modeling the `llm` port at the minimal honest grain — `complete(String) -> String`, a transcript in and the model's next action as text out — and record the struct-typed-port-method gap as a candidate engine refinement (render method types in the owning crate, resolve their path the way operation requests already are).

### Recovery — a bad write bricks the protocol binary

Re-pointing the method meant another `patch --write`, but the first one had already reprojected a `generated.rs` that does not compile (the `theseus_modeling::LlmRequest` reference). Because `query`, `patch`, and `generate` *are* the `theseus` binary, and the binary no longer builds, the protocol is unavailable to fix the model it just broke. `patch --write` has no compile gate.

Recovery is `git checkout -- rust/cli/src/generated.rs rust/model/src/self_model.rs`: the reprojected files are version-controlled, so reverting them restores the last compiling model and the protocol with it. This is a real property of a self-rewriting tool — keep the projection under version control, and a self-inflicted break is one checkout away from recovery.

With the binary restored, the corrected batch (seven edits, `llm` as `complete(String) -> String`) applied cleanly. `cargo build` then left exactly one error: the `Ctx` wiring leaf.

### Authoring the leaves

The protocol generated the `Llm` port trait and the `Ctx.llm` slot. Two authored leaves closed the build, both in `main.rs`:

- `OfflineLlm`, a stub adapter implementing `Llm` — the hexagonal seam the protocol never crosses.
- one line wiring `llm: &llm` into the composition root.

The `chat` handler was authored through the protocol itself: `theseus implement --method chat --body-file <body> --expect-model-hash <h>` spliced a passthrough body — open a transcript from the message, call `self.llm.complete`, return the reply — into `service.rs`. The signature came from the model; only the body was supplied.

`allow_writes` was dropped before authoring: it gates mutating tools, which the passthrough has none of, so it was a field ahead of its consumer — the same dead-weight lesson as the port. It returns when tool dispatch does.

### Phase 1 result

Green: `verify` conformant, coverage 15/15, clippy clean. End to end, `theseus chat --message "Hello, Theseus."` runs the generated command surface, parser, `Invocation`, and `dispatch`, through the `chat` handler and the `llm` port, to the `OfflineLlm` adapter and back as text.

Everything between the command line and the port — command surface, request struct, trait method, port trait, composition-root slot — was generated from protocol edits. The authored leaves are exactly the two the architecture names: the adapter and its wiring. The handler body itself went in through `implement`.

Answer so far: yes, the surface of the harness is built with Theseus alone. The walls are (1) port methods cannot yet carry model-defined struct types, and (2) a write that yields non-compiling code disables the protocol until a `git checkout`. Neither blocked phase 1.

Next: the tool-dispatch loop — the model calling Theseus's own operations as tools — and a scripted `OfflineLlm` with an end-to-end test, then the real model adapter behind a write gate.

# Building the agent harness with Theseus only

An experiment log. The question: can Theseus grow its own agent harness — a `chat` operation whose handler drives an LLM that calls Theseus's own operations as tools — through its protocol alone (`query`/`patch`/`scaffold`/`generate`/`implement`), hand-authoring only the genuine leaves (adapters, composition-root wiring, handler bodies)?

This log records every command and every issue, including the points where the protocol stops and authoring begins.

## Plan

The harness is a second inbound projection over the one Theseus service, plus a `chat` operation that runs the loop and an `llm` outbound port the loop drives. Locked decisions: tool surface is the self-modeling operations only. The LLM provider is an offline scripted stub first. Categorical depth (flow conformance) is deferred.

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

The `chat` handler was authored through the protocol itself: `theseus implement --method chat --body-file <body> --expect-model-hash <h>` spliced a passthrough body — open a transcript from the message, call `self.llm.complete`, return the reply — into `service.rs`. The signature came from the model. Only the body was supplied.

`allow_writes` was dropped before authoring: it gates mutating tools, which the passthrough has none of, so it was a field ahead of its consumer — the same dead-weight lesson as the port. It returns when tool dispatch does.

### Phase 1 result

Green: `verify` conformant, coverage 15/15, clippy clean. End to end, `theseus chat --message "Hello, Theseus."` runs the generated command surface, parser, `Invocation`, and `dispatch`, through the `chat` handler and the `llm` port, to the `OfflineLlm` adapter and back as text.

Everything between the command line and the port — command surface, request struct, trait method, port trait, composition-root slot — was generated from protocol edits. The authored leaves are exactly the two the architecture names: the adapter and its wiring. The handler body itself went in through `implement`.

Answer so far: yes, the surface of the harness is built with Theseus alone. The walls are (1) port methods cannot yet carry model-defined struct types, and (2) a write that yields non-compiling code disables the protocol until a `git checkout`. Neither blocked phase 1.

Next: the tool-dispatch loop — the model calling Theseus's own operations as tools — and a scripted `OfflineLlm` with an end-to-end test, then the real model adapter behind a write gate.

### The tool-dispatch loop

The `chat` handler is now the agent loop. The model drives Theseus's own read-only operations as tools, so the loop closes onto the model it inspects.

The reply protocol is one JSON object per turn: `{"tool": name, "input": {…}}` to call a tool, or `{"answer": text}` to finish. `run_agent` opens a transcript with the framing and the user's message, then each turn parses the completion: a tool call runs through `run_tool` (a dispatch over `self`'s own operations — `model`, `query`, `verify`, `coverage`) and its JSON result is appended to the transcript. An answer ends the loop. `MAX_TURNS` guards a model that never finishes. The handler is a single line, `run_agent(self, self.llm, &request.message)`. The loop is generic over `TheseusService` and `Llm`, so it tests with doubles.

Two unit tests in `service.rs`, both offline: a `ScriptedLlm` replays a fixed list of completions, a `NoopWorkspace` stands in for the filesystem. One drives the whole loop — a `query` tool call, then an answer — and asserts the final text. The other calls `run_tool` directly and asserts the `query` result names the `chat` operation, proving the dispatch reaches a real operation and returns real data. No network, no writes.

This part is authored, not generated: the loop and the tool dispatch are the `chat` handler's behavior, which is an authored leaf. The model surface around it was protocol-built. Generating the tool schema and dispatch from the operations — the second inbound projection — remains the open engine step (phase 2 of the plan): it would replace the hand-written `run_tool` match the way the generated command surface replaced a hand-written argument parser.

The tool surface is read-only by choice. A write tool (`patch`/`generate`) closes the self-modification loop, gated by a `chat --allow-writes` permission — that, and the real model adapter, are next.

### Result

`theseus chat --message "…"` runs the loop end to end against the offline stub, which answers without tools. The scripted tests run the loop with a tool call. Green: full suite, clippy, conformant.

### The write tool and the allow-writes gate

The loop's tool surface was read-only. A `patch` tool now closes the self-modification loop: the model proposes edits as a list of `verb|target|key=value` strings — the same batch vocabulary `theseus patch --edit` takes — and the loop builds the request, stamping the current model hash so the model never tracks it. The agent edits the model it inspects.

A `patch` that writes is refused unless the chat permits it. `allow_writes` returns to `ChatRequest` — added through the protocol, now with a consumer — and surfaces as the generated `--allow-writes` flag, default off. When a write tool runs without it, the loop feeds a refusal back to the model rather than failing, so the model can adapt. This is the one irreducible permission for an agent that can rewrite its own source.

Two offline tests cover the gate, both driving `run_tool` directly with a `write: true` patch: refused without the flag (exact-matching the refusal text), applied with it. The applied case runs the real edit through a no-op workspace, which discards the reprojection — the test asserts the outcome is `ok` and the diff names the new type, and touches no files on disk.

One in-process limit, noted not fixed: a write reprojects `self_model.rs` and `generated.rs` to disk, but the running agent's in-memory model is the fixed `theseus_model()` value, so later tool calls in the same session still see the pre-write model. The write persists. A rebuild loads it. Within one process the agent cannot see its own edits reflected.

### Result

The self-modification loop is closed and gated. Offline, the suite proves a scripted model can call a read-only tool and answer, and that the write gate refuses or permits a `patch` by the flag. `theseus chat --message "…" --allow-writes` accepts the flag and runs the loop; the offline stub still answers without tools. The remaining step is the real model adapter — blocking HTTP behind the same `Llm` port — so a live model drives the loop.

### Closing the in-session staleness

The earlier limit — a write persisted to disk but the running agent kept reading the fixed `theseus_model()` value, so it could not see its own edits within a session — is now closed.

The loop clones the model into a working copy at the start of the session and threads it through the tools. Every accepted `patch` updates the working model, so a `query`, `verify`, or `model` on a later turn sees the edit. The corrected model was never far away: `apply_edits` already returns it. The loop simply keeps it as the session's model rather than discarding it.

This moves the tools off the fixed-model service methods. `run_tool` now calls the operation functions — `query`, `apply_edits`, `verify`, `coverage`, `describe` — against the working model directly. These are the same functions the CLI handlers wrap. The loop binds them to the session's mutable model rather than the composition root's fixed one. The agent's tools are still Theseus's own operations, now over a model that moves as the agent edits it.

The `write` flag keeps its meaning: persist to disk, gated by `allow_writes`. An in-memory edit always applies and is ephemeral — discarded when the process exits unless a write persists it. So an agent without the gate can still reason over hypothetical edits in memory. Only persistence to its own source is gated.

A `persist` helper now shares the reprojection between the loop and the CLI `patch` handler. The new test adds a type with no write, then queries it on a later call and finds it — the proof that the session sees its own edits.

What remains is only the cross-process step: a write reprojects source that a rebuild compiles in. Within a process the agent now sees its edits. Across a restart the rebuilt binary loads them.

### The pivot: chat is an inbound, not an operation

Modeling `chat` as one of Theseus's operations was a category error. The operations are capabilities — what the tool can do. Chat is a driver — a way to invoke those capabilities, like the CLI. It sits at the same level as the command surface, not inside it. The tell was the urge to mark `chat` so the tool catalog would exclude it from itself. A flag that says "this operation is not really an operation" is the model asking to be corrected.

So chat moved out of the service and became an inbound, beside the CLI. The one binary split into a service library and a set of inbound binaries that drive it. `theseus` (the library, L3) holds the operations, the `Session`, and the shared tool catalog. `theseus-cli` keeps the command surface. `theseus-agent` runs the loop. The model gained a second transport kind for the agent loop, and the codegen learned that only a `Cli` inbound renders a command surface — an agent or server inbound is a purely authored binary, so the model renders nothing for it.

The working model carried over unchanged. A `Session` now holds it, and both the agent loop and a server drive the same `Session::call`, so every inbound sees one tool surface with one set of semantics. The tool catalog is a curated, hand-written view of the operations — a subset, with simplified inputs — co-located with the dispatch it must agree with, and shared by every agent-driving inbound.

### The real model adapter

The `Llm` port gained a real implementation beside the offline stub. `AnthropicLlm` drives the loop over the Messages API with native tool use: the conversation and the catalog render into a request, and the reply's `tool_use` blocks come back as the tools the loop dispatches. The port stayed the seam — the loop is identical whether a scripted stub or a live model answers. Configuration is read from the environment, and the binary falls back to the offline stub when no key is set, so it runs with or without a network.

The port turned async at this step. The loop and its inbound binaries run on a current-thread runtime at the transport edge, while the core — the kernel, the engine, the service — stays synchronous. The port method returns `impl Future`, so a synchronous stub and an async HTTP adapter satisfy the same trait with no boxing.

### The second agent-driving inbound: MCP

The agent loop drives Theseus from inside its own process. A Model Context Protocol server drives it from outside. The point is the comparison: an external agent — Claude Code, say — and Theseus's own loop can drive the same tools, over the same session, and be measured against each other. For that to be fair, the two must share one surface, not two parallel implementations of it.

They do. The `mcp-server` binary, in the `theseus-mcp` crate, is a third inbound that serves `theseus::tool_catalog()` over stdio and dispatches each `tools/call` to a `Session::call` — the same catalog and the same dispatch the agent loop uses. The server implements the SDK's `ServerHandler`: `list_tools` renders the catalog into the protocol's `Tool` shape, and `call_tool` runs the named tool against the session and returns its result as text. A failed tool returns its error as the result so the host can recover, the way the loop feeds an error back into the conversation. A new transport kind models the inbound, and the write gate carries over as a launch flag — writes refused unless the server is started with `--allow-writes`.

One wrinkle shaped the design. A `Session` borrows its workspace, so it cannot be held for the server's whole lifetime behind the `'static` bound the SDK requires. The server holds the working model behind a lock instead, and reconstructs a session per call over a copy of it, reading the model back through a new `Session::into_model` so accepted edits carry into the next call. The working-model semantics are preserved across calls without the session outliving any one of them.

### Result

Three inbounds now drive one service: the CLI, the in-process agent loop, and the MCP server. A stdio handshake against `mcp-server` — initialize, `tools/list`, `tools/call` — returns the five tools with their schemas and dispatches a `query` through the session, returning the model hash and the operation handles. The catalog the external host sees is the catalog the loop sends a model. Green: full suite, clippy, conformant. What remains is to run the comparison — register `mcp-server` with an external host and drive the same edits both ways.

### The comparison, run

The comparison ran on one goal: add a newtype `Slug` over `String`, write it, and verify the workspace still conforms. The internal loop drew it first, with a real model behind the `Llm` port. The external arm drew it second — Claude Code driving `mcp-server` over the protocol. Same goal, same tools, same `Session`. The only difference is who picks the next tool.

The first internal run failed — the model thrashed for sixteen turns and gave up. The loop is opaque from outside, so the next move was to make it speak: an `AGENT_TRACE` flag that streams each turn's tool calls and results to stderr. The trace showed three faults, and only one was the model's.

It needed the model root as the parent to add a top-level type, but `query` never minted that handle — the one address it needed was the one the catalog hid. It needed the patch grammar, and the tool described itself as `verb|target|key=value` with no example, so it rediscovered from diagnostics that attributes are pipe-separated, that a newtype is a shape and not a kind, and that a shape reads `newtype:Inner` — a dozen turns on syntax alone. And when it finally added the type and verified it, the sixteen-turn budget had run out one turn before it could report success. The work had landed. The run still called itself a failure.

Three fixes followed, two of them in the shared surface. `query` mints the model root. The patch tool carries a worked example. The turn budget is larger. Because both inbounds drive the engine's `query` and the shared `tool_catalog()`, fixing those fixed both arms at once — the external agent, run after, got the syntax right on its first patch and finished in two calls. Only the turn budget belonged to the loop alone.

This is what the second inbound was for. A surface built by the person who modeled it reads as obvious to them. Put a cold agent in front of it — internal or external — and the gaps surface: the handle the lister omits, the grammar with no example. The comparison named no winner between the two agents. It used them as two probes of one surface, and the surface came out better for both.

### Growing an operation

The catalog could reshape the model but not the code beneath it. An agent could `patch` a new operation into the model, but the handler that operation needs is an authored leaf, and there was no tool to write it — so the workspace sat unimplemented and `verify` failed on the coverage gate. The agent reached the model and stopped at the source.

Two tools closed that gap. `show` returns an operation's handler, and for one not yet written it returns the generated signature instead, so the agent can read the request and response types before authoring. `implement` writes a handler body into the service impl, gated by the same write permission as a patch. The body the agent supplies is only what goes inside the braces — the splice wraps it in the signature the model dictates.

With both, the loop closes from the outside. The agent `patch`es an operation in, which reprojects the contract so the trait gains a method that defaults to unimplemented. It `show`s the operation to read the signature, `implement`s a body, and `verify`s. The model, the regenerated contract, and the new handler line up, and the workspace conforms.

It runs. Asked to add a `greet` operation, the agent attached it to the service and watched the edit reproject the self-model, the trait contract, and the CLI command surface together. It read back `fn greet(&self) -> anyhow::Result<String>`, wrote a one-line body into the service impl, and verified: fifteen operations, all implemented. No one edited the code by hand.

This is the plank replaced. The ship kept its shape — every check still passes — while a piece of it was taken out and put back by something other than the shipwright. The model describes the tool, the tool regenerates from the model, and an agent drives that loop from either side of one surface. Theseus extends itself.

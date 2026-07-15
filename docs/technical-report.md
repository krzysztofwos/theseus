# Theseus: a self-modeling agent harness

**Technical report for colleagues**  
**Date:** July 2026  
**Status:** Research prototype with live evaluation evidence; not a production multi-tenant product

---

## Executive summary

**Theseus** is a Rust system in which an LLM agent develops software—including Theseus itself—through a **modeled, verified architecture** rather than free-form file edits. The agent’s tools are the system’s own operations: inspect the architecture model, patch it, regenerate contracts, splice handlers, edit bounded authored Rust items, compile/test/verify, checkpoint and roll back, then rebuild so new code becomes live.

The central idea is a **fixed point**: a machine-readable model of the project _is_ the source of contracts, CLIs, HTTP/gRPC surfaces, and the agent tool catalog. Regeneration and ten structural checks keep the tree honest. Mutations that touch the workspace go through a **transactional write path** (lease, write-ahead log, compile gate, all-or-nothing commit). Sessions can open not only Theseus but **any durable project** identified by `theseus.json` and a JSON model record—so the same agent catalog grows foreign applications.

**Why it matters:** most coding agents treat the filesystem as a scratchpad. Theseus treats architecture as data, tools as projections of that data, and self-modification as a closed, recoverable loop. Live goals show an unassisted model shipping capabilities in both the harness and a freshly initialized foreign project.

---

## 1. Problem and thesis

### 1.1 The usual agent loop

A typical coding agent is an LLM on a loop with tools such as “read file,” “write file,” and “run shell.” That is powerful and brittle: nothing structural forces the agent’s mental model of the system to match the code, and nothing prevents half-applied edits after a crash or a failed compile.

### 1.2 Theseus’s claim

Theseus claims a stronger loop:

1. **Describe** the system as a model (services, operations, ports, crates, transports).
2. **Project** that model into contracts, surfaces, and tool schemas.
3. **Verify** that the workspace still matches the model (dependencies, layering, handlers, flows, drift).
4. **Act** only through operations that respect gates, ownership, and transactions.
5. **Recover** via exact snapshots of model-owned state.
6. **Become** the new system by rebuilding and resuming the conversation (`restart`).

Named after the Ship of Theseus: planks (capabilities) can be replaced while the ship (conformance) remains.

---

## 2. What “self-modeling” means

At the heart of the repository is a value, `theseus_model()`, authored and re-projected as `rust/model/src/self_model.rs`. It describes:

- Workspace **crates** and their **layers** (dependency direction is checked).
- **Types** exchanged by operations and ports.
- **Services** (operations + outbound ports), not tied to one transport.
- **Inbounds** (CLI, HTTP, gRPC, Agent, MCP) and optional **interior** ports (e.g. the agent’s LLM).
- **Clients** that re-implement a service contract over the wire.

From that model, `generate` renders:

- Service traits, request types, port traits, write gates, composition roots.
- CLI command surfaces and dispatch.
- HTTP handlers and gRPC protos/glue.
- Wire clients.
- Agent/MCP **tool catalog** and **tool dispatch** for operations marked with a `tool` description.
- A canonical form of the self-model itself.

Hand-authored “leaves” stay human (or agent) territory: handler bodies, real adapters (filesystem, git, cargo, Anthropic), and inbound `main` wiring. Everything else is a projection that must not be edited by hand.

**`verify` runs ten checks**, including:

| #   | Check                               | Role                                                   |
| --- | ----------------------------------- | ------------------------------------------------------ |
| 1–2 | Dependency presence and layering    | Architecture as a functor over the crate graph         |
| 3–6 | Type/port/inbound/client resolution | No dangling names                                      |
| 7   | Generated drift                     | Disk matches a fresh render                            |
| 8   | Implementation coverage             | Every operation has an authored handler                |
| 9   | Flow conformance                    | Handlers touch exactly the ports they declare (`uses`) |
| 10  | Interior coverage                   | Loop adapters implement full port methods              |

Several checks use a small **category/functor kernel** (no external CT framework)—structural laws, not decoration.

The engine (`theseus-modeling`) is **adopter-agnostic**. Theseus is one adopter; a separate **journal** workspace (`adopters/journal/`) consumes the same engine and proves reuse from outside.

---

## 3. Architecture at a glance

Roughly **17 crates** under `rust/`, ~34k lines of Rust (including generated), nightly toolchain, edition 2024.

```text
L0  kernel          finite categories / functor law
L1  modeling        model DSL, patch, verify, codegen, scaffold, splice
L1  calculator, text-utils, workspace (mutation WAL)
L2  model           theseus_model() + paths; calculator clients; bootstrap
L3  theseus         service, session, project context, adapters
L4  agent, mcp, http, grpc (+ clients)
L5  cli             `theseus` binary
```

**Doctrine:** reads are ambient (handlers may read the tree); **mutations** cross ports (`workspace`, `checkpoint`, `toolchain`, `project`, …). Write permission is an operator flag (`--allow-writes`); gated methods are refused without it. Gates are **modeled**, not hand-maintained wrappers.

**Async edge, sync engine:** generated service/port traits are async; pure modeling stays synchronous.

---

## 4. Capability surface (operations)

The Theseus service exposes operations used both as CLI subcommands and (where `tool` is set) as agent tools. Conceptually they group as follows.

### 4.1 Inspect architecture

| Operation  | Purpose                                                              |
| ---------- | -------------------------------------------------------------------- |
| `model`    | Emit the model as JSON (plus content hash via query surface)         |
| `query`    | Stable handles for nodes (`op:…`, `type:…`, `port:…`, model root, …) |
| `coverage` | Which operations still lack handlers                                 |
| `show`     | Handler or adapter source; signature if unimplemented                |
| `verify`   | Full ten-check conformance report                                    |

### 4.2 Change the model and regenerate

| Operation  | Purpose                                                          |
| ---------- | ---------------------------------------------------------------- |
| `patch`    | Structured edits (`add` / `remove` / `rename` / `set`) by handle |
| `generate` | Reproject all generated files                                    |
| `scaffold` | Create missing service crate skeletons                           |

`patch --write` is not “write some files.” It **declares a path set**, checks the **persisted projection**, acquires a **repository lease**, applies through a **WAL**, runs a **compile gate**, then **commits or restores** the entire batch (including `Cargo.lock` protection).

### 4.3 Author behavior

| Operation        | Purpose                                                                                                                                                   |
| ---------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `implement`      | Splice a handler or port-adapter method body; compile-gated                                                                                               |
| `edit_rust_item` | Insert/replace one **named top-level** Rust item in an owned authored `.rs` file, using a **complete-file revision** from `read` (stale-write protection) |

`edit_rust_item` is deliberately **not** a general editor: no arbitrary ranges, no new files, no manifests, no `impl` members, no import-only edits. That is a product decision: typed, ownership-aware mutation first.

### 4.4 Inspect source (non-modeled)

| Operation | Purpose                                                          |
| --------- | ---------------------------------------------------------------- |
| `read`    | UTF-8 file with path, revision, capped contents, truncation flag |
| `search`  | Substring hits, capped, root-confined                            |
| `list`    | Directory listing                                                |

Paths cannot escape the project root. Prefer `show` for modeled handlers; use browse tools for everything else.

### 4.5 Prove and quality-gate

| Operation | Purpose                                                       |
| --------- | ------------------------------------------------------------- |
| `check`   | `cargo check` (structured `CheckReport`: `ok` + detail)       |
| `test`    | `cargo test`                                                  |
| `lint`    | Clippy with warnings denied (agent-grown from local evidence) |

### 4.6 Recover

| Operation           | Purpose                                                                  |
| ------------------- | ------------------------------------------------------------------------ |
| `snapshot`          | Exact checkpoint of tracked + model-owned paths (modes, symlink targets) |
| `rollback`          | Restore that state transactionally; unrelated untracked files left alone |
| `diff`              | Diff vs snapshot (write-gated: builds private Git objects)               |
| `release` / `prune` | Explicit snapshot retention                                              |

Snapshots are **pinned private refs** under a project-scoped namespace, bounded in size and count, and survive ordinary GC. Live sessions distinguish **working** vs **persisted** model state so a rollback can restore both disk and in-memory model.

### 4.7 Process and project drive

| Operation | Purpose                                                                                                                    |
| --------- | -------------------------------------------------------------------------------------------------------------------------- |
| `restart` | Prove compile readiness; **agent inbound** rebuilds and resumes the transcript                                             |
| `drive`   | Rebuild and invoke a **project’s own CLI** for a modeled operation (prove a grown capability live without embedding shell) |
| `calc`    | Example port to the in-tree Calculator service                                                                             |

### 4.8 Outbound ports (dependencies of the service)

Typical ports: `project` (immutable root + layout), `workspace` (writes), `checkpoint`, `toolchain`, `calculator`, and on the agent inbound `llm`.

---

## 5. How an agent runs

### 5.1 Internal loop (`theseus-agent`)

1. Open a **project** (`--project ROOT` or Theseus itself).
2. Build a **Session** over working model + gated adapters.
3. Each turn: LLM sees system framing + transcript + **tool catalog** rendered from the model.
4. Tool calls go through `Session::call` / dispatch; failures return as tool results so the model can recover.
5. Writes require `--allow-writes`.
6. `restart` (solo tool call): rebuild binary, persist transcript under `.theseus/` (private file modes on Unix), re-exec with `--resume`.

The LLM port has an **Anthropic** adapter and an **offline scripted stub** (no API key → hermetic tests).

### 5.2 External loop (`mcp-server`)

Same catalog and session semantics over **Model Context Protocol** stdio, so Claude Code or another host can drive Theseus without reimplementing tools. That dual drive was used early to improve tool descriptions (e.g. model-root handles, patch examples).

### 5.3 Other inbounds

| Binary                        | Role                                                                             |
| ----------------------------- | -------------------------------------------------------------------------------- |
| `theseus` CLI                 | Human and script surface; `--remote` swaps in HTTP client                        |
| `http-server` / `grpc-server` | All operations on the wire; **StatefulSession** holds one locked revision stream |
| Clients                       | Same typed errors (`Unimplemented`, `Refused`) across HTTP/gRPC                  |

### 5.4 Foreign projects

A project is not “whatever directory the agent likes.” It is:

- Operator-selected **root** (launcher `--project`; **not** a tool argument).
- Strict **`theseus.json`** + canonical **JSON model record**.
- Versioned **Rust workspace layout** (path ownership for generate/scaffold/edit/checkpoint).

`ProjectContext::open` reconstructs capability **without executing project code** (no trusting a random `build.rs` as discovery).

**Initialize** (operator CLI today):

```sh
mkdir /tmp/app && git -C /tmp/app init
cargo run -p theseus-cli -- \
  --project /tmp/app init --id app \
  --modeling-path "$PWD/rust/modeling"
```

That seeds a minimal modeled service + CLI in one **transactional** apply with compile gate and cold-open before commit. The agent then develops the project with the same tools as for Theseus.

---

## 6. Safety model (what is and is not promised)

Theseus invests heavily in **mutation integrity** for a local, single-operator research harness.

**In scope and implemented (Unix):**

- Root confinement for browse tools and generated paths.
- Repository lease; durable mutation journal; drop-time rollback.
- Optimistic check of expected persisted projection before write.
- Compile gate before commit for model/source mutations.
- Stale-revision refusal for governed Rust item edits.
- Exact snapshot/restore of owned paths; bounded snapshot inventory.
- Transcript files with restrictive permissions.

**Explicitly out of scope / incomplete:**

- Multi-tenant auth or network exposure as a product.
- Hostile same-account races swapping path components mid-operation (needs fd-relative traversal).
- Windows parity for WAL/metadata restoration.
- Sandboxing Cargo build scripts.
- Long-running **foreign server processes**: `drive` rebuilds and invokes a project's CLI inbound to completion; managing a serving process (start, health, stop) awaits a process-manager contract.

Honesty about boundaries is part of the design documentation, not an afterthought.

---

## 7. Evidence: live evaluations and deterministic tests

### 7.1 Live goal corpus (`evals/`)

Goals use a real model (`AGENT_TRACE=1`). Mechanical invariants stay in `cargo test`. Summary as of 2026-07-14:

| #   | Goal                                                         | Outcome                                                                           |
| --- | ------------------------------------------------------------ | --------------------------------------------------------------------------------- |
| 1   | Add operation + handler; stay conformant                     | Green (e.g. agent-designed `diff`)                                                |
| 2   | Grow port method + adapter, restart, call live               | Green (`test`, checkpoint)                                                        |
| 3   | Snapshot → break → rollback drill                            | Green (full recovery evidence)                                                    |
| 4   | Scaffold in-tree service                                     | Green (`text-utils`)                                                              |
| 5   | Investigate subsystem citing files                           | Green (`restart` end-to-end write-up)                                             |
| 6   | Author capability from `search`/`read` evidence              | Green (`lint`)                                                                    |
| 7   | **Foreign cold project:** init seed + agent ships capability | **Strict green** (16/32 turns; `health` + test + verify)                          |
| 8   | Rebuild and call a grown capability from the agent's session | **Green** (`drive`: agent grew `clear` in the journal and proved add → 1 → clear → 0 live, no restart) |
| 9   | Initialize a foreign project from a goal string              | **Green** (loop-level `initialize`: agent chose the identity, seeded the root, shipped two operations, proved them via `drive`) |

Goals 7–9 are the product-shaped proof: _same catalog, different root, new software_ — including its bootstrap and its live acceptance. Getting 8 and 9 green surfaced four engine defects only a foreign agent could reach (enumerated scaffold re-exports, a missing JSON-renderer dependency in the scaffolded surface, transcripts lost on model-port failure, a home-sized turn budget), each fixed at the source; the eval loop doubled as the QA loop.

### 7.2 Deterministic integration

Notable tests include foreign journal sessions, transactional init of empty repos, transport session locking, mutation recovery, and round-trip clients. These prove **policy and mechanics**; live evals prove **tooling is usable by a model**.

### 7.3 Historical self-extension

Without listing every run: the agent has previously grown **test**, **checkpoint/snapshot/rollback**, and **diff** capabilities through its own tools—artifacts kept in-tree as ordinary code, not demos on a slide.

---

## 8. Recent trajectory (July 2026)

Work in early–mid July concentrated on closing the gap between “self-modifying demo” and “harness for other software”:

1. **Inspection tools** — `read` / `search` / `list` with root guards and revisions.
2. **Transactional mutations** — workspace crate, WAL, compile-gated commits.
3. **Stateful long-lived sessions** — speculative vs persisted model; HTTP/gRPC share a lock.
4. **Governed authored edits** — `edit_rust_item` with revision + ownership.
5. **Project abstraction** — `theseus.json`, layouts, `--project`, cold open.
6. **Transactional `init`** — empty Git root → minimal app.
7. **Checkpoint hardening** — exact bytes/modes, project-scoped refs, release/prune.
8. **Live goals 3 and 7** — recovery and foreign cold path green.
9. **Tool quality** — clearer schemas, boolean refusal, browse repair suggestions, formatted handlers, CLI warning cleanup.

Scale roughly: from a small self-model to **tens of operations**, multi-service examples (calculator, text-utils), multi-transport surface, and a second adopter path.

---

## 9. How this differs from “just another coding agent”

| Dimension              | Typical coding agent             | Theseus                                         |
| ---------------------- | -------------------------------- | ----------------------------------------------- |
| Architecture knowledge | Implicit in chat / RAG           | Explicit model + verify                         |
| Tool surface           | Generic FS/shell                 | Project operations projected from model         |
| Self-modification      | Edit own files opportunistically | Closed loop: patch → implement → gate → restart |
| Failed write           | Often partial tree               | Transaction rollback                            |
| New product            | Open folder, start coding        | Init durable project; same catalog              |
| Conformance            | Hope CI passes                   | Structural checks always available as a tool    |
| Transport              | One IDE host                     | CLI + agent + MCP + HTTP + gRPC, one contract   |

Theseus is **narrower** than a general IDE agent (typed edits, no free shell in the default catalog) and **deeper** on architectural honesty and mutation recovery.

---

## 10. Limitations and near-term roadmap

**Still hard / not claimed:**

- Live autonomy is not free: goals use budgeted turns; complex foreign copies may need larger budgets or resume.
- No first-class long-horizon **episodic memory DAG** (session transcripts are files; related research in a sibling _git-memory_ experiment explores Git as context store—not yet fused into Theseus).
- Long-running **foreign servers** remain a non-goal until a process-manager contract exists; one-shot rebuild-and-invoke (`drive`) and agent-visible `init` both landed in mid-July.
- Typed dependency/manifest edits and richer `impl`-member edits are natural extensions only when evals fail without them.
- Agent guidance is still mostly tool strings and static docs; **version-matched skills** and harness-wide **explain/repair codes** (beyond model `PATCH*`) are planned, not fully shipped.

**Documented next priorities** (see `docs/what-next.md` and **`docs/agent-surface-plan.md`**):

1. Agent-surface polish inspired by peer review of Zerolang *patterns* (not the language): skills from the running binary, diagnostics-as-repair-contracts, optional model-hash CAS, outline inspection, gate-trust framing, automated eval runner, command contracts.
2. Typed source/manifest expansion only when a recorded eval fails without it.
3. Process-manager contract for serving inbounds; fd-relative filesystem publication as hardening.

---

## 11. Trying it (operators)

From a clean checkout (nightly Rust):

```sh
cargo test
cargo run -p theseus-cli -- verify
cargo run -p theseus-cli -- query
# Agent (needs ANTHROPIC_API_KEY for live model; otherwise offline stub):
cargo run -p theseus-agent -- "what can you do?"
cargo run -p theseus-agent -- --allow-writes "…"
```

Foreign project sketch:

```sh
mkdir /tmp/theseus-app && git -C /tmp/theseus-app init
cargo run -p theseus-cli -- \
  --project /tmp/theseus-app init --id theseus-app \
  --modeling-path "$PWD/rust/modeling"
cargo run -p theseus-agent -- \
  --project /tmp/theseus-app --allow-writes \
  "add a health operation, test it, and leave the project conformant"
```

Prefer disposable repos or branches for write-enabled runs; use `snapshot` before exploratory mutation.

---

## 12. Related reading in-repo

| Document                                                   | Content                                      |
| ---------------------------------------------------------- | -------------------------------------------- |
| `README.md`                                                | Product thesis and protocol overview         |
| `CLAUDE.md`                                                | Working map for developers and coding agents |
| `docs/building-the-harness.md`                             | Experiment log of growing the agent surface  |
| `docs/second-adopter.md`                                   | Engine reuse via journal                     |
| `docs/what-next.md`                                        | Current status, boundaries, next work        |
| `docs/agent-surface-plan.md`                               | Phased implementation plan (agent UX polish) |
| `docs/technical-report-abstract.md`                        | Email-length brief                           |
| `evals/README.md`                                          | Live goal table and traces                   |
| `docs/building-a-calculator.md` / `adding-an-operation.md` | How-to guides                                |

---

## 13. Bottom line for colleagues

Theseus is a **clean-room research system** answering: _what if the agent’s tools, the system architecture, and the conformance checks were the same object?_

It delivers:

- A **reusable modeling engine** and a **self-describing adopter**.
- A **multi-transport** surface with one contract.
- A **self-modifying agent** that has grown real capabilities and left them in the tree.
- **Transactional, compile-gated** mutation of model and governed source.
- **Project-rooted** development of foreign Rust services, with live goals 7–9 evidence (cold growth, `drive`, agent init).

It does **not** claim production multi-user security, full general-purpose editing, or that every LLM run will succeed without budget and good tool descriptions. What it _does_ claim is checked by `verify`, integration tests, and recorded live evals—not by aspiration alone.

If the industry question is “what is an agent?”, Theseus’s working answer is operational:

> **An agent is a loop with a durable project identity, a structured action surface, recoverable state transitions, and the ability to change its own means of acting—while remaining subject to mechanical proof.**

---

### Appendix A — Review note on repository state (for maintainers)

As of the 2026-07-14 revision:

- **The committed line matches every capability above**, including `drive`, the loop-level `initialize`, and the full nine-row corpus; the workspace is green (396 tests, ten checks conformant, clippy clean).
- Untracked local helpers (`Makefile`, `run.sh`, `.env`) may exist and are not part of the product surface.

### Appendix B — Suggested one-slide pitch

1. **Model is the product schema** → generate contracts and agent tools.
2. **Verify is continuous architecture CI** as an operation.
3. **Agent edits model + leaves, not random files.**
4. **Transactions + snapshots** make self-modification recoverable.
5. **Same catalog develops other projects** after durable init.
6. **Evidence:** live green goals 1–9, including foreign cold path, `drive`, and agent init.

Next implementation track: `docs/agent-surface-plan.md`.

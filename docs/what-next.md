# What to tackle next

Strategic status for Theseus as a self-modifying agent harness: the smallest complete set of tools an agent can use to inspect, change, prove, recover, and extend its own harness or another modeled Rust project.

## July 2026 checkpoint

The safety foundation, foreign-project workflow, and live foreign evals are in place.

- **Structured inspection:** `read` returns `{path, revision, contents, truncated}`, where the revision covers the complete file even when the returned contents are capped. `search` and `list` are bounded, root-confined discovery operations.
- **Governed authored edits:** `edit_rust_item` inserts or replaces one named top-level Rust item in an existing authored file owned by the active layout. It uses the revision returned by `read`, parses all three source states, and commits only after `cargo check --workspace --all-targets` passes under the repository lease. The response distinguishes a committed edit from a compile-gated rollback.
- **Transactional model edits:** `patch --write`, `generate`, `scaffold`, and `implement` declare their path sets, verify the persisted model revision, publish through the durable WAL, capture `Cargo.lock`, and either commit the whole batch or restore it.
- **Structured failure surfaces:** renderer validation returns typed diagnostics instead of unwinding, toolchain operations return structured reports, and user-controlled checkpoint values cannot be interpreted as Git options. Model `patch` refusals carry coded diagnostics and repair shapes; harness-wide codes and `explain` are planned (see agent-surface plan).
- **Durable project open:** a strict `theseus.json` identifies a stable project ID, a versioned Rust layout, and a canonical JSON model record. `ProjectContext::open` reconstructs the project without executing project code. CLI, agent, MCP, HTTP, and gRPC launchers accept `--project ROOT`; stateful servers keep one locked project session across requests.
- **Transactional project initialization:** operator CLI `init` and the agent loop-level initialize affordance (root pinned by launcher flags; model chooses identity) seed an empty top-level Git repository with a minimal modeled service and CLI. The seed is compile-gated inside a WAL transaction and cold-opened before commit.
- **Live proof of foreign software:** `drive` rebuilds and invokes a project’s own CLI for a modeled operation under the repository lease. Live goals 7–9 are green: cold capability growth, live drive of a grown op, and agent-visible init from a goal string (`evals/README.md`).
- **Recovery:** raw-tree snapshots preserve regular-file bytes and modes plus symlink targets, include tracked and model-owned present/absent paths, and restore through the WAL. Immutable snapshot commits remain pinned by project-scoped refs until explicit release or pruning.
- **Foreign deterministic proof:** journal and initialized-project integration tests cold-open, mutate, compile-gate, verify, and roll back through the public session API.

Deterministic tests prove policy and mechanics. Live outcomes remain separate in `evals/README.md`.

## Peer lessons (Zerolang review)

A review of the peer experiment **Zerolang** (agent-first language: program graph as compiler input, `.0` as human projection) concluded that Theseus should **adopt agent-surface patterns**, not the language or a Rust program-graph IR.

| Peer pattern | Theseus stance |
| ------------ | -------------- |
| Version-matched skills from the running tool | **Adopt** — Phase 1 of the agent-surface plan |
| Diagnostics as repair contracts (`explain`, fix plans, safety labels) | **Adopt** for harness failures beyond `PATCH*` — Phase 2 |
| Optimistic edit CAS (`expect graphHash`) | **Adopt** selectively (`expect_model_hash` / session stamp) — Phase 3 |
| Token-efficient views (`outline`, around-symbol) | **Adopt** on `read`/`show` — Phase 4 |
| Trust validated writes; don’t re-check for ceremony | **Adopt** in framing and skills — Phase 5 |
| Sandboxed / automated eval runner | **Adopt** — Phase 6 |
| Command contracts against CLI/tool drift | **Adopt** — Phase 7 |
| Graph IR / binary store / rewrite-by-example language IR | **Reject** for Theseus |
| Auto-applied fix plans | **Reject** (plans only) |

Full sequencing, acceptance criteria, and non-goals: **`docs/agent-surface-plan.md`**.

## Current order

### 1. Agent-surface polish (primary track)

Execute `docs/agent-surface-plan.md` in this merge train:

1. **Skills** — `theseus skills` / `skills get` (and optional agent tool) embedded with the binary; topics: workflow, model, source, diagnostics, project.
2. **Diagnostics** — stable harness codes + `explain`; map gate/stale/ownership failures into repair-shaped results.
3. **Framing** — teach gate trust; reserve `check` for when no fresh gated result exists.
4. **Inspection** — outline / narrower source views.
5. **CAS** — optional `expect_model_hash` on patch; session-consistent stamping.
6. **Eval automation** — list/run goals; deterministic acceptance in CI; live runs isolated and recorded.
7. **Command contracts** — catalog and outcome schema guards.

### 2. Expand authored reach only where an eval fails

`edit_rust_item` remains intentionally narrow (named top-level items in owned `.rs` files). Do not schedule general writes. Pull only when a recorded goal cannot complete safely:

- `impl` member edit
- new authored file creation under layout ownership
- manifest / dependency edit

Each expansion keeps revision CAS, path authorization, WAL, lockfile capture, and all-target compile gate—and updates skills/diagnostics in the same change.

### 3. Process manager for long-lived servers

`drive` covers one-shot rebuild-and-invoke for project CLIs. HTTP/gRPC/MCP preserve session state but do not hot-load binaries. A process-manager contract is still required for:

- draining or rejecting calls while a rebuild is pending
- rebuilding the correct binary from the selected root
- replacing the process after a successful build
- long-lived **foreign servers** (start / health / stop), not only CLI one-shots

Do not infer hot reload from stateful request handling.

### 4. Filesystem race boundary (hardening)

The WAL validates paths, types, hardlinks, bounds, and internal state. Pathname traversal still assumes no hostile same-account parent swaps. Closing that requires descriptor-relative open/publish (`openat2` or constrained `openat`), not more string canonicalization. Track as P2 hardening unless a concrete threat model changes.

## Explicit boundaries

- Workspace mutation and exact metadata restoration are Unix-only. Regular-file permission bits are preserved; ownership, ACLs, extended attributes, and timestamps are not.
- Same-account pathname replacement remains outside the threat model until fd-relative publication lands.
- Project discovery accepts only the strict manifest plus a canonical JSON model projection. A Rust-builder model remains a trusted, compiled Theseus bootstrap path and is deliberately not executed by `ProjectContext::open`.
- The project root is selected by the operator (launcher flags) and is not a free tool argument for switching projects mid-session.
- Initialization accepts an operator-selected local `theseus-modeling` path as trusted input. Cargo build scripts and compiler descendants are not sandboxed.
- The authored editor can change only an existing, model-owned, authored Rust file for supported top-level item kinds. Generated projections, control records, and arbitrary paths are refused.
- A complete-file source revision prevents stale cooperative writes, not a hostile process changing the file during path resolution.
- Drop-time WAL rollback is synchronous but cannot report its failure to a canceled caller.
- Snapshot inventories and sizes are bounded; unsupported Git modes fail closed.
- Unrelated untracked files remain outside rollback by design.
- `diff` is write-gated because it creates temporary Git objects.
- Live LLM success is not implied by deterministic session tests; treat eval traces as evidence, not CI flakes.

## Working workflow

From the Theseus repository:

```sh
mkdir /tmp/theseus-app
git -C /tmp/theseus-app init
cargo run -p theseus-cli -- \
  --project /tmp/theseus-app init --id theseus-app \
  --modeling-path "$PWD/rust/modeling"
cargo run -p theseus-cli -- --project /tmp/theseus-app verify
cargo run -p theseus-agent -- \
  --project /tmp/theseus-app --allow-writes \
  "add a health operation, test it, and leave the project conformant"
```

For an existing durable adopter:

```sh
cargo run -p theseus-cli -- --project adopters/journal verify
cargo run -p theseus-agent -- \
  --project adopters/journal --allow-writes \
  "add a count operation and leave the project conformant"
```

Run write-enabled sessions on a disposable branch or repository. The write gate authorizes mutation; it does not replace a recovery point—`snapshot` before exploratory change.

## Cold-project milestone

Recorded strict green on 2026-07-12 (goal 7) and extended by goals 8–9 (drive + agent init):

> A live agent, rooted in a foreign project and given a goal, ships capability through the catalog, leaves the project conformant, and can prove behavior (`drive`) or restore snapshots without manual source edits.

Traces and acceptance notes: `evals/README.md`, `evals/goal-7-2026-07-12.md`.

## Related docs

- `docs/agent-surface-plan.md` — **implementation plan** for skills, diagnostics, CAS, evals automation, contracts
- `docs/technical-report.md` / `docs/technical-report-abstract.md` — shareable overview
- `docs/building-the-harness.md` — experiment log for growing the agent surface
- `docs/second-adopter.md` — foreign-root and cold-open integration proof
- `docs/affordance-projector.md` — contract versus surface affordances
- `evals/README.md` — live goal results, distinct from deterministic tests
- `README.md` / `CLAUDE.md` — product thesis and working map

# What to tackle next

Strategic status for Theseus as a self-modifying agent harness: the smallest complete set of tools an agent can use to inspect, change, prove, recover, and extend its own harness or another modeled Rust project.

## July 2026 checkpoint

The safety foundation and the first foreign-project workflow are implemented.

- **Structured inspection:** `read` returns `{path, revision, contents, truncated}`, where the revision covers the complete file even when the returned contents are capped. `search` and `list` are bounded, root-confined discovery operations.
- **Governed authored edits:** `edit_rust_item` inserts or replaces one named top-level Rust item in an existing authored file owned by the active layout. It uses the revision returned by `read`, parses all three source states, and commits only after `cargo check --workspace --all-targets` passes under the repository lease. The response distinguishes a committed edit from a compile-gated rollback.
- **Transactional model edits:** `patch --write`, `generate`, `scaffold`, and `implement` declare their path sets, verify the persisted model revision, publish through the durable WAL, capture `Cargo.lock`, and either commit the whole batch or restore it.
- **Structured failure surfaces:** renderer validation returns typed diagnostics instead of unwinding, toolchain operations return structured reports, and user-controlled checkpoint values cannot be interpreted as Git options.
- **Durable project open:** a strict `theseus.json` identifies a stable project ID, a versioned Rust layout, and a canonical JSON model record. `ProjectContext::open` reconstructs the project without executing project code. CLI, agent, MCP, HTTP, and gRPC launchers accept `--project ROOT`; stateful servers keep one locked project session across requests.
- **Transactional project initialization:** `theseus --project ROOT init --id ID` seeds an empty top-level Git repository with a minimal modeled service and CLI, generated projections, authored adapter leaves, a workspace manifest, a canonical model record, and a project manifest. The seed is compile-gated inside the same transaction and cold-opened before commit. Interrupted creation is retried only when the WAL proves the exact seed targets were all newly created; the proof is rechecked under the repository lease.
- **Recovery:** raw-tree snapshots preserve regular-file bytes and modes plus symlink targets, include tracked and model-owned present/absent paths, and restore through the WAL. The current layout owns root `Cargo.toml` and `theseus.json`; legacy descriptors preserve their frozen ownership. Immutable snapshot commits can start from an unborn `HEAD` and remain pinned by project-scoped refs until explicit release or pruning.
- **Foreign proof:** the journal test cold-opens a durable project, proves root binding, adds and implements a modeled operation, rejects a bad authored test module through the compile gate, accepts a good one, checks, tests, verifies, rolls back byte-exactly, and cold-opens again. The initialized-project test performs the corresponding workflow from an empty Git repository.

These claims are deterministic integration coverage. They are not evidence that a live model can complete the same foreign-project goal unaided; live outcomes remain separate in `evals/README.md`.

## Current order

### 1. Run the cold foreign-project eval live

The highest-value next result is no longer another primitive. It is a real-model run over a newly initialized project:

1. An operator creates an empty Git repository and invokes `theseus ... init`.
2. The agent starts with `--project ROOT --allow-writes` and only the catalog plus a goal.
3. It reads local source, patches the model, implements the operation, edits a top-level authored item if needed, tests, verifies, and snapshots or rolls back.

Record the trace, turn count, failed tool calls, manual interventions, and final conformance. After the agent finishes, run the produced command as a separate deterministic acceptance step. The current foreign agent cannot rebuild and invoke its newly generated operation from inside the old process, and initialization is operator bootstrap rather than an agent tool; the eval must not claim either capability until those boundaries change.

### 2. Expand authored reach only where the eval fails

`edit_rust_item` is intentionally narrower than a text editor. It handles named top-level functions, modules, structs, enums, traits, type aliases, constants, and statics in existing layout-owned `.rs` files. It does not currently handle:

- `use`, `extern crate`, or macro items
- methods inside an existing `impl`
- arbitrary byte ranges or non-Rust files
- creation of a new source file
- `Cargo.toml`, `theseus.json`, or other control records

Keep expansion typed and ownership-aware. Likely next slices are a governed `impl`-member edit and declared creation of a new authored Rust file. Each should retain stale-read detection, exact path authorization, a bounded parser input, WAL publication, lockfile capture, and an all-target compile gate. Add a general write only if a concrete eval cannot be expressed safely through a typed operation.

### 3. Define process replacement for every long-lived inbound

HTTP, gRPC, and MCP now preserve a locked session across calls, but a process still starts from the context its launcher opens. Only the internal Theseus agent owns a rebuild, transcript persistence, `exec`, and resume handoff, and that restart path intentionally refuses a foreign project. A process manager needs an explicit contract for:

- draining or rejecting new calls while a rebuild is pending
- persisting the project/session identity and write policy
- rebuilding the correct binary from the selected root
- replacing the process only after a successful build
- resuming or invalidating in-flight transport state

Do not infer hot reload from stateful request handling; these are separate properties.

### 4. Automate the live corpus

The goal table is useful but still manually run. Add one command that executes selected goals with trace capture, records model/provider and harness revision, checks final deterministic invariants, and compares results without treating model variance as a unit-test failure. Keep mechanical concurrency, rollback, parser, and transport laws in `cargo test`.

### 5. Narrow the remaining filesystem race boundary

The WAL validates paths, file types, hardlink counts, bounds, and internal state, but pathname traversal and publication still assume no hostile same-account process swaps a parent directory between checks. Closing that boundary requires descriptor-relative traversal and publication (`openat2` or a carefully constrained `openat` design), not more string canonicalization.

## Explicit boundaries

- Workspace mutation and exact metadata restoration are Unix-only. Regular-file permission bits are preserved; ownership, ACLs, extended attributes, and timestamps are not.
- Same-account pathname replacement remains outside the threat model. The selected project root is revalidated, but individual opens are not yet descriptor-relative end to end.
- Project discovery accepts only the strict manifest plus a canonical JSON model projection. A Rust-builder model remains a trusted, compiled Theseus bootstrap path and is deliberately not executed by `ProjectContext::open`.
- The project root is selected by the operator and is not a tool argument. A session cannot switch root, project ID, layout, or model-record location.
- Initialization requires an existing empty canonical Git top level. It accepts an operator-selected local `theseus-modeling` crate path and Cargo dependency graph as trusted code. Cargo build scripts and compiler descendants are not sandboxed.
- Initialization is a bespoke CLI command, not a modeled session operation. A running agent can develop an initialized project but cannot currently create a project root itself.
- The authored editor can change only an existing, model-owned, authored Rust file. Generated projections, tests outside that ownership set, foreign paths, control records, and new paths are refused. Supported top-level item kinds are bounded explicitly; this is not a general-purpose source rewrite.
- A complete-file source revision prevents stale cooperative writes, not a hostile process from changing the file during path resolution. The transaction rechecks declared state before publication and fails closed on mismatch.
- Drop-time WAL rollback is synchronous but cannot report its failure to a canceled caller. The next lease retries recovery and refuses further writes if recovery cannot complete.
- Snapshot manifests and contents are bounded: 4 MiB per manifest, 64 MiB per blob, 256 MiB aggregate, 4,096 paths, and 1,024 retained snapshots per project. Unsupported versions, project mismatches, submodules, unmerged entries, and tree modes other than regular files or symlinks fail closed.
- Current layout descriptors own the root workspace and project manifests. Version-one descriptors remain readable under their original ownership derivation; compatibility does not silently reinterpret an old snapshot as the current layout.
- Snapshot ownership is the union derived from the frozen snapshot model and current persisted model, not a permanent provenance ledger. Unrelated untracked files remain outside rollback by design.
- `diff` is write-gated because it creates a temporary index and private alternate object store. It does not change source paths, but it does write Git objects.
- Snapshot and diff objects are quarantined before ref publication. A crash in the narrow promotion-before-ref window can leave valid unreachable loose objects for ordinary Git garbage collection.
- Source changes become executable only after rebuilding the relevant process. Stateful HTTP/gRPC/MCP sessions preserve model state; they do not hot-load newly compiled code.
- The internal `restart` flow rebuilds only the Theseus harness project. Foreign projects currently require an external build/run handoff.
- Live LLM success is not implied by deterministic session tests. Goal 7 remains unrun live until its trace is recorded.

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

Run those commands on a disposable branch or repository. The write gate authorizes mutation; it does not replace a recovery point, so snapshot before an exploratory change.

## Cold-project milestone

This milestone ran strict green on 2026-07-12:

> A live agent, rooted in a freshly initialized project and given only a goal, ships a small capability using the catalog, leaves the project conformant, and can restore its starting snapshot without manual source edits; an explicit acceptance step then runs the produced CLI.

The unassisted run used 16 of 32 turns. It retained its pre-write snapshot, authored a real Tokio test, left the project warning-free and conformant, and passed the literal external CLI acceptance. The trace and acceptance hashes are recorded in `evals/goal-7-2026-07-12.md`.

The next work should follow the remaining observed boundaries: a typed dependency/manifest edit for capabilities that need new crates, machine-enforced snapshot and proof discipline rather than prompt-only sequencing, and an explicit process-manager contract for foreign rebuilds. Agent-visible initialization is useful after those boundaries, but adding more transports does not unlock them.

## Related docs

- `docs/building-the-harness.md` — experiment log for growing the agent surface
- `docs/second-adopter.md` — foreign-root and cold-open integration proof
- `docs/affordance-projector.md` — contract versus surface affordances
- `evals/README.md` — live goal results, distinct from deterministic tests
- `README.md` / `CLAUDE.md` — product thesis and working map

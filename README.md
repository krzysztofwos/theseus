# Theseus

A self-modeling CLI seed, named for the Ship of Theseus: it holds a model of its own architecture and regenerates its own code from that model, plank by plank, while every conformance law keeps passing.

The whole system is a fixed point. `rust/model/src/self_model.rs` is a Rust value that describes the very workspace that compiles it — its crates and their layers, its services and operations, its ports and adapters, its transports, and the interior of its own agent loop. `theseus generate` renders that value back into code: the service contracts, the request types, the port traits with their write gates and borrowed-adapter forwarders, the composition roots, the CLI surface, the HTTP handlers, the gRPC proto and glue, the wire clients, the agent tool catalog — and `self_model.rs` itself, canonically, so the source of the model is one of the model's own projections.

`theseus verify` holds the workspace to that model with ten checks, several realized as functors over a small category-theory kernel: required dependencies exist, dependencies descend a layer preorder, type references resolve, port targets and adapter bindings name real services, generated files match a fresh render, every operation has an authored handler, every handler reaches exactly the ports its operation declares, and every adapter of the agent loop's interior authors its full port. The hand-authored code is exactly the leaves the architecture names — handler bodies, adapter methods, composition roots — and everything else is a projection that regenerates without touching them.

## The protocol

The model is edited through a small agent protocol rather than a text editor. `query` mints a stable handle for every node and a content hash for the whole model. `patch` applies verb-per-edit changes addressed by those handles — add an operation, hang a port on an inbound, gate a method, retune the loop's turn budget — refusing anything malformed with a coded diagnostic and a repair shape. With `--write`, the complete projection is checked against its persisted revision, journaled, written under a process-independent repository lease, compile-checked, and either committed as one batch or restored. `show` reads an operation's handler or a port method's adapter, returning the generated signature when nothing is authored yet. `implement` splices a handler or adapter body through the same transaction and returns the compile decision structurally. `coverage` lists what remains unauthored, and `check` and `test` prove the tree. `snapshot` pins an exact raw-tree checkpoint, `rollback` restores it through the same write-ahead log, and `release` or `prune` explicitly retires refs. `diff` is write-gated because constructing its comparison tree writes temporary Git objects.

The transaction and checkpoint engine currently supports Unix filesystems. It rejects traversal, hardlinks, overlapping targets, stale model projections, and unsafe journal files. Checkpoints bypass Git filters and retain raw regular-file bytes, Unix permission modes, and symbolic-link targets. Their inventory covers tracked paths plus the snapshot and current persisted models' owned paths, so files absent from the snapshot become transactional tombstones on rollback while unrelated untracked files remain untouched. Each snapshot is a distinct, immutable commit pinned under `refs/theseus/snapshots/<full-object-id>` and paired with ordering metadata that is deterministic among retained snapshots. It survives Git garbage collection, and snapshots are never evicted automatically.

The supported snapshot schema is versioned and bounded: manifests are at most 4 MiB, individual blobs 64 MiB, aggregate contents 256 MiB, inventories 4,096 paths, and the harness retains at most 1,024 snapshots. Version one freezes every model wire shape and its ownership policy so later binaries must provide an explicit migration. File contents are binary, but workspace paths must be UTF-8; submodules and Git tree modes other than regular files and symlinks are refused. `diff` escapes invalid UTF-8 and non-layout control bytes in its text response. Snapshot and diff objects are built in private quarantine stores, while a second lease in the canonical Git object database serializes linked worktrees and recovers interrupted promotion copies. The pathname checks still assume no hostile same-account process swaps parent directories during an operation. These are explicit safety boundaries, as are the WAL's inability to report a drop-time rollback failure to a canceled caller and the trust placed in Cargo descendants to stop mutating source when canceled.

## The loop

The `agent` binary turns the protocol inward: an LLM drives those same operations as tools over a working copy of the model, behind a generated `Llm` port with an Anthropic adapter and an offline stub. Writes are gated by `--allow-writes`. A rollback adopts the model stored in its snapshot inside the live session; `restart` remains the boundary that rebuilds the workspace and resumes the persisted conversation inside the new binary, so restored or edited source becomes running code. Starting an already-built server binary does not reconstruct a newer model from disk, so process replacement must rebuild first. The `mcp-server` binary exposes the identical tool surface over the Model Context Protocol, so an external agent drives the same session the internal loop does. The loop's own shape — its model port, its turn budget — lives in the self-model and is edited through the same protocol as everything else.

This is not hypothetical. The agent has extended itself end to end three times, each kept in the tree: it gave itself a `test` capability (a port method, its cargo adapter, and the operation over it), built its own checkpoint capability (snapshot and rollback, with the gate policy argued and authored), and — given only the goal, with every design decision left open — designed and shipped a `diff` capability, then demonstrated it live after restarting into the binary that contained it. Each run went patch, author, check, test, verify, restart, use.

## Trying it

```sh
cargo run -p theseus-cli -- verify     # ten checks against the self-model
cargo run -p theseus-cli -- model      # the model as JSON, with its hash
cargo run -p theseus-cli -- query      # every addressable node
cargo run -p theseus-cli -- coverage   # the unauthored-handler worklist
cargo run -p theseus-cli -- calc --expression '(2 + 3) * 4'
```

The `theseus` CLI, the HTTP and gRPC servers, and the wire clients all carry the one contract; `theseus --remote <URL>` drives a remote instance through every subcommand unchanged. The agent needs `ANTHROPIC_API_KEY` (it falls back to a scripted offline stub without one):

```sh
cargo run -p theseus-agent -- "what can you do?"
cargo run -p theseus-agent -- --allow-writes "give yourself a new capability…"
```

## The layout

Sixteen crates under `rust/`, strictly layered: a category/functor kernel that knows nothing of Theseus, a model-generic engine (vocabulary, hashing, verification, code generation, the patch protocol, source splicing), the concrete self-model, the Theseus service with its shared adapters, and the inbounds and clients — CLI, agent loop, MCP server, HTTP and gRPC servers and their clients — plus two more services: the calculator, the worked example, and the text utilities, grown whole by the agent. `CLAUDE.md` carries the working map; `docs/building-the-harness.md` is the experiment log, entry by entry; `docs/adding-an-operation.md` and `docs/building-a-calculator.md` are the guided walkthroughs.

## Status

A clean-room experiment in self-modeling systems and agent harnesses, built from scratch with no external categorical framework. The engine's reusability is proven by a second adopter — a journal service in `adopters/journal/`, a workspace the self-model knows nothing about, verified by the same ten checks (`docs/second-adopter.md`). The interesting claims are the checked ones: run `verify` and read the ten lines.

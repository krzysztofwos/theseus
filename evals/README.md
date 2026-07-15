# The goal corpus

Live evals for the agent harness: each goal is run with a real model behind the `Llm` port, its trace kept, and its result recorded here. The mechanical loop invariants (restart interception, gate refusals, resume shapes, transaction recovery, and foreign-project workflows) live in `cargo test`; these goals branch on model judgment, so they only mean anything live.

Run a goal with `AGENT_TRACE=1 cargo run -p theseus-agent -- --allow-writes "<goal>"` and record: date, turns, outcome, findings.

For a durable foreign project, add `--project ROOT`. Initialization is currently an operator CLI bootstrap, so create the seed before starting goal 7:

```sh
mkdir /tmp/theseus-eval && git -C /tmp/theseus-eval init
cargo run -p theseus-cli -- \
  --project /tmp/theseus-eval init --id eval-app \
  --modeling-path "$PWD/rust/modeling"
AGENT_TRACE=1 cargo run -p theseus-agent -- \
  --project /tmp/theseus-eval --allow-writes \
  "add a health operation, test it, and leave the project conformant"
```

## The runner

The corpus is also a command. [`rust/evals`](../rust/evals) (binary `evals`) is the runnable projection of this table:

```sh
cargo run -p theseus-evals -- list     # the goals, their kind and acceptance
cargo run -p theseus-evals -- show 7   # goal 7's full prompt
cargo run -p theseus-evals -- run 7    # goal 7's deterministic acceptance, no API key
cargo run -p theseus-evals -- run 7 --live --allow-writes   # drive goal 7 with a real model
cargo run -p theseus-evals -- check    # every goal's deterministic acceptance
```

Deterministic acceptance proves each goal's artifact survives — an agent-grown operation still in the model, a foreign integration test still green — with no model in the loop. A live run stays operator-driven (a real model, budgeted turns), seeds an isolated root for a foreign goal, and records its trace and a result row under `evals/runs/`.

`evals check` runs on every push and pull request through [CI](../.github/workflows/ci.yml), so a change that drops a grown capability or breaks a foreign integration test fails the build. Live model runs are never in CI.

| #   | Goal                                                                                 | Proves                                    | Last run                                   | Outcome                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
| --- | ------------------------------------------------------------------------------------ | ----------------------------------------- | ------------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 1   | Add an operation with a handler; leave the workspace conformant                      | The model loop                            | 2026-07-06 (diff)                          | green — designed and shipped `diff`, ~27 turns                                                                                                                                                                                                                                                                                                                                                                                                                                                                                         |
| 2   | Grow a port method + adapter, restart, call it live                                  | Full self-modification                    | 2026-07-03 (test), 2026-07-04 (checkpoint) | green — both kept in-tree                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              |
| 3   | Snapshot, break something, roll back                                                 | Recovery                                  | 2026-07-12                                 | green — the full ten-step drill with evidence at each step: snapshot, deliberate handler corruption, restart, damage proven live, rollback, zero-byte `diff`, restart, recovery proven live; it also released its snapshot unprompted, and the framing now reserves retention to the operator                                                                                                                                                                                                                                          |
| 4   | Scaffold an in-tree service, author it, verify                                       | Multi-service                             | 2026-07-09 (text-utils)                    | green — exposed scaffold/generate itself first                                                                                                                                                                                                                                                                                                                                                                                                                                                                                         |
| 5   | Explain a subsystem end to end, citing files                                         | Investigation over `read`/`search`/`list` | 2026-07-10                                 | green — a fully cited end-to-end account of `restart` (model → codegen → generated catalog/dispatch → handler → loop → resumed binary), every claim tied to a file it read; systematic `list`/`read`/`search`/`show` exploration; trace in the session scratchpad                                                                                                                                                                                                                                                                      |
| 6   | Author a capability from local evidence gathered by `search`+`read`, not only `show` | Reading before writing                    | 2026-07-10                                 | green — `lint` built end to end; ~20 searches and several reads before authoring (CLAUDE.md, rust-toolchain.toml, the neighboring adapters), evidence cited in the report; live `lint` returned clean from the rebuilt binary; one authored seam (`StatefulSession`, added the same day) needed a hand delegation, caught by its completeness test                                                                                                                                                                                     |
| 7   | Grow a capability in a freshly initialized foreign project                           | Other software through the same catalog   | 2026-07-12                                 | strict green - `health` shipped in one unassisted leg (16/32 turns, 17 tool calls): snapshot before the first write, model patch, formatted handler, governed Tokio test, `test`, and `verify`. The agent snapshot remained pinned; cold-process check/test/verify/coverage, exact named test, Clippy with warnings denied, and literal CLI output `ok` all passed. [Full run record](goal-7-2026-07-12.md). The larger journal-copy variant took three resumed budgets, so a per-project or per-goal `turns` override remains useful. |
| 8   | Rebuild and call a new capability from the foreign agent process                     | Foreign process replacement               | 2026-07-13                                 | green — `drive` projects one modeled operation as its `Cli` inbound's exact command line (crate, binary, subcommand, and kebab flags from the model; only field values are the caller's), rebuilds through `cargo run` under the repository lease, and is write-gated because running project code is an effect. The agent grew `clear` in the journal (port method, adapter, operation, handler) and proved it live in one session: `add` → count `1` → `clear` → count `0`, 21 of 32 turns, no restart — foreign capabilities rebuild through the drive itself |
| 9   | Initialize a foreign project from a goal string                                      | Agent-visible bootstrap                   | 2026-07-14                                 | green — from an empty git repository and a goal string: the loop's `initialize` affordance (root operator-pinned by `--project --init`, engine path launcher-pinned, only the identity the model's — it chose `shopping-list` unaided), then a store port with two methods, two operations, adapters and handlers, all proven live through `drive`: three adds and a numbered list, ten checks conformant, 33 of 64 turns. The road there surfaced and fixed four defects: enumerated scaffold re-exports strangling grown request types (now `pub use generated::*`), the scaffolded surface missing its JSON renderer dependency, lost transcripts on model-port failure (now `Outcome::Interrupted`), and the home-sized turn budget (now 64, retuned through the protocol itself) |

## Deterministic prerequisites

These tests make the environment for goal 7 reproducible and remain its required regression coverage:

- `cargo test -p theseus --test foreign_project` cold-opens the journal manifest, proves root-bound Cargo, patches and implements an operation, rejects a compile-failing governed Rust item edit, accepts a valid one, checks, tests, verifies, restores exact owned state, and cold-opens again.
- `cargo test -p theseus --test initialized_project` transactionally seeds an empty top-level Git repository, snapshots it, grows and runs its CLI through public tools, restores it, and cold-opens the seed.
- Launcher/parser tests prove CLI, agent, MCP, HTTP, and gRPC accept one explicit `--project ROOT`; stateful transport tests prove repeated HTTP/gRPC calls share one locked session.

A deterministic session proves policy and mechanics. The linked traced run is the separate evidence that the descriptions, diagnostics, and tool granularity were sufficient for autonomous use.

## Next: automation and agent-surface polish

Live goal runs stay operator-driven (`AGENT_TRACE=1 cargo run -p theseus-agent -- …`); the [runner](#the-runner) scripts the setup and recording, and CI holds the deterministic half. Version-matched skills, harness diagnostic codes, and the eval runner have since landed; the remaining planned improvements—optimistic model-hash CAS, token-efficient inspection, and command contracts—are specified in **`docs/agent-surface-plan.md`**. Living priority order: **`docs/what-next.md`**.

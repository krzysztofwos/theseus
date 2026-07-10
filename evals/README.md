# The goal corpus

Live evals for the agent harness: each goal is run with a real model behind the `Llm` port, its trace kept, and its result recorded here. The mechanical loop invariants (restart interception, gate refusals, resume shapes) live in `cargo test`; these goals branch on model judgment, so they only mean anything live.

Run a goal with `AGENT_TRACE=1 cargo run -p theseus-agent -- --allow-writes "<goal>"` and record: date, turns, outcome, findings.

| # | Goal | Proves | Last run | Outcome |
| - | ---- | ------ | -------- | ------- |
| 1 | Add an operation with a handler; leave the workspace conformant | The model loop | 2026-07-06 (diff) | green — designed and shipped `diff`, ~27 turns |
| 2 | Grow a port method + adapter, restart, call it live | Full self-modification | 2026-07-03 (test), 2026-07-04 (checkpoint) | green — both kept in-tree |
| 3 | Snapshot, break something, roll back | Recovery | not yet run | — |
| 4 | Scaffold an in-tree service, author it, verify | Multi-service | 2026-07-09 (text-utils) | green — exposed scaffold/generate itself first |
| 5 | Explain a subsystem end to end, citing files | Investigation over `read`/`search`/`list` | 2026-07-10 | green — a fully cited end-to-end account of `restart` (model → codegen → generated catalog/dispatch → handler → loop → resumed binary), every claim tied to a file it read; systematic `list`/`read`/`search`/`show` exploration; trace in the session scratchpad |
| 6 | Author a capability from local evidence gathered by `search`+`read`, not only `show` | Reading before writing | 2026-07-10 | green — `lint` built end to end; ~20 searches and several reads before authoring (CLAUDE.md, rust-toolchain.toml, the neighboring adapters), evidence cited in the report; live `lint` returned clean from the rebuilt binary; one authored seam (`StatefulSession`, added the same day) needed a hand delegation, caught by its completeness test |
| 7 | (later) Stand up a foreign adopter from a goal string | Other software | blocked on rooted sessions (§3) | — |

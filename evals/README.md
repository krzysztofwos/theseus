# The goal corpus

Live evals for the agent harness: each goal is run with a real model behind the `Llm` port, its trace kept, and its result recorded here. The mechanical loop invariants (restart interception, gate refusals, resume shapes) live in `cargo test`; these goals branch on model judgment, so they only mean anything live.

Run a goal with `AGENT_TRACE=1 cargo run -p theseus-agent -- --allow-writes "<goal>"` and record: date, turns, outcome, findings.

| # | Goal | Proves | Last run | Outcome |
| - | ---- | ------ | -------- | ------- |
| 1 | Add an operation with a handler; leave the workspace conformant | The model loop | 2026-07-06 (diff) | green — designed and shipped `diff`, ~27 turns |
| 2 | Grow a port method + adapter, restart, call it live | Full self-modification | 2026-07-03 (test), 2026-07-04 (checkpoint) | green — both kept in-tree |
| 3 | Snapshot, break something, roll back | Recovery | not yet run | — |
| 4 | Scaffold an in-tree service, author it, verify | Multi-service | 2026-07-09 (text-utils) | green — exposed scaffold/generate itself first |
| 5 | Explain a subsystem end to end, citing files | Investigation over `read`/`search`/`list` | 2026-07-10 | ran to completion within budget; systematic `list`/`read`/`show` exploration observed; the answer text was lost to an operator filter, and the rerun hit an empty API balance — regrade on the next run |
| 6 | Author a capability from local evidence gathered by `search`+`read`, not only `show` | Reading before writing | — | blocked on API credits |
| 7 | (later) Stand up a foreign adopter from a goal string | Other software | blocked on rooted sessions (§3) | — |

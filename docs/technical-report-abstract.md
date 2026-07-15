# Theseus — short brief (email / abstract)

**Theseus** is a research prototype of a **self-modeling agent harness** in Rust: an LLM develops software—including Theseus itself—through a **machine-readable architecture model**, not free-form shell and file tools alone.

## One paragraph

Theseus holds a model of its crates, services, operations, and ports. From that model it **generates** service contracts, CLI/HTTP/gRPC surfaces, and the agent **tool catalog**. Ten structural **verify** checks keep the workspace honest (dependencies, layering, handler coverage, flow, generated drift). An agent (internal loop or MCP host) uses the same operations humans use: query/patch the model, implement handlers, edit bounded authored Rust items, compile/test/verify, snapshot/roll back, then rebuild so new code becomes live. Mutations that touch disk are **transactional** (repository lease, write-ahead log, compile gate, all-or-nothing commit). The same catalog develops a **foreign project** opened from a durable `theseus.json` and model record — bootstrapped by the loop itself from an empty repository (`--project --init`; the model chooses only the identity), grown through the same operations, and accepted live through `drive`, which rebuilds and invokes the project's own modeled CLI. Live evals close a nine-goal corpus (mid-July 2026), ending with an unassisted run from empty repo to a working, conformant shopping-list service proven through its own command line.

## Why it is different

| Usual coding agent               | Theseus                                  |
| -------------------------------- | ---------------------------------------- |
| Architecture is implicit in chat | Architecture is data + continuous verify |
| Generic read/write/shell         | Tools are projected operations           |
| Partial edits on failure         | Transactional rollback                   |
| New app = open a folder          | Durable project + same catalog           |

## What it is not

Not a multi-tenant production product. Not a general IDE editor. No claim that every LLM run succeeds. Explicit boundaries: local single-operator focus, Unix-oriented mutation engine, and no management of long-running foreign server processes yet — one-shot rebuild-and-invoke is in the catalog; serving is not.

## Evidence (high level)

- Structural `verify` + integration tests for transactions, foreign open, init.
- Live goal corpus, nine for nine: self-extension, recovery drill, investigation/read-before-write, **foreign cold project**, live foreign acceptance (`drive`), and goal-string bootstrap (`initialize`).
- Agent-grown capabilities kept in-tree (e.g. test, checkpoint, diff, lint).

## Try (operators)

```sh
cargo run -p theseus-cli -- verify
cargo run -p theseus-agent -- "what can you do?"   # needs API key for live model
```

Full write-up: `docs/technical-report.md`  
Status / boundaries: `docs/what-next.md` · Implementation plan: `docs/agent-surface-plan.md` · Live goals: `evals/README.md`

---

## Copy-paste email

**Subject:** Theseus — self-modeling agent harness (brief)

Hi —

Quick share on **Theseus**, a side research project: a Rust agent harness where the LLM’s tools are generated from an explicit **architecture model** of the system, and the workspace is held to that model by automated **verify** checks.

In short: the agent patches the model, regenerates contracts, authors handlers through governed splices (not arbitrary file writes), compile-gates every mutating batch under a WAL/transaction, can snapshot/roll back exact project state, and can reopen and develop **other** small Rust projects with the same catalog after an operator init. Multiple front ends (CLI, internal agent loop, MCP, HTTP, gRPC) share one contract.

It is a research prototype with live eval traces (including a foreign project that shipped a small capability unassisted), not a production multi-user product. Happy to walk through a demo or the longer technical report if useful.

Full report: `docs/technical-report.md` in the repo.  
Status and next work: `docs/what-next.md`. Implementation plan for agent-surface polish: `docs/agent-surface-plan.md`.

—

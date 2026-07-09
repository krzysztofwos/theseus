# What to tackle next

Strategic analysis for Theseus as a self-modifying agent harness: a complete set of tools an agent uses to extend itself and develop other software. Written after a project review (architecture, verify status, tool surface, harness log, and second adopter).

## Context

Theseus already closes the **self-extension** loop for model-shaped work:

```text
snapshot → patch → show → implement → check → test → verify → restart → use
         ↘ rollback if needed
```

Coverage is full, `verify` is green, and the agent has grown real capabilities (test, checkpoint, diff, text-utils) through that loop — `restart` itself is now a modeled operation, and an exhausted run resumes with `agent --resume`. Multiple inbounds (CLI, agent, MCP, HTTP, gRPC) share one contract and one session. The second adopter (`adopters/journal/`) proves the engine works outside Theseus.

The open problem is not another transport or more category theory. It is this:

> The agent has excellent tools for **architecture as a model**, and almost no tools for **everything that is not a modeled method** — or for **software that is not Theseus itself**.

That gap is what to close next.

## The bottleneck

| Capability | Self-extend Theseus | Develop other software |
| ---------- | ------------------- | ---------------------- |
| Model ops (`query` / `patch` / `verify`) | Strong | Weak (fixed to Theseus’s model + root) |
| Handler/adapter splice (`show` / `implement`) | Strong | Only inside that model’s crates |
| Build/test gates | Strong | Same, if root is that workspace |
| Checkpoint/restart | Strong | Same |
| Read arbitrary source / search tree | Missing | Missing |
| Write freeform files (tests, `main`, docs, non-method code) | Partial (only generated + splices) | Missing |
| “New product” as a first-class object | In-tree services only | Journal is hand-run, outside the agent |

The harness is great at **replacing planks of its own architecture**. It is not yet a complete programming agent, and it is not yet a product factory. Those are two different expansions; do them in order.

## Recommended order

### 1. First: make the agent able to *see and touch* authored code

**Highest leverage.** Every successful live run in the harness log still depended on patterns the model could only partly reach: neighboring adapters via `show`, house style, compile errors from `implement` / `check`. Real development needs:

- **`read`** — workspace-relative path → contents (and maybe line range)
- **`search`** — content/path search (ripgrep-shaped is fine)
- **`list`** — directory listing

These fit the existing doctrine: **reads stay ambient / unported** (no mutation), exposed as operations with `tool` attributes so they join the catalog. No new port required unless a pure filesystem port is wanted later for symmetry.

Two constraints the first cut must respect. An operation joins **every** transport — the HTTP and gRPC inbounds render handlers for all operations, `tool` attribute or not — so `read` is also a wire endpoint and needs a root guard (canonicalize under the workspace root, refuse escapes) from day one; whether transport scoping should become a modeled fact is a question to note, not to solve here. And freeform writes (`write_source` / `edit`) are **deferred to §4**, pulled by a failed eval: raw file writes are the one capability no check governs, and the typed middle path — extending the splice family, e.g. an `implement --test` inserting a `#[cfg(test)]` item through syn spans — should be tried first when a goal demands it.

Without reads, the agent cannot reliably:

- invent non-trivial adapter bodies from local examples
- add integration tests for a new capability
- fix wedge points outside generated/spliced regions
- work on software whose interesting parts are not `fn` methods on a service trait

**Do this before** big “build other apps” work. Foreign workspaces without read/search just move blindness to a new directory.

**Success criterion:** a live goal authored from local evidence — a non-trivial adapter or handler written by `search` + `read` over neighboring code, not only `show`. (“Add a unit test” belongs to the deferred write slice; tests are not splice-reachable today.)

### 2. Second: measure the harness (a tiny eval suite)

The method was already proven once: cold agent + MCP comparison improved the surface. Make that permanent.

A small **goal corpus** under something like `evals/`:

| Goal | Proves |
| ---- | ------ |
| Add type / operation + implement + verify | Model loop |
| Grow a port method + adapter + restart + call it | Full self-mod |
| Snapshot → break → rollback | Recovery |
| Scaffold in-tree service + implement + verify | Multi-service |
| (later) Init foreign adopter + first green verify | Other software |

The mechanical loop invariants (restart interception, gate refusals, resume shapes) already live in `cargo test` and stay there. The goal corpus is **live-only** — every goal branches on model judgment, which a scripted stub cannot follow — run on a cadence with `AGENT_TRACE` traces kept, tracking turn count and success/fail per goal.

Without evals, every new tool is a story; with evals, you know whether the tool surface got better the way the slug-type comparison did.

**Success criterion:** One command that runs the corpus live and reports regressions against the recorded results.

### 3. Third: “other software” as a first-class adopter workflow

Journal proved the engine. It did **not** prove the *agent* can develop foreign software. The missing product is:

> An agent session rooted at **any** workspace that has a model of record + engine dep, using the same tools.

Concrete slices:

1. **Root parameter** — Session / CLI / agent take `--root <path>` (or env); `workspace_root`, checkpoint, toolchain, generate paths all relative to it. This is bigger than a flag: ambient reads assume *the* root (compile-time `workspace_root()` is used by handlers directly), so the root threads through handlers, not just adapters.
2. **Model of record as input** — not only `theseus_model()`; load / hold the working model for that adopter. The hidden step: path conventions (`generated_files`, `authored_impls`, adapter files) are adopter **code** today (`theseus-model`, `journal-model`) — a foreign-rooted session needs them either modeled as vocabulary or injected as a trait. Naming this is what makes §3 an arc, not a flag.
3. **Init / project as tools** — what journal’s `project` binary does (scaffold missing crates + write generated files), reachable from the agent.
4. **One live goal:** “From empty dir (or template), produce a journal-class service; `verify` green; CLI works.”

That is the distinctive Theseus path to “develop other software”: **other software is another model**, not “become a general coding agent that ignores the model.”

In-tree demos (calculator, text-utils) stay valuable, but they never leave the Theseus fixed point. Foreign adopters do.

**Success criterion:** Agent-driven third adopter (or re-drive journal from cold) with no hand-edited generated files.

### 4. Fourth: close remaining autonomy holes (only as evals demand)

These matter, but they should be **pulled by failed goals**, not built as a pile:

| Gap | When it matters |
| --- | --------------- |
| **Bootstrap as a tool / recovery path** | Agent wedges renderer + consumer; today needs a human |
| **`patch --write` + compile gate** | Bad reprojections still possible before `check` |
| **Untracked files vs snapshot** | Rollback leaves garbage; agent creates files then rolls back |
| **Owned composition-root / freeform wiring** | Mostly fixed by generated `Standalone`; remaining holes show up when growing inbounds |
| **Freeform write (`write_source` / typed test-splice first)** | An eval fails for want of a file `implement` cannot own |
| **Structured check/test/implement outcomes** | `verify` is already structured JSON; the prose outcomes are `check`, `test`, and `implement` |
| **Richer `implement` context** | Auto-include neighboring method snippets in `show` |

Avoid spending a quarter on HTTP auth, multi-tenant sessions, or the generic affordance projector. Those serve deployment, not “complete tools for self-extension and development.”

## What not to prioritize next

- **More transports** — CLI, agent, MCP, HTTP, and gRPC already exist. Enough probes.
- **Deeper kernel / more functors** — Checks already do real work; diminishing returns for the harness goal.
- **Bigger self-model surface for demos** (`calc`-style) — Fun, but does not unlock new agent autonomy.
- **Becoming Claude Code** — A full IDE agent without a model story abandons the project’s thesis. General read/edit is a **leaf** capability; the product remains model → generate → verify.

## North star for the next phase

Phrase the next milestone so it can fail loudly:

> **An agent, with `--allow-writes`, can grow a capability in Theseus *and* stand up a small foreign service from a goal string, using only the tool catalog — with a regression eval that stays green.**

Break that into three shippable increments:

1. **See/touch** — read/search/list (+ gated freeform write if needed)
2. **Know** — eval corpus for self-mod goals
3. **Elseware** — rooted sessions + adopter init/project driven by the agent

## Suggested first ticket (smallest cut of #1)

Add three read-only operations on Theseus (with solid `tool` descriptions and examples, learned the hard way from the slug run):

1. `read` — `{ path, start_line?, end_line? } → String`
2. `search` — `{ pattern, path? } → String` (substring match, capped output; a std-only walk — no new binary dependency, `rg` can come later if an eval wants regex)
3. `list` — `{ path } → String`

Wire them as pure handlers (direct `tokio::fs`, a std-only walk), root-guarded, no new ports, no `uses`. Extend system framing: “prefer `show` for modeled handlers; use `read`/`search` for everything else.” Then one eval: author a non-trivial adapter by reading an existing one via `search`+`read`, not only `show`.

That single slice makes every later goal cheaper — self-mod and foreign software alike — without diluting the architecture story.

## Bottom line

The self-modifying core is already credible. Next work should convert that into a **complete development surface** (read/edit/eval first), then **generalize the session off the Theseus monorepo** so “other software” is the same loop on another model — not a separate human ritual in `adopters/`.

## Related docs

- `docs/building-the-harness.md` — experiment log of growing the agent surface
- `docs/second-adopter.md` — engine reusability proven from outside
- `docs/affordance-projector.md` — contract vs surface affordances (resolved by simplification)
- `README.md` / `CLAUDE.md` — product thesis and working map

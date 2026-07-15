# Implementation plan: agent-surface polish

**Status:** planned  
**Date:** 2026-07-16  
**Inputs:** live evals 1–9 (green), foreign project workflow, review of peer project Zerolang (graph-first agent language) for _patterns only_  
**Non-goals:** adopt Zerolang as a language, build a program-graph IR for Rust, or broaden free-form file writes

This plan improves how agents **learn, fail, re-plan, and prove** work against Theseus. The structural core (model, verify, transactions, project rooting, governed edits) is already in place. The gap is **agent-facing productization** of that core.

Related status: `docs/what-next.md`. Peer analysis summary lives in the “Peer lessons” section of that doc.

---

## Principles

1. **Semantic edits stay primary.** Prefer model patch, `implement`, `edit_rust_item`, and `drive` over unrestricted text writes.
2. **Version-matched guidance.** Anything an agent loads to learn the harness must match the **running binary** (or its model hash), not a stale external skill file alone.
3. **Failures are repair contracts.** Stable codes, short next actions, optional plans with safety labels. Text for humans; JSON when tools need fields.
4. **Trust gates that already ran.** After a compile-gated write reports success, do not spend turns re-`check`ing only to confirm.
5. **Evals drive expansion.** Widen typed edits only when a recorded goal fails for want of them.
6. **Isolate validation.** Agent/CI checks against dirty trees must not race the developer’s working tree.

---

## Phases

### Phase 0 — Doc and baseline freeze (done with this plan)

**Deliverables**

- `docs/what-next.md` reoriented around completed foreign path + agent-surface track.
- This plan committed and linked from README / technical report.

**Acceptance**

- [x] Plan and status docs describe green evals 1–9 and the next ordered work.
- [ ] No claim that Zerolang language machinery is in scope.

---

### Phase 1 — Version-matched skills surface

**Problem.** Tool descriptions and `CLAUDE.md` teach the harness, but they can drift from the built binary. Zerolang’s `zero skills get <topic>` ties guidance to the compiler that will run.

**Design**

- Add a small skill catalog served by the running tool:
  - CLI: `theseus skills`, `theseus skills get <name> [--topic <section>]`
  - Optional agent tool `skills` (read-only, ambient) so MCP/agent loops can fetch the same text.
- Topics (initial set, keep each small):

  | Topic         | Approx size | Content                                                                      |
  | ------------- | ----------- | ---------------------------------------------------------------------------- |
  | `workflow`    | short       | Snapshot → patch → implement/edit → gate trust → test/verify → drive/restart |
  | `model`       | short       | Handles, patch verbs, `uses` / `tool` attrs, query filters                   |
  | `source`      | short       | `read` revision, `edit_rust_item` kinds, ownership, when to use `show`       |
  | `diagnostics` | short       | How to read refuse codes, check reports, gate rollbacks                      |
  | `project`     | short       | `--project`, init/open, layout ownership, drive vs restart                   |

- Content lives as static markdown (or strings) **embedded in the crate that ships with the binary**, keyed by topic name. Optionally stamp model hash / crate version in the skill header.
- Agent system framing: “fetch `workflow` once per session; fetch other topics only when needed; do not refetch unchanged topics.”

**Implementation sketch**

1. `rust/theseus/src/skills/` or `skill-data/*.md` included via `include_str!`.
2. Operation or CLI-only command; prefer a **modeled operation** if MCP/agent must share it (`tool` attribute set).
3. Unit tests: every topic non-empty; `skills get unknown` fails with a list of names.
4. Agent framing string updated to mention `skills`.

**Acceptance**

- [ ] `theseus skills` lists topics; `theseus skills get workflow` prints version/hash header + body.
- [ ] Agent or MCP can obtain the same body without reading repo docs on disk.
- [ ] Skill text does not invent ops that are not in the model (spot-check against `query --kind operation`).

**Risks**

- Skill text becomes a second source of truth. Mitigate: generate the **operation list** section from the model at render time; keep narrative hand-authored and short.

---

### Phase 2 — Diagnostics as repair contracts

**Problem.** Model `patch` already returns coded `PATCH*` diagnostics with repair shapes. Compile gates, path errors, stale revisions, flow failures, and verify gaps are mostly free prose.

**Design**

Introduce a shared **harness diagnostic** shape (extend existing patterns where possible):

```text
code: THS001 | PATCH020 | …
message: short summary
help: one next action
repair: optional { id, summary }
safety: format-only | behavior-preserving | architecture-changing | requires-human-review
```

Surfaces:

| Failure class              | Example codes | Repair hint                                   |
| -------------------------- | ------------- | --------------------------------------------- |
| Stale source revision      | `SRC001`      | Re-`read`, pass new `revision`                |
| Path escape / not owned    | `SRC002`      | Use layout-owned path; `list` root            |
| Write gate refused         | `GATE001`     | Rerun with `--allow-writes`                   |
| Compile gate rollback      | `GATE002`     | Read `detail`; fix body; retry implement/edit |
| Verify flow/coverage/drift | `VFY00x`      | Use check name + named gap from report        |
| Unknown snapshot           | `CKP001`      | Snapshot in this session first                |

Add:

- `explain` operation or CLI: `theseus explain THS001` → stable text/JSON.
- Prefer **text** in interactive agent results; include `code` + `help` in structured JSON results where outcomes already serialize.

Optional later: `fix_plan` that returns candidate steps only (no auto-apply), with `safety` labels—Zerolang’s `fix --plan` pattern.

**Implementation sketch**

1. Define `HarnessDiagnostic` in `theseus` or `theseus-modeling` (if shared with patch).
2. Map existing `PATCH*` into the same envelope (alias, not rewrite all codes).
3. Thread codes through `implement` / `edit_rust_item` failure paths and common browse errors.
4. `explain` reads a static table of code → rule + help.
5. Tests: each public code has an explain entry; one end-to-end stale-revision error carries `SRC001`.

**Acceptance**

- [ ] Stale `edit_rust_item` failure includes a stable code and actionable `help`.
- [ ] `explain SRC001` works without network.
- [ ] Agent-facing tool results for gated rollbacks expose `ok: false` **and** a code (not detail-only).

**Risks**

- Over-encoding every anyhow string. Start with the top failure modes from eval traces, not exhaustive coverage.

---

### Phase 3 — Expect-hash / revision CAS on multi-step mutations

**Problem.** Long MCP/HTTP sessions and multi-turn agents can submit patches against a stale mental model. Zerolang’s `expect graphHash` makes staleness loud. Theseus already has file **revisions** for `edit_rust_item` and optimistic **expected projection** on workspace commits; model-level CAS for agents is incomplete.

**Design**

- Optional fields on mutating requests:
  - `expect_model_hash` on `patch` (when `write` true or always for dry-run consistency checks).
  - Keep mandatory `revision` on `edit_rust_item` (already).
- Session/MCP: when the agent omits expect hash, **server may stamp** the working model hash it is about to mutate (auto-CAS within one session). Document that concurrent writers need explicit expect.
- On mismatch: refuse with diagnostic code + “re-query / re-read” repair; do not apply partial edits.

**Implementation sketch**

1. Extend `PatchRequest` (model change → generate → handlers).
2. Compare to `model_hash(&working)` before `apply_edits` / persist.
3. Tool schema + skill text describe the field; examples in `skills get model`.
4. Tests: mismatch refuses; match applies; session auto-stamp unit test.

**Acceptance**

- [ ] Explicit wrong `expect_model_hash` never writes.
- [ ] Session path either requires or auto-stamps hash; behavior documented in skills.
- [ ] No regression in goal-shaped offline tests.

**Risks**

- Request-type growth reintroduces affordance conflation (see `docs/affordance-projector.md`). Prefer a single optional field on the core contract used by all transports, not CLI-only flags.

---

### Phase 4 — Token-efficient inspection

**Problem.** Agents burn turns and context on full-file `read`s. Zerolang offers `view --fn`, `--around`, `--outline`, short handles.

**Design**

Extend existing tools rather than adding many new ones:

| Enhancement | Behavior                                                                                          |
| ----------- | ------------------------------------------------------------------------------------------------- |
| `show`      | Already method-scoped; ensure tool text prefers it over `read` for handlers                       |
| `read`      | Optional `max_bytes` / keep cap; optional `outline` mode for `.rs` (item signatures only via syn) |
| `search`    | Already capped; add clearer “use before read” guidance                                            |
| `query`     | Already filters; add skill examples for narrow queries                                            |

Optional: `outline` as a dedicated op if `read` mode flags get messy.

**Acceptance**

- [ ] `read` outline mode (or equivalent) returns only top-level item signatures for a layout-owned `.rs` file.
- [ ] Skill `source` teaches outline → search → read-slice → edit.
- [ ] At least one unit test for outline on a fixture file.

---

### Phase 5 — Agent framing: trust the gate

**Problem.** After `implement` / `edit_rust_item` / `patch --write` already compile-gated, agents still call `check` “to be sure,” wasting budget (visible in long foreign runs).

**Design**

Update system framing and `skills get workflow`:

1. Snapshot before first write.
2. Mutate through gated tools; **read the structured check in the tool result**.
3. `test` when behavior changed; `verify` when architecture/model changed.
4. `check` only if no fresh gated result exists (e.g. after manual tree edits, or before `restart`/`drive` if unsure).
5. `drive` to prove a project CLI op live; `restart` for harness self-mod.

**Acceptance**

- [ ] Framing and skills agree; no instruction to always `check` after every implement.
- [ ] Optional eval metric: median redundant `check` calls after successful gated writes drops on a re-run of goal 7 or 9 (record only; do not fail CI on LLM variance).

---

### Phase 6 — Eval automation

**Problem.** `evals/README.md` is the corpus of record but runs are manual. Zerolang’s fixture + sandboxed live runner is the pattern.

**Design**

```text
cargo run -p theseus-evals -- --list
cargo run -p theseus-evals -- --goal 7 --fixture    # scripted LLM or no LLM where possible
cargo run -p theseus-evals -- --goal 7 --live       # real model, isolated worktree
```

Each live run records:

- goal id, date, model id, harness git commit / model hash
- turn count, tool call counts
- final deterministic checks (verify, named tests, CLI stdout)
- path to `AGENT_TRACE` log

**Isolation:** each live run uses a fresh worktree or temp project root; never the developer’s dirty main tree.

**Fixture mode:** offline scripted tool sequences for goals that are pure policy (where already unit-tested, skip); for LLM goals, fixture may only re-check acceptance scripts against a checked-in tree snapshot.

**Acceptance**

- [ ] One entrypoint lists goals and runs a selected goal’s **deterministic acceptance** without an API key.
- [ ] Live mode documents required env (`ANTHROPIC_API_KEY`) and writes a result row template under `evals/runs/`.
- [ ] CI runs deterministic acceptance only; live remains operator-driven.

---

### Phase 7 — Command contracts

**Problem.** Tool schemas and CLI envelopes can drift without a dedicated guard.

**Design**

- Snapshot or assert:
  - `tool_catalog()` names ⊆ modeled ops with `tool` set
  - each catalog entry has non-empty description and object schema
  - `CheckReport` / patch outcome JSON shapes for golden fixtures
- Optional: clap help freeze for critical subcommands (`verify`, `skills`, `init`).

**Acceptance**

- [ ] `cargo test -p theseus` (or modeling) fails if a `tool`-tagged op is missing from the catalog renderer.
- [ ] Golden test for one patch refusal diagnostic envelope.

---

### Phase 8 — Typed expansion only if evals demand (backlog)

Do **not** schedule by default. Pull when a goal fails:

| Expansion                                      | Trigger example                                                   |
| ---------------------------------------------- | ----------------------------------------------------------------- |
| `impl` method edit                             | Cannot add test helper methods without top-level item workarounds |
| New authored file creation                     | Cannot add `tests/foo.rs` without leaving ownership model         |
| Manifest / dependency edit                     | Need new crate dep to ship a capability                           |
| Process manager for long-lived foreign servers | Goal needs HTTP server stay-up, not `drive` one-shot              |

Each expansion keeps: ownership checks, revision/CAS, WAL, compile gate, skills/diagnostics updates.

---

### Phase 9 — Process manager & fd-relative FS (later)

Retained from prior roadmap; not agent-surface polish but still real:

- Rebuild/replace contract for HTTP/gRPC/MCP and foreign servers.
- `openat` / `openat2` publication path for stronger same-account safety.

Track under `docs/what-next.md`; do not block Phases 1–7.

---

## Suggested sequencing and sizing

| Phase                | Depends on          | Rough effort | Priority |
| -------------------- | ------------------- | ------------ | -------- |
| 1 Skills             | —                   | S–M          | P0       |
| 2 Diagnostics        | — (parallel with 1) | M            | P0       |
| 5 Framing trust-gate | 1                   | S            | P0       |
| 4 Outline inspection | —                   | S–M          | P1       |
| 3 Expect-hash        | model/request gen   | M            | P1       |
| 6 Eval automation    | 5 helpful           | M            | P1       |
| 7 Command contracts  | 1–2                 | S            | P1       |
| 8 Typed expansion    | failed eval         | variable     | P2       |
| 9 Process/FS         | —                   | L            | P2       |

**Recommended first merge train:** Phase 1 + Phase 5 (skills + framing), then Phase 2 (diagnostics), then Phase 6 (eval runner skeleton).

---

## Definition of done (program-level)

The agent-surface polish track is **done enough** when:

1. A cold agent can load **workflow + model** skills from the running binary and complete goal 7 or 9 without reading `CLAUDE.md` from the repo.
2. The top failure modes from foreign runs return **stable codes** with `explain` support.
3. One command runs **deterministic** eval acceptance; live runs have a standard recording layout.
4. Catalog/schema **contracts** fail CI on silent drift.
5. `docs/what-next.md` lists only remaining P2 items and explicit boundaries.

---

## Explicit non-adoptions (from Zerolang review)

| Peer idea                                             | Decision                                       |
| ----------------------------------------------------- | ---------------------------------------------- |
| Program graph as source of truth for application code | Reject for Theseus; model + Rust leaves remain |
| Binary `.graph` store / import-export of language IR  | Reject                                         |
| Rewrite-by-example expression IR                      | Defer indefinitely                             |
| Auto-apply fix plans                                  | Reject; plans only, human/agent chooses        |
| Production multi-tenant sandbox network               | Out of scope                                   |

---

## References

- Peer patterns: Zerolang README, `skill-data/agent.md`, `skill-data/diagnostics.md`, `skill-data/graph.md`, `evals/README.md` (upstream experiment under `platform-starter-kit/scratch/zerolang`)
- Theseus status: `docs/what-next.md`
- Theseus evidence: `evals/README.md`
- Affordance caution: `docs/affordance-projector.md`
- Product overview: `docs/technical-report.md`

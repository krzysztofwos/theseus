# The affordance projector

Status: a diagnosis and a proposed design, with a runnable proof of concept in `rust/modeling/examples/generic_projector.rs`. Not built into the engine. This note captures the thinking so the decision can be made deliberately.

## The symptom

Theseus grew a second way to drive its own operations. The `Cli` inbound calls the generated `TheseusService` contract. The `Agent` and `Mcp` inbounds drive a hand-written `Session::call` over a hand-written `tool_catalog()`. Two surfaces, and the second one re-implements what the first one calls, so the two have already drifted (`implement`'s message and its hash/write semantics differ between them). A code review flagged the duplication. The natural fixes seemed to be "generate the agent surface too" or "hand-write the CLI too for symmetry." Both felt wrong — one adds machinery, the other sheds the tool's most legible self-modeling demonstration.

The clue is that the agent surface is not a faithful projection of the operations. It exposes a curated subset, and it reshapes inputs: `patch` takes `{edit, write}`, not `PatchRequest`'s nine fields. `implement` drops `expect_model_hash` and `body_file`. The catalog hand-writes those reshaped shapes. That reshaping is where the duplication lives, and it is the thing to explain.

## The diagnosis

The request type is doing double duty. `PatchRequest` and `ImplementRequest` are not the operation's input — they are the CLI's surface shape, and the CLI's affordances have been welded into the contract that every surface shares. The agent surface looks curated only because it is the same contract with the CLI's affordances stripped back out by hand.

Every field the agent drops is a surface affordance, not a domain input:

| operation                       | contract (core input) | difference, classified                                                                                          |
| ------------------------------- | --------------------- | --------------------------------------------------------------------------------------------------------------- |
| `model` / `verify` / `coverage` | (empty)               | identical on both surfaces                                                                                      |
| `query`                         | `find, node, kind`    | identical on both — faithful either way                                                                         |
| `show`                          | `method`              | identical on both                                                                                               |
| `patch`                         | `edits, write`        | `verb/target/kind/name/to/set` = alternate-form affordance. `expect_model_hash` = concurrency-supply affordance |
| `implement`                     | `method, body`        | `expect_model_hash` = concurrency-supply. `body_file` = input-source affordance                                 |

The tell that clinches it: the operations whose request type is pure core — `query`, `show`, `verify`, `coverage` — need zero reshaping and project identically to every surface. Only the operations with affordances welded on need pruning. The agent is not curating. It is seeing the contract without the CLI's clothes on.

Three field-level affordances account for every difference, and they form a small closed set:

- concurrency-supply — who provides the expected model hash. The CLI takes it as a flag. The `Session` stamps the live working-model hash. A gRPC client passes it in request metadata.
- input-source — where a text field comes from. The CLI offers file/stdin/inline. The agent passes it inline.
- alternate-form — a convenience entry over a canonical one. The CLI offers `patch`'s single-edit form. The batch `edit` is the core.

Two more per-surface axes sit alongside them: op-set (which operations a surface carries — `calc` is absent from the agent because that inbound's `Ctx` does not wire the calculator port, not because it is a different operation) and presentation (per-surface help and descriptions).

## Why the precursors never named it

The `categorical-architecture` lineage this project descends from generates whole systems from a model and binds authored leaves into the generated surfaces. It handles a surface that does not match the generated shape by authoring it — per-route `authored` versus `scaffold-once` channels, a readiness ledger, a gap inventory. That is a workaround, not a diagnosis: when a surface wants a different shape, defect to authoring.

They could get away with it because their tooling is not modeled. The `ca` command that runs generate/verify/patch on the model is hand-written (`categorical-architecture-astronomy-shop/src/bin/ca.rs` hand-parses `--expect-model-hash`), and the model describes the application being built, not the tool that builds it. So they never model an operation whose surface carries tooling affordances — the hash lives in the authored CLI and never touches a modeled contract.

Theseus is different in the way that forces the issue: it is self-modeling. Its tooling operations are the modeled operations, and it generates its own tooling CLI from the self-model. The moment the tool folds into its own model, the operation contract and the tooling-surface affordances land in the same struct. The precursors kept the affordance authored-and-separate because their tooling was never modeled. Theseus cannot sidestep — it is its own model — so it has to actually separate contract from affordance. That separation is a contribution past the lineage, not a port of it.

## The design: a generic projector

Separate the two things the request type conflates, and the whole tension dissolves.

- An operation declares its pure contract — semantic input and output only — plus properties naming which affordances apply (`hash_checked`, a `sourceable` field, an `alt_form`).
- Each inbound declares an affordance policy — how it resolves each applicable affordance (hash by caller / auto / metadata, source file+inline / inline, forms all / canonical).
- The codegen becomes one generic projector: a `TransportBackend` per transport that renders `(contract, policy)`. The CLI is `core + {caller-hash, file-source, single-edit, clap}`. The agent is `core + {auto-hash, inline, json-schema}`. MCP is the agent set over stdio. gRPC is `core + {metadata-hash}` over proto/tonic.

No surface is privileged, because none is the canonical shape anymore — the contract is, and surfaces are projections with affordance layers. `Transport` stops being a branch inside one renderer and becomes which backend to dispatch to.

## In practice

The proof of concept renders one clean `patch` contract to three transports. It shares nothing per-surface but the policy:

```text
one contract:  patch(edit: [Edit], write: bool) -> PatchOutcome
  properties:  hash_checked, alt_form(single-edit)

── Cli ──────────────────────────────────
theseus patch --edit <[Edit]> --write <bool>
    # or the single-edit form: --verb --target --kind --name --to --set
    --expect-model-hash <HASH>

── Agent ─────────────────────────────────
{ "name": "patch",
  "input_schema": { "type": "object", "properties": { "edit": { "type": "array" }, "write": { "type": "boolean" } } } }

── Grpc ──────────────────────────────────
rpc Patch(PatchRequest) returns (PatchOutcome);
message PatchRequest {
  repeated Edit edit = 1;
  bool write = 2;
}
// the handler reads `x-expect-model-hash` from request metadata
```

The CLI's `--expect-model-hash` and single-edit flags are now clearly its affordance policy, not part of the shared contract. Adding gRPC was one more backend plus a one-line policy — an additive change, not surgery on a CLI-special renderer.

## Is the codegen modeled? Should it be?

No, and it should not be. The self-model carries a crate node for `theseus-modeling` and the `generate` operation, but the render logic is authored Rust, not model data. The codegen is the authored floor of the fixed point. `self_model.rs` describes the tool that holds it, but that recursion has to bottom out in code that is not generated — the kernel and the codegen are that floor. Modeling the renderer is a category error and an infinite regress: something would have to render it from the model, and now you need a meta-codegen. The renderer renders the model. It is not rendered from the model.

The line the projector draws is: model the what, author the how. Modeled data is the operations and their clean contracts, the inbounds, and each inbound's transport and affordance policy — what Theseus declares about its own surfaces. The authored, fixed engine is the generic projector and one backend per transport — how each wire format is emitted. Adding a transport is a model declaration (an inbound with a policy) plus one authored backend. After the backend exists, more inbounds on that transport are model-only.

## What it buys

- Every surface generated — the lineage-faithful direction — symmetric, drift-gated, no duplication, no hand-written surface, no authored surface seam.
- The contracts cleaned of transport leakage: `PatchRequest` becomes `{edits, write}`.
- An engine that is smaller and more general than either today's design or the drop-generated-CLI prototype, because it stops special-casing `clap` and starts treating every surface as `contract + affordances`.
- New transports — gRPC, then HTTP — drop in as a backend plus a declaration.

## The risk that decides it

The affordance vocabulary must stay small and closed for this to be a simplification rather than a second framework. Today's evidence is three field-affordances (concurrency-supply, input-source, alternate-form) plus op-set and presentation — genuinely small, and every difference across all operations classified into it with nothing left over. If it stays that small in practice, this is the synthesis. If it sprawls, it is curation with extra steps, and the honest move would be to stop and reconsider.

# Building a Calculator Service

This document builds a calculator as a separate service living in its own crate and exposes it through the Theseus CLI as a single `calc` operation. It demonstrates multi-service modeling: an in-process service, a crate of its own, an outbound port bound to that service, and code generation, authoring, and verification across the crate boundary. The mechanics of a single operation are described in [adding an operation](adding-an-operation.md). This document builds on those concepts.

The result is a `theseus calc` subcommand that parses on the command line and delegates, through an in-process port, into a `theseus-calculator` crate that owns the arithmetic. The calculator comprises a shared `Operands` request (two numbers), a `CalcResult` response, and the operations `add`, `subtract`, `multiply`, and `divide`.

## Setup

```sh
alias theseus='cargo run -q -p theseus-cli --'
```

All commands assume this alias and the repository root as the working directory.

## 1. Scaffold the crate

A new crate is created under `rust/`. The workspace `members = ["rust/*"]` glob picks it up, so no workspace-manifest edit is required.

`rust/calculator/Cargo.toml`:

```toml
[package]
name = "theseus-calculator"
version = "0.1.0"
edition = "2024"
description = "A calculator service Theseus exposes through its CLI"

[dependencies]
anyhow = { workspace = true }
```

`rust/calculator/src/lib.rs` wires the modules and re-exports the contract and the adapter:

```rust
mod generated;
mod service;

pub use generated::{CalculatorService, Operands};
pub use service::Calc;
```

`rust/calculator/src/service.rs` holds the authored adapter, an empty implementation of the contract whose methods fall through to their `unimplemented` defaults until authored:

```rust
use crate::generated::CalculatorService;

/// The calculator adapter.
pub struct Calc;

impl CalculatorService for Calc {}
```

The `generated` module does not exist yet; code generation renders it in step 3.

## 2. Register the crate in the model

The crate node and the dependency edge from `theseus-cli` are a direct edit to the model source. Crate registration is the one structural change made to `rust/model/src/self_model.rs` by hand; it is a candidate for a future patch operation.

A node is added for the new crate, at layer 1 with no dependencies, and `theseus-cli` gains a dependency on it:

```rust
.crate_node("theseus-calculator", "calculator", 1, &[])
.crate_node(
    "theseus-cli",
    "cli",
    3,
    &["theseus-model", "theseus-modeling", "theseus-calculator"],
)
```

The dependency edge is what the required-dependency and layering checks verify against the real manifests once the calculator is in place.

## 3. Declare the service and the calc surface

The remaining model change — the types, the Calculator service and its operations, and the Theseus service's `calc` operation and outbound port — is declared in a single hash-checked `patch`. The hash is read after the crate-node edit, since that edit changed the model:

```sh
H=$(theseus query | grep -o '"model_hash": "[^"]*"' | head -1 | cut -d'"' -f4)

theseus patch --write --expect-model-hash "$H" \
  --edit 'add|model:theseus|kind=type|name=Operands|shape=struct:a=f64:Left operand.,b=f64:Right operand.' \
  --edit 'add|model:theseus|kind=type|name=CalcResult|shape=foreign:String' \
  --edit 'add|model:theseus|kind=type|name=CalcRequest|shape=struct:op=String:The operator: add, subtract, multiply, or divide.,a=f64:Left operand.,b=f64:Right operand.' \
  --edit 'add|model:theseus|kind=service|name=Calculator|inbound=InProcess|crate=theseus-calculator' \
  --edit 'add|service:theseus:Calculator|kind=operation|name=add|summary=Add the operands.|request=Operands|response=CalcResult' \
  --edit 'add|service:theseus:Calculator|kind=operation|name=subtract|summary=Subtract the operands.|request=Operands|response=CalcResult' \
  --edit 'add|service:theseus:Calculator|kind=operation|name=multiply|summary=Multiply the operands.|request=Operands|response=CalcResult' \
  --edit 'add|service:theseus:Calculator|kind=operation|name=divide|summary=Divide the operands.|request=Operands|response=CalcResult' \
  --edit 'add|service:theseus:Theseus|kind=operation|name=calc|summary=Evaluate an arithmetic expression through the calculator service.|request=CalcRequest|response=CalcResult' \
  --edit 'add|service:theseus:Theseus|kind=port|name=calculator|summary=Evaluates arithmetic through the calculator service.|target=Calculator'
```

Three aspects are notable. The `Calculator` service is added with an `InProcess` inbound transport, so code generation renders its contract trait but no command surface. Its operations are addressed to it by its handle, `service:theseus:Calculator`, rather than to the model root. The `calculator` port is bound to the `Calculator` service with `target=Calculator`, so its contract is that service's operations rather than a set of methods of its own.

With `--write`, the proposed model is reprojected. Because the model now places one service in `theseus-calculator` and one in `theseus-cli`, code generation renders a scaffolding file per crate: `rust/calculator/src/generated.rs` and `rust/cli/src/generated.rs`.

## 4. Inspect the generated contract

The calculator crate's generated file holds the contract trait and the plain request struct, with no command surface and no command-line dependency:

```sh
sed -n '1,12p' rust/calculator/src/generated.rs
#   pub struct Operands { pub a: f64, pub b: f64 }
#   pub trait CalculatorService {
#       fn add(&self, _request: Operands) -> anyhow::Result<String> { ... }
#       ...
#   }
```

The cli crate's generated file gains the `calc` subcommand, the typed `CalcRequest` parser, and a composition-root field bound to the calculator's contract:

```sh
grep "calculator" rust/cli/src/generated.rs
#   pub calculator: &'a dyn theseus_calculator::CalculatorService,
```

## 5. Wire the cli leaves

Three authored edits connect the cli to the calculator. The path dependency is added to `rust/cli/Cargo.toml`:

```toml
theseus-calculator = { path = "../calculator" }
```

The composition root in `rust/cli/src/main.rs` constructs the adapter and passes it into the context:

```rust
let calculator = theseus_calculator::Calc;
let ctx = Ctx {
    model: &model,
    workspace: &workspace,
    calculator: &calculator,
};
```

The `calc` handler in `rust/cli/src/service.rs` maps the parsed request onto the port and dispatches on the operator:

```rust
fn calc(&self, request: CalcRequest) -> anyhow::Result<String> {
    let operands = theseus_calculator::Operands { a: request.a, b: request.b };
    match request.op.as_str() {
        "add" => self.calculator.add(operands),
        "subtract" => self.calculator.subtract(operands),
        "multiply" => self.calculator.multiply(operands),
        "divide" => self.calculator.divide(operands),
        other => anyhow::bail!("unknown operator `{other}`, expected add, subtract, multiply, or divide"),
    }
}
```

## 6. Author the calculator handlers

The calculator's four operations begin on their `unimplemented` defaults. Coverage reports them across both crates, and each is authored with `implement`, which resolves the operation to its service and writes the handler into that service's crate:

```sh
theseus coverage      #   9 implemented of 13; gaps: add, subtract, multiply, divide
H=$(theseus query | grep -o '"model_hash": "[^"]*"' | head -1 | cut -d'"' -f4)
theseus implement --method add      --body 'Ok(format!("{}", request.a + request.b))' --expect-model-hash "$H"
theseus implement --method subtract --body 'Ok(format!("{}", request.a - request.b))' --expect-model-hash "$H"
theseus implement --method multiply --body 'Ok(format!("{}", request.a * request.b))' --expect-model-hash "$H"
theseus implement --method divide   --body 'Ok(format!("{}", request.a / request.b))' --expect-model-hash "$H"
#   wrote the handler for `add` into rust/calculator/src/service.rs. Rebuild to load it
```

The handlers land in `rust/calculator/src/service.rs`, the authored file of the crate the `Calculator` service lives in.

## 7. Run and verify

```sh
cargo build -p theseus-cli
theseus calc --op add      --a 2 --b 3   #   5
theseus calc --op subtract --a 10 --b 4  #   6
theseus calc --op multiply --a 6 --b 7   #   42
theseus calc --op divide   --a 1 --b 8   #   0.125
```

```sh
theseus coverage          #   13 / 13 implemented
theseus verify            #   conformant: workspace matches its self-model
```

Verification is conformant across all six checks. The `theseus-cli` to `theseus-calculator` dependency is a real crate edge, so the required-dependency and layering functors confirm the inter-service link with no check specific to it: the same machinery that holds Theseus's own layering holds the calculator's.

## Summary

A calculator was built as a separate service in its own crate and exposed through the CLI as a single `calc` operation. The crate scaffolding and the crate-node registration were authored directly; the service, its operations, the response type, and the bound port were declared through one `patch`; code generation rendered a contract into each crate. The four arithmetic handlers were authored through `implement` into the calculator crate. Verification confirmed the cli-to-calculator dependency through the existing crate-graph checks. The authored code was the crate skeleton, three cli leaves, and four arithmetic handler bodies.

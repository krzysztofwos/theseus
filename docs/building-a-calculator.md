# Building a Calculator Service

This document builds a calculator as a separate service living in its own crate and exposes it through the Theseus CLI as a single `calc` operation. It demonstrates multi-service modeling: an in-process service, a crate of its own scaffolded from the model, an outbound port bound to that service, and code generation, authoring, and verification across the crate boundary. The mechanics of a single operation are described in [adding an operation](adding-an-operation.md). This document builds on those concepts.

The result is a `theseus calc` subcommand that parses on the command line and delegates, through an in-process port, into a `theseus-calculator` crate that owns the arithmetic. The calculator comprises a shared `Operands` request (two numbers), a `CalcResult` response, and the operations `add`, `subtract`, `multiply`, and `divide`.

The build proceeds in two phases. The first models and scaffolds the calculator crate as a self-contained service. The second exposes it through the CLI. The order matters: the cli does not reference the calculator until the calculator crate exists and is depended upon, so each `theseus` command in between still builds.

## Setup

```sh
alias theseus='cargo run -q -p theseus-cli --'
```

All commands assume this alias and the repository root as the working directory.

## 1. Model the calculator service

The crate, the shared types, and the Calculator service with its four operations are declared in a single `patch`. The crate is registered through the protocol with `kind=crate`, carrying its directory and layer. The service is placed in it with `crate=theseus-calculator` and an `InProcess` inbound transport, so it contributes a contract trait but no command surface:

```sh
theseus patch --write \
  --edit 'add|model:theseus|kind=crate|name=theseus-calculator|dir=calculator|layer=1' \
  --edit 'add|model:theseus|kind=type|name=Operands|shape=struct:a=f64:Left operand.,b=f64:Right operand.' \
  --edit 'add|model:theseus|kind=type|name=CalcResult|shape=foreign:String' \
  --edit 'add|model:theseus|kind=service|name=Calculator|inbound=InProcess|crate=theseus-calculator' \
  --edit 'add|service:theseus:Calculator|kind=operation|name=add|summary=Add the operands.|request=Operands|response=CalcResult' \
  --edit 'add|service:theseus:Calculator|kind=operation|name=subtract|summary=Subtract the operands.|request=Operands|response=CalcResult' \
  --edit 'add|service:theseus:Calculator|kind=operation|name=multiply|summary=Multiply the operands.|request=Operands|response=CalcResult' \
  --edit 'add|service:theseus:Calculator|kind=operation|name=divide|summary=Divide the operands.|request=Operands|response=CalcResult'
```

The reprojection writes the self-model source. The calculator crate's generated contract is deferred: a crate is registered before its skeleton exists, so its `generated.rs` is not written into a directory the workspace cannot yet build. Nothing under `rust/calculator` is created at this point.

## 2. Scaffold and generate the crate

Scaffolding writes the crate's authored leaves from the model — its `Cargo.toml` (`anyhow` plus a path dependency per modeled crate dependency, none here), its `lib.rs` module wiring and re-exports, and an empty `Calculator` adapter in `service.rs`:

```sh
theseus scaffold
#   scaffolded rust/calculator/Cargo.toml
#   scaffolded rust/calculator/src/lib.rs
#   scaffolded rust/calculator/src/service.rs
```

With the crate's manifest now present, generation fills its contract:

```sh
theseus generate
#   wrote rust/calculator/src/generated.rs
#   ...
```

The generated file holds the contract trait and the plain request struct, with no command surface and no command-line dependency:

```sh
sed -n '1,12p' rust/calculator/src/generated.rs
#   pub struct Operands { pub a: f64, pub b: f64 }
#   pub trait CalculatorService {
#       fn add(&self, _request: Operands) -> anyhow::Result<String> { ... }
#       ...
#   }
```

## 3. Author the calculator handlers

The calculator's four operations begin on their `unimplemented` defaults. Each is authored with `implement`, which resolves the operation to its service and writes the handler into that service's crate — here `rust/calculator/src/service.rs`:

```sh
theseus implement --method add      --body 'Ok(format!("{}", request.a + request.b))'
theseus implement --method subtract --body 'Ok(format!("{}", request.a - request.b))'
theseus implement --method multiply --body 'Ok(format!("{}", request.a * request.b))'
theseus implement --method divide   --body 'Ok(format!("{}", request.a / request.b))'
```

The calculator crate now compiles as a self-contained service:

```sh
cargo build -p theseus-calculator   #   Finished
```

## 4. Expose the service through the CLI

The cli's dependency on the calculator is an authored edit to `rust/cli/Cargo.toml`, paired with the matching edge in the model:

```toml
theseus-calculator = { path = "../calculator" }
```

```sh
theseus patch --write \
  --edit 'add|crate:theseus:theseus-cli|kind=dep|name=theseus-calculator' \
  --edit 'add|model:theseus|kind=type|name=CalcRequest|shape=struct:op=String:The operator: add, subtract, multiply, or divide.,a=f64:Left operand.,b=f64:Right operand.' \
  --edit 'add|service:theseus:Theseus|kind=operation|name=calc|summary=Evaluate an arithmetic expression through the calculator service.|request=CalcRequest|response=CalcResult' \
  --edit 'add|service:theseus:Theseus|kind=port|name=calculator|summary=Evaluates arithmetic through the calculator service.|target=Calculator'
```

The `calculator` port is bound to the `Calculator` service with `target=Calculator`, so its contract is that service's operations. The reprojection gives the cli's generated module the `calc` subcommand, the typed `CalcRequest` parser, and a composition-root field bound to the calculator's contract:

```sh
grep "calculator" rust/cli/src/generated.rs
#   pub calculator: &'a dyn theseus_calculator::CalculatorService,
```

Two authored leaves complete the wiring. The composition root in `rust/cli/src/main.rs` constructs the adapter and passes it into the context:

```rust
let calculator = theseus_calculator::Calculator;
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

## 5. Run and verify

```sh
cargo build -p theseus-cli
theseus calc --op add      --a 2 --b 3   #   5
theseus calc --op subtract --a 10 --b 4  #   6
theseus calc --op multiply --a 6 --b 7   #   42
theseus calc --op divide   --a 1 --b 8   #   0.125
```

```sh
theseus coverage          #   all operations implemented
theseus verify            #   conformant: workspace matches its self-model
```

Verification is conformant across all six checks. The `theseus-cli` to `theseus-calculator` dependency is a real crate edge, so the required-dependency and layering functors confirm the inter-service link with no check specific to it: the same machinery that holds Theseus's own layering holds the calculator's.

## Summary

A calculator was built as a separate service in its own crate and exposed through the CLI as a single `calc` operation. The crate, its dependency edge, the service, its operations, the response type, and the bound port were declared through `patch`. The crate skeleton was written by `scaffold`. Code generation rendered a contract into each crate. The four arithmetic handlers were authored through `implement` into the calculator crate. Verification confirmed the cli-to-calculator dependency through the existing crate-graph checks. The authored code was the cli's manifest dependency, two cli leaves, and four arithmetic handler bodies.

# Adding an operation

Adding an operation to Theseus requires two steps:

1. Describe the operation in the model — `theseus patch` (or an edit to `self_model.rs`).
2. Author its handler — `theseus implement` (or an edit to `service.rs`).

All other elements are generated. The command surface, the request parser, the service-trait method, the `Invocation` variant, and the dispatch are all derived from the single model edit. `generated.rs` is never edited, and `main.rs` is edited only for non-default output, such as an exit code or a follow-up notice.

The two steps are identical whether performed by a person through an editor or by an agent through the protocol. The protocol exists so that an agent, which lacks a general-purpose file editor, can perform the same edits as typed, hash-checked operations.

A larger example — a calculator built as a separate service in its own crate, with a shared request type and four operations, exposed through the CLI as a single `calc` operation — is given in [building a calculator](building-a-calculator.md).

## Setup

No binary is installed. The CLI runs through Cargo. The following alias abbreviates the invocations below:

```sh
alias theseus='cargo run -q -p theseus-cli --'
```

All commands below assume this alias and the repository root as the working directory.

## Walkthrough

This walkthrough adds a temporary `greet` operation that takes no request and returns a greeting string, then removes it. The operation may be retained by substituting other names.

### 1. Describe the operation in the model

An operation references a request type and a response type, so any new type is added first. `greet` requires a response type. The example adds a foreign type backed by `String`. Each edit is checked against the model hash, so the current hash is read first.

```sh
theseus query | grep model_hash
#   "model_hash": "8e13f194ff830a8f",
```

Add the response type, supplying that hash:

```sh
theseus patch --write \
  --verb add --target model:theseus --kind type --name Greeting \
  --set 'shape=foreign:String' \
  --expect-model-hash 8e13f194ff830a8f
#   "diff": [ "+ type Greeting (foreign:String)" ]
#   applied `add` and reprojected. Rebuild, then `coverage` shows any handler left to author
```

Each `--write` reprojects the model, changing the hash. After re-reading it, add the operation (the request defaults to `Empty`):

```sh
theseus query | grep model_hash      # note the new hash, e.g. 3a9d4587db6d9e90

theseus patch --write \
  --verb add --target model:theseus --kind operation --name greet \
  --set 'summary=Greet the user.' --set response=Greeting \
  --expect-model-hash 3a9d4587db6d9e90
#   "diff": [ "+ operation greet (Empty => Greeting)" ]
```

`--target model:theseus` attaches the new node to the model root. `--kind` names the node to add. The same verb adds methods (`--target port:theseus:workspace --kind method`), fields, and variants. See [the edit vocabulary](#the-edit-vocabulary).

### 2. Build

```sh
cargo build -p theseus-cli
#   Finished `dev` profile ...
```

The build succeeds as soon as the operation exists. The trait method defaults to an `unimplemented` error (no `E0046`), and the dispatch is generated (no `E0004`), so the operation is callable immediately. It reports itself as unimplemented.

### 3. Identify the gap

```sh
theseus coverage
#   {
#     "total": 8,
#     "implemented": 7,
#     "unimplemented": [
#       { "name": "greet", "summary": "Greet the user.",
#         "request": "Empty", "response": "Greeting" }
#     ]
#   }
```

`coverage` lists every operation still on its `unimplemented` default, together with the request and response types to be implemented. `verify` fails on the same list, enforcing the requirement the compiler no longer does, so a continuous-integration run flags an unimplemented operation.

### 4. Author the handler

`implement` renders the handler signature from the model and writes a method into `service.rs`. Only the body is supplied. The command is hash-checked, so the hash is read again.

```sh
theseus query | grep model_hash      # note the hash

theseus implement --method greet \
  --body 'Ok("hello from Theseus".to_string())' \
  --expect-model-hash <hash>
#   wrote the handler for `greet` into rust/cli/src/service.rs. Rebuild to load it
```

The body is plain Rust. It may use `self.model`, the wired ports (`self.workspace`), and any item `service.rs` imports. A request-taking operation receives `request` (see [variations](#with-arguments)). A body too long for a single shell string may be supplied through `--body-file -` (stdin, via a heredoc) or `--body-file <path>`.

### 5. Run the operation

```sh
cargo run -q -p theseus-cli -- greet
#   hello from Theseus
```

The result is a `String`, so the generated `dispatch` prints it as text. A structured (non-`String`) response is printed as pretty JSON.

### 6. Verify

```sh
theseus coverage          # 8 / 8 implemented
theseus verify
#   ✓ crate graph: required dependencies present
#   ✓ crate graph: dependency direction (layering functor)
#   ✓ types: every reference resolves to a definition
#   ✓ ports: every service-targeting port resolves to a service
#   ✓ inbounds: every inbound adapter drives a defined service
#   ✓ generated code: in sync with model (drift gate)
#   ✓ operations: every operation has an authored handler
#   conformant: workspace matches its self-model
```

### 7. Remove the temporary operation

To retain `greet`, omit this step. To remove it, restore the model and handler, then reproject the generated code:

```sh
git checkout HEAD -- rust/model/src/self_model.rs rust/cli/src/service.rs
theseus generate
theseus verify        # conformant
```

`patch --verb remove --target op:theseus:greet` removes the operation from the model, but the orphaned handler in `service.rs` must be removed manually, because a trait implementation cannot contain a method the trait no longer declares.

## Viewing and revising a handler

`implement` is not limited to insertion. `show` returns a handler's current source, and `implement` rewrites it in place, so a handler can be read and revised entirely through the protocol, without a file editor.

```sh
theseus show --method greet
#   fn greet(&self) -> anyhow::Result<Greeting> {
#       Ok("hello from Theseus".to_string())
#   }

theseus implement --method greet \
  --body 'Ok(format!("hello, {} operations", self.model.operations().len()))' \
  --expect-model-hash <hash>
#   wrote the handler for `greet` into rust/cli/src/service.rs. Rebuild to load it
```

The rewrite is precise: `implement` locates the method by its exact source span and replaces only that method, leaving every other handler unchanged. For an operation still on its default, `show` returns the signature it would have, marked as falling through to the default — the information `coverage` reports, for a single operation.

## Variations

### With arguments

`greet` takes `Empty`. To accept arguments, add a struct request type first. Its fields become command-line flags automatically and are parsed into the request struct the handler receives.

```sh
theseus patch --write \
  --verb add --target model:theseus --kind type --name GreetRequest \
  --set 'shape=struct:name=String' \
  --expect-model-hash <hash>

# set the field's help text
theseus patch --write \
  --verb set --target field:theseus:GreetRequest.name \
  --set 'doc=Who to greet.' \
  --expect-model-hash <hash>

# the operation now takes the request
theseus patch --write \
  --verb add --target model:theseus --kind operation --name greet \
  --set 'summary=Greet someone.' --set request=GreetRequest --set response=Greeting \
  --expect-model-hash <hash>
```

`generate` renders a `--name` flag from the `name` field. The handler reads it:

```sh
theseus implement --method greet \
  --body 'Ok(format!("hello {}", request.name))' \
  --expect-model-hash <hash>

cargo run -q -p theseus-cli -- greet --name World
#   hello World
```

Field types determine the flag shape: `bool` is a flag, `Vec<T>` a repeatable value, `Option<T>` an optional value, and any other type a required value.

### With custom output

The generated `dispatch` prints a `String` as text and any other type as pretty JSON. This covers most operations. When an operation requires an exit code, per-file lines, or a follow-up notice, add an arm to the overrides in `run()` in `rust/cli/src/main.rs`, the only location in the composition root that is edited:

```rust
Invocation::Greet => {
    let report = service.greet()?;
    // custom rendering, exit codes, …
}
```

Every operation without such an arm falls through to `generated::dispatch`, so an ordinary operation requires no change here.

### Editing directly, without the protocol

The actions of `patch` and `implement` may also be performed directly:

1. Edit `rust/model/src/self_model.rs` — add the `.foreign_type(...)` or `.struct_type(...)` call and the `.operation(...)` call. The file is plain Rust and may be edited directly.
2. Run `theseus generate` to refresh `generated.rs` (omitting this fails the drift-gate test).
3. Author the handler in `impl TheseusService for Ctx` in `rust/cli/src/service.rs`.

The result and the gates are identical. The protocol is the editor-free path used by an agent. The direct path performs the same two steps through a text editor.

## Reference

### Generated versus authored files

| File                           | Owner                                  | Contents                                                                                                                |
| ------------------------------ | -------------------------------------- | ----------------------------------------------------------------------------------------------------------------------- |
| `rust/cli/src/generated.rs`    | `generate` (`// @generated`)           | command surface, request structs, `TheseusService` trait, `Invocation`, dispatch, port traits, `Ctx` |
| `rust/model/src/self_model.rs` | `generate` / `patch` (`// @generated`) | the model, projected to its builder form (the fixed point)                                                              |
| `rust/cli/src/service.rs`      | authored / `implement`                 | the operation handlers (`impl TheseusService for Ctx`)                                                                  |
| `rust/cli/src/main.rs`         | authored                               | composition root, adapters, output overrides                                                            |

### Gates

| Gate                                           | Property enforced                                                                                            |
| ---------------------------------------------- | ------------------------------------------------------------------------------------------------------------ |
| `--expect-model-hash` on `patch` / `implement` | the edit is computed against the model last observed. A concurrent change is refused with a coded diagnostic |
| reference safety in `patch`                    | no operation references an undefined type, and no type is removed while referenced                           |
| `coverage`                                     | the derived list of operations still on their `unimplemented` default                                        |
| `verify`                                       | required dependencies, layering direction, type references, port targets, inbound services, generated drift, implementation coverage |

### The edit vocabulary

`patch` provides four verbs over the handles `query` produces:

```
theseus patch --verb add    --target <parent-handle> --kind <kind> --name <name> [--set k=v …]
theseus patch --verb remove --target <handle>
theseus patch --verb rename --target <handle> --to <name>
theseus patch --verb set    --target <handle> --set k=v …
```

Handles take the forms `op:theseus:<name>`, `type:theseus:<name>`, `port:theseus:<name>`, `crate:theseus:<name>`, `service:theseus:<name>`, `inbound:theseus:<name>`, and the nested `method:theseus:<port>.<name>`, `field:theseus:<type>.<name>`, `variant:theseus:<type>.<name>`, and `dep:theseus:<crate>.<dep>`. The model root is `model:theseus`. `kind` for an `add` is one of `operation`, `type`, `port`, `method`, `field`, `variant`, `crate`, `dep`, `service`, or `inbound` — the crate-and-service kinds are exercised in [building a calculator](building-a-calculator.md).

Type shapes (`--set shape=…`) are `newtype:Inner`, `foreign:Path`, `enum:A,B,C`, and `struct:field=Type,field=Type`. A struct field may carry its documentation inline as `field=Type:doc`, and a non-`String` field type is parsed and validated as that type on the command line.

Several edits apply under one hash check with a repeatable `--edit 'verb|target|key=value…'`, supplied in place of `--verb` and `--target`. The batch is atomic: it is refused as a whole if any edit is refused. A worked batch is given in [building a calculator](building-a-calculator.md).

### Troubleshooting

- `stale model hash` — read the current hash with `theseus query` and retry. A `--write` changes the hash, so re-read it between chained edits.
- `no operation named …` or `already has a handler` from `implement` — the method must name a current operation that is unimplemented. Run `theseus coverage`.
- drift-gate failure (`theseus_conforms_to_its_self_model`) — `self_model.rs` was edited without running `theseus generate`.

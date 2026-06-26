# Building a Calculator

This document presents a complete worked example: a four-operation calculator with a shared, typed request, built end to end through the `theseus` CLI without manual edits to any file. It exercises the workflow across multiple operations. The mechanics of a single operation are described in [adding an operation](adding-an-operation.md). This document builds on those concepts.

The calculator comprises a shared `Operands` request (two numbers), a `CalcResult` response, and the operations `add`, `subtract`, `multiply`, and `divide`.

## Setup

```sh
alias theseus='cargo run -q -p theseus-cli --'
```

All commands assume this alias and the repository root as the working directory.

## 1. Describe the calculator in one batch

The complete shape — a response type, a typed request with inline field documentation, and four operations — is declared in a single `patch`. A repeatable `--edit` argument carries one `verb|target|key=value…` specification each, applied in order after a single hash check, so a service is created in one hash-checked invocation rather than one per edit. The hash is read once:

```sh
H=$(theseus query | grep -o '"model_hash": "[^"]*"' | head -1 | cut -d'"' -f4)

theseus patch --write --expect-model-hash "$H" \
  --edit 'add|model:theseus|kind=type|name=CalcResult|shape=foreign:String' \
  --edit 'add|model:theseus|kind=type|name=Operands|shape=struct:a=f64:Left operand.,b=f64:Right operand.' \
  --edit 'add|model:theseus|kind=operation|name=add|summary=Add.|request=Operands|response=CalcResult' \
  --edit 'add|model:theseus|kind=operation|name=subtract|summary=Subtract.|request=Operands|response=CalcResult' \
  --edit 'add|model:theseus|kind=operation|name=multiply|summary=Multiply.|request=Operands|response=CalcResult' \
  --edit 'add|model:theseus|kind=operation|name=divide|summary=Divide.|request=Operands|response=CalcResult'
#   "diff": [ "+ type CalcResult (foreign:String)", "+ type Operands (struct:…)",
#             "+ operation add (Operands => CalcResult)", … ]
```

Two aspects of the `Operands` specification are notable. First, the fields are `f64` rather than `String`; a non-`String` field is parsed and validated as its declared type, so the handler receives numeric values. Second, each field carries its documentation inline as `name=Type:doc`, which becomes the argument's `--help` text, eliminating a separate edit per field.

If any edit in the batch is refused, the entire batch is refused and nothing is written; the model hash is unchanged, so it is re-read before a retry.

## 2. Build and identify the gaps

```sh
cargo build -p theseus-cli
#   Finished `dev` profile ...
```

The build succeeds: every new trait method defaults to `unimplemented`, and the presentation is generated. The command surface is already available, with the field documentation as help text and the values typed:

```sh
theseus add --help
#   Usage: theseus add --a <a> --b <b>
#   Options:
#         --a <a>  Left operand.
#         --b <b>  Right operand.
```

```sh
theseus coverage      #   8 implemented of 12; gaps: add, subtract, multiply, divide
```

## 3. Author the handlers

The operands arrive already typed, so the bodies are arithmetic and require no parsing. `implement` writes the handler into `service.rs` and leaves the model unchanged, so the hash is constant across all four invocations:

```sh
H=$(theseus query | grep -o '"model_hash": "[^"]*"' | head -1 | cut -d'"' -f4)
theseus implement --method add      --body 'Ok(format!("{}", request.a + request.b))' --expect-model-hash "$H"
theseus implement --method subtract --body 'Ok(format!("{}", request.a - request.b))' --expect-model-hash "$H"
theseus implement --method multiply --body 'Ok(format!("{}", request.a * request.b))' --expect-model-hash "$H"
theseus implement --method divide   --body 'Ok(format!("{}", request.a / request.b))' --expect-model-hash "$H"
```

A body too long for a single shell string may be supplied through `implement --body-file -` (stdin, via a heredoc) or `--body-file <path>`.

## 4. Run the calculator

```sh
cargo build -p theseus-cli
theseus add --a 2 --b 3        #   5
theseus subtract --a 10 --b 4  #   6
theseus multiply --a 6 --b 7   #   42
theseus divide --a 1 --b 8     #   0.125
```

A non-numeric argument is rejected by the command surface before any handler runs, because the argument is validated as `f64`.

## 5. Verify

```sh
theseus coverage          #   12 / 12 implemented
theseus verify            #   conformant: workspace matches its self-model
```

## Summary

The complete calculator — a response type, a typed request with two documented fields, four operations, and four handlers — was built with one `patch` invocation and four `implement` invocations, with no file edits. The CLI generated the four subcommands, the typed `--a` and `--b` parsing, the trait, the dispatch, and the text presentation; the only authored code was the four arithmetic handler bodies.

## Cleanup

The calculator was added to Theseus's own model for demonstration. Restore the model and handlers, then reproject:

```sh
git checkout HEAD -- rust/model/src/self_model.rs rust/cli/src/service.rs
theseus generate
theseus verify        #   conformant
```

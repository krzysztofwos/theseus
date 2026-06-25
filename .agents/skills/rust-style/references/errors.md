# Error handling — detail

## Library vs application errors

| Crate type     | Error tool  | When                                                                  |
| -------------- | ----------- | --------------------------------------------------------------------- |
| Library / port | `thiserror` | Always. Domain APIs, repository traits, anything callable.            |
| Binary `main`  | `anyhow`    | Only for human-consumption errors at the top level (logs, exit code). |

`crates/smithkit-node` is a binary, but its internal modules (identity index, relay, sponsor queue) are library-shaped. Their public APIs return structured errors (`IdentityIndexError`, `RelayError`). Don't sprinkle `anyhow::Error` through those boundaries.

## Structured error enums

```rust
#[derive(thiserror::Error, Debug)]
pub enum ParseError {
    #[error("invalid syntax at position {position}")]
    InvalidSyntax { position: usize },

    #[error("unexpected token: {0}")]
    UnexpectedToken(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),
}
```

- Named-field variants when the fields carry meaning beyond their type.
- Tuple variants for a single transparent wrap.
- `#[error("…")]` strings: lowercase, no trailing period, enough context to be useful in a log line without the full chain.

## Composition with `#[from]`

```rust
#[derive(thiserror::Error, Debug)]
pub enum ServiceError {
    #[error(transparent)]
    Database(#[from] DatabaseError),

    #[error(transparent)]
    Validation(#[from] ValidationError),

    #[error("service-specific error: {0}")]
    ServiceSpecific(String),
}
```

`#[from]` gives `?` propagation for free. `#[error(transparent)]` ensures the rendered message is the wrapped error's message — no `"service error: database error: connection refused"` triple-wrap.

## The escape hatch

```rust
#[derive(thiserror::Error, Debug)]
pub enum SaveError {
    #[error("entity already exists with id {id}")]
    AlreadyExists { id: String },

    #[error("optimistic lock failure")]
    ConcurrentModification,

    #[error(transparent)]
    Unknown(anyhow::Error),
}
```

`Unknown(anyhow::Error)` is acceptable as a catch-all for unforeseen underlying errors — it keeps the public API structured while still wrapping arbitrary sources. Use it sparingly; if a specific error case keeps showing up under `Unknown`, promote it to a named variant.

## Bad patterns to avoid

```rust
// Loses type safety, callers can't match.
fn parse(input: &str) -> Result<Ast, String> {
    Err("parse failed".to_string())
}

// Forces downcasting on callers.
fn save(...) -> Result<(), Box<dyn Error>> { ... }
```

If callers need `err.downcast_ref::<X>()` to make decisions, the error type is wrong — split the wrapper into named variants.

## Rules

1. Codify all error states in your public API. If a caller might handle a case differently, it deserves a named variant.
2. Design errors for the audience. Library callers need structure; application logs need clarity. Don't conflate.
3. Preserve chains. `#[from]` and `source()` keep the cause visible.
4. Errors are `'static` by default — design assuming they outlive their origin context.
5. No `Result<T, String>`. Anywhere. Even in scripts.

# Module organization, testing, docs, macros — detail

## Module layout

```rust
mod user {
    mod model;        // Data structures
    mod service;      // Business logic
    mod repository;   // Persistence

    pub use model::{User, UserId};
    pub use service::UserService;
    // Keep repository module-private.
}
```

Split a file once it mixes concerns or grows past a few hundred lines. A monolithic `mod user` with three thousand lines hides the seams that should be visible.

## Visibility

- Default to `pub(crate)` or `pub(super)`.
- Promote to `pub` only when a stable external interface needs it.
- Use a facade module that re-exports the intended surface; keep implementation modules private.
- Document every `pub` item.

## Testing

Co-locate unit tests with the code they test:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_input() {
        // Test one specific behaviour.
    }
}
```

Put integration tests in `tests/` at the crate root; each file becomes its own crate.

A builder pattern for test fixtures keeps the noise out of each test:

```rust
struct TestUserBuilder {
    name: String,
    email: String,
}

impl TestUserBuilder {
    fn new() -> Self {
        Self {
            name: "Test User".into(),
            email: "test@example.com".into(),
        }
    }

    fn with_name(mut self, name: &str) -> Self {
        self.name = name.into();
        self
    }

    fn build(self) -> User {
        User::new(self.name, self.email).unwrap()
    }
}
```

For domain types under `crates/smithkit-node`, prefer in-memory adapters (see the `rust-hexagonal` skill) over mocking frameworks. Per CLAUDE.md Hard Rule 1: no mocks in production code; `#[cfg(test)] mod test_doubles { … }` is exactly the right scope.

## Documentation

````rust
/// Parse a lambda expression into an AST.
///
/// # Arguments
///
/// * `input` - The lambda expression as a string
///
/// # Returns
///
/// The parsed AST or a parsing error
///
/// # Examples
///
/// ```
/// let ast = parse("λx.x")?;
/// assert_eq!(ast, Lam::Lambda("x", Box::new(Lam::Var("x"))));
/// ```
///
/// # Errors
///
/// Returns `ParseError` if the input is syntactically invalid.
pub fn parse(input: &str) -> Result<Lam, ParseError> {
    // ...
}
````

Module-level docs go at the top of the file with `//!`:

```rust
//! # User Management Module
//!
//! Handles registration, authentication, and profile management.
```

## Macros

Use macros only when a function won't suffice. Prefer declarative (`macro_rules!`) for simple cases; reach for proc-macros only when the input genuinely cannot be expressed as a function.

```rust
/// Define a thiserror enum with named variants.
macro_rules! define_error {
    ($name:ident { $($variant:ident($ty:ty)),* $(,)? }) => {
        #[derive(Debug, thiserror::Error)]
        pub enum $name {
            $(
                #[error(stringify!($variant))]
                $variant($ty),
            )*
        }
    };
}
```

Document the input syntax and the expanded output. Test macros with a range of inputs — they are harder to debug than functions because errors point at the call site, not the definition.

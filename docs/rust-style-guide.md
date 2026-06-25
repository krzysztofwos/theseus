# Rust Style Guide

This guide consolidates best practices from production Rust development, focusing on error handling, type design, architecture patterns, and performance considerations.

## 1. Error Handling

### Core Principles

**Use `thiserror` for library errors and domain boundaries:**

- Define structured error types at module/crate boundaries
- Preserve error context through the call stack
- Never use `Result<T, String>` - it violates type safety

**Use `anyhow` for application-level error handling:**

- Only in binary crates, never in libraries
- When the error is for human consumption (logs, UI)
- When you don't need programmatic error handling

### Error Design Patterns

```rust
// Good: Structured error with thiserror
#[derive(thiserror::Error, Debug)]
pub enum ParseError {
    #[error("invalid syntax at position {position}")]
    InvalidSyntax { position: usize },

    #[error("unexpected token: {0}")]
    UnexpectedToken(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),
}

// Bad: String errors lose type safety
fn parse(input: &str) -> Result<Ast, String> {
    Err("parse failed".to_string()) // Never do this!
}
```

### Error Composition

When composing errors from multiple sources:

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

### Key Rules

1. **Codify all error states in your public API** - Use enums to represent all possible failures
2. **Design errors for your audience** - Library errors need structure, application errors need clarity
3. **Preserve error chains** - Use `#[from]` and `source()` to maintain error context
4. **Avoid forcing downcasting** - If users need to downcast your errors, redesign them
5. **Be `'static` by default** - Errors often outlive their origin context

## 2. Type Design and Newtypes

### Newtype Philosophy

**Parse, don't validate:** Make invalid states unrepresentable through the type system.

```rust
// Good: Email type that guarantees validity
pub struct Email(String);

impl Email {
    pub fn parse(s: &str) -> Result<Self, EmailError> {
        validate_email(s)?;
        Ok(Email(s.to_string()))
    }
}

// Bad: Stringly-typed with repeated validation
pub fn send_email(email: &str) -> Result<(), Error> {
    validate_email(email)?; // Validation scattered throughout codebase
    // ...
}
```

### Newtype Implementation Pattern

```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct UserId(u64);

impl UserId {
    pub fn new(id: u64) -> Result<Self, ValidationError> {
        if id == 0 {
            return Err(ValidationError::InvalidId);
        }

        Ok(UserId(id))
    }

    // Provide controlled access
    pub fn as_u64(&self) -> u64 {
        self.0
    }
}

// Implement conversions judiciously
impl From<UserId> for u64 {
    fn from(id: UserId) -> Self {
        id.0
    }
}

// Avoid Deref unless truly transparent
// impl Deref for UserId { ... } // Usually wrong!
```

### When to Use Newtypes

- **Domain identifiers:** `UserId`, `OrderId`, `SessionToken`
- **Validated strings:** `Email`, `PhoneNumber`, `Url`
- **Units of measure:** `Meters`, `Seconds`, `Bytes`
- **State tracking:** `Validated<T>`, `Sanitized<T>`

## 3. Architecture Patterns

### Hexagonal Architecture

Structure your application with clear boundaries:

```text
src/
├── domain/       # Core business logic (no external deps)
├── ports/        # Trait definitions for external services
├── adapters/     # Implementations of ports
├── services/     # Application services orchestrating domain
└── api/          # External interfaces (HTTP, CLI)
```

### Domain Boundaries

```rust
// Domain model - pure business logic
pub struct Order {
    id: OrderId,
    items: Vec<OrderItem>,
    status: OrderStatus,
}

// Port - interface to external world
pub trait OrderRepository {
    async fn save(&self, order: &Order) -> Result<(), RepositoryError>;
    async fn find(&self, id: OrderId) -> Result<Option<Order>, RepositoryError>;
}

// Adapter - concrete implementation
pub struct PostgresOrderRepository { /* ... */ }

impl OrderRepository for PostgresOrderRepository {
    // Implementation details hidden from domain
}
```

### Dependency Management

- Domain depends on nothing
- Ports are owned by the domain
- Adapters depend on ports
- Services orchestrate domain and ports
- API layer depends on services

## 4. Ownership and Memory Management

### Smart Pointer Guidelines

**Use `Box<T>` for:**

- Heap allocation when size unknown at compile time
- Breaking recursive type definitions
- Trait objects (`Box<dyn Trait>`)

**Use `Rc<T>` for:**

- Shared ownership within a single thread
- Graph-like data structures
- Only when lifetime is truly unclear

**Use `Arc<T>` for:**

- Shared ownership across threads
- Long-lived shared state
- Consider `Arc<Mutex<T>>` or `Arc<RwLock<T>>` for mutation

**Use `RefCell<T>` sparingly:**

- Interior mutability when required by API
- Prefer redesigning to avoid it
- Always document invariants

### Memory Management Best Practices

```rust
// Prefer moving over cloning
let data = expensive_computation();
process(data); // Move, don't clone

// Use references when possible
fn process(data: &Data) -> Result<(), Error> {
    // Borrow, don't take ownership
}

// Clone explicitly when needed
let shared_data = data.clone(); // Make cloning visible
```

## 5. Performance Considerations

### Optimization Guidelines

1. **Measure first** - Use benchmarks before optimizing
2. **Algorithmic improvements** beat micro-optimizations
3. **Allocate thoughtfully** - Reuse buffers, use `with_capacity`
4. **Consider `Cow<'a, T>`** for conditional cloning
5. **Profile in release mode** - Debug builds mislead

### Common Patterns

```rust
// Pre-allocate collections
let mut vec = Vec::with_capacity(expected_size);

// Reuse buffers
let mut buffer = String::new();
for item in items {
    buffer.clear();
    write_to_buffer(&mut buffer, item);
    process(&buffer);
}

// Use iterators over collecting
let sum: i32 = values
    .iter()
    .filter(|x| x.is_valid())
    .map(|x| x.value())
    .sum(); // No intermediate Vec
```

## 6. Code Organization

### Module Structure

```rust
// Good: Clear module organization
mod user {
    mod model;      // Data structures
    mod service;    // Business logic
    mod repository; // Persistence

    pub use model::{User, UserId};
    pub use service::UserService;
    // Keep repository private
}

// Bad: Everything in one file
mod user; // 3000 lines of mixed concerns
```

### Visibility Guidelines

- Start with maximum privacy (`pub(crate)`, `pub(super)`)
- Expose only stable interfaces as `pub`
- Document public API thoroughly
- Use facade pattern for complex modules

## 7. Testing Strategies

### Test Organization

```rust
#[cfg(test)]
mod tests {
    use super::*;

    // Unit tests close to implementation
    #[test]
    fn test_specific_behavior() {
        // Test one thing
    }
}

// Integration tests in tests/ directory
// tests/integration_test.rs
```

### Test Patterns

```rust
// Use builder pattern for test data
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

## 8. Documentation Standards

### Code Documentation

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
    // Implementation
}
````

### Module Documentation

```rust
//! # User Management Module
//!
//! This module provides user authentication and management.
//!
//! ## Overview
//!
//! The user system handles:
//! - Registration and validation
//! - Authentication and sessions
//! - Profile management
```

## 9. Async Patterns

### Async Best Practices

```rust
// Prefer async-trait for traits with async methods
#[async_trait]
pub trait Repository {
    async fn find(&self, id: Id) -> Result<Option<Entity>, Error>;
}

// Avoid blocking in async contexts
async fn process() {
    // Bad: blocks the executor
    std::thread::sleep(Duration::from_secs(1));

    // Good: yields to executor
    tokio::time::sleep(Duration::from_secs(1)).await;
}
```

## 10. Macro Usage

### Macro Guidelines

- Use macros only when functions won't suffice
- Prefer declarative macros (`macro_rules!`) for simple cases
- Document macro inputs and outputs clearly
- Test macros thoroughly with various inputs

```rust
/// Creates a new error type with the given name and variants
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

## Summary

This guide emphasizes:

- **Type safety** through newtypes and structured errors
- **Clear boundaries** via hexagonal architecture
- **Performance** through measurement and iteration
- **Maintainability** via documentation and testing

Remember: Good Rust code makes invalid states unrepresentable and errors impossible to ignore.

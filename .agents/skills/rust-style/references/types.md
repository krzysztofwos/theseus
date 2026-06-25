# Type design and newtypes — detail

## Parse, don't validate

A newtype that constructs through a fallible parser guarantees validity for the rest of the program. Validation lives in one place — the constructor — and the type system enforces it everywhere else.

```rust
pub struct Email(String);

impl Email {
    pub fn parse(s: &str) -> Result<Self, EmailError> {
        validate_email(s)?;
        Ok(Email(s.to_string()))
    }
}
```

Compare to the anti-pattern, where validation is scattered:

```rust
// Bad: each call site must remember to validate.
pub fn send_email(email: &str) -> Result<(), Error> {
    validate_email(email)?;
    // ...
}
```

## Newtype shape

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

    pub fn as_u64(&self) -> u64 {
        self.0
    }
}

impl From<UserId> for u64 {
    fn from(id: UserId) -> Self {
        id.0
    }
}
```

- Derive `Debug, Clone, PartialEq, Eq, Hash` by default. Add `Copy` only for tiny value types where copying is cheaper than borrowing.
- Constructors that can fail return `Result`; constructors that cannot are named `new` / `generate`.
- Accessors: `as_u64(&self) -> u64`, `as_str(&self) -> &str`. Never `pub` the inner field.
- `From<NewType> for Inner` is fine; `Deref<Target = Inner>` almost always isn't — it makes the newtype transparent for method dispatch and undoes the type-level constraint.

## When to use a newtype

- **Domain identifiers** — `UserId`, `OrderId`, `OrgSlug`, `Username`. Prevents accidentally passing a `CustomerId` where a `UserId` is expected.
- **Validated strings** — `Email`, `PhoneNumber`, `Url`. Validation happens once.
- **Units of measure** — `Meters`, `Seconds`, `Bytes`. Prevents adding `Meters + Seconds` at compile time.
- **State tracking** — `Validated<T>`, `Sanitized<T>`. Encodes a check that has already happened.

## When not to bother

- Throwaway local variables.
- Types where the only operation is "the same as the inner type, but named differently."
- Cases where a function signature already constrains the value (`fn foo(n: NonZeroU32)`).

## Cross-language constants

SmithKit-specific: some newtypes wrap values that are load-bearing across the Rust/JS boundary (`OrgSlug`, sigchain field names, HKDF info strings). Per CLAUDE.md: changing the validation rule on one side without the other will silently break interop. When in doubt, grep for the constant in both `crates/` and `sdk/packages/`.

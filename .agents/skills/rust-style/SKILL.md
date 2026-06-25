---
name: rust-style
description: "Apply production Rust style for this codebase — thiserror for libraries and domain APIs, anyhow only in binaries, newtypes for domain values (parse don't validate), measured smart-pointer choice (Box/Rc/Arc/RefCell), async patterns that don't block the executor, and module organization that starts maximally private. Use whenever writing or reviewing Rust in this repo: adding a Result type, designing a struct that wraps a String/Uuid/u64, choosing Box vs Arc, writing an async trait, organizing a new module, composing errors with #[from], or reviewing a PR that touches error variants, public APIs, or async behaviour."
---

# Rust Style

Production Rust patterns for this codebase. These rules supersede generic Rust advice — they reflect what has worked here and what has gone wrong.

The long-form guide is at `docs/rust-style-guide.md`. This skill is the operational summary with pointers to detailed examples. The hexagonal-architecture sibling skill (`rust-hexagonal`) covers cross-module structure; this one covers the patterns inside each module.

## Errors

**Libraries and domain APIs use `thiserror`.** Define structured error enums; never `Result<T, String>`. Use `#[from]` and `#[error(transparent)]` to compose. Recent refactors in this repo (`RelayError`, `IdentityIndexError`) drop `anyhow` from API surfaces in favour of typed errors — extend that direction, don't reverse it.

**`anyhow` is for binaries only**, and only where the error is for human consumption (top-level logs, the `main()` return type). Inside any library-shaped module — even one that lives in `crates/smithkit-node` — return structured errors.

**Errors are `'static` by default** and often outlive their origin context — design accordingly.

Detailed composition patterns and the rules table: [references/errors.md](references/errors.md).

## Types and newtypes

**Parse, don't validate.** Wrap primitives in a newtype that guarantees validity at construction. The constructor returns `Result<Self, ValidationError>`; downstream code can trust the type without re-checking.

Use newtypes for:

- Domain identifiers (`UserId`, `OrderId`, `OrgSlug`, `Username`)
- Validated strings (`Email`, `Url`)
- Units of measure (`Bytes`, `Seconds`)
- State-tracking wrappers (`Validated<T>`, `Sanitized<T>`)

Do not implement `Deref` on a newtype unless the wrapper is genuinely transparent — it usually defeats the point. Expose controlled accessors (`as_u64`, `as_str`) instead.

Full pattern with `From`/`TryFrom` guidance: [references/types.md](references/types.md).

## Ownership and async

- `Box<T>` — heap allocation for known-but-unsized values, recursive types, trait objects.
- `Rc<T>` — single-threaded shared ownership. Reach for it only when ownership genuinely cannot be expressed by borrowing.
- `Arc<T>` — cross-thread shared ownership. For mutation, `Arc<Mutex<T>>` or `Arc<RwLock<T>>`.
- `RefCell<T>` — interior mutability of last resort; document the invariants if you reach for it.

Prefer moves over clones; clone explicitly when you mean it. Take `&T` by default in function signatures.

For async: never `std::thread::sleep` in an async context — it blocks the executor. Use `tokio::time::sleep(…).await`. This codebase returns `impl Future<…> + Send` from trait methods rather than `#[async_trait]`; follow the surrounding code.

Detailed guidance and ownership patterns: [references/patterns.md](references/patterns.md).

## Module organization

- Start with maximum privacy: `pub(crate)` and `pub(super)` first; promote to `pub` only at stable interfaces.
- Split files by concern (`model`, `service`, `repository`) once a single file mixes concerns or grows past a few hundred lines.
- Pre-allocate (`Vec::with_capacity`) and reuse buffers in hot loops; prefer iterator chains over intermediate `Vec`s.

More on testing scaffolding, doc comments, and macros: [references/organization.md](references/organization.md).

## Key rules — non-negotiable

1. No `Result<T, String>` in any code. Define an enum.
2. No `anyhow` in libraries. `thiserror` at boundaries; convert with `#[from]`.
3. Preserve error chains. Use `#[error(transparent)]` for pass-through variants.
4. Don't force callers to downcast. If they have to, the error type is wrong — redesign.
5. Don't silence failing tests, `#[allow(dead_code)]`, or comment out assertions to make CI green (CLAUDE.md Hard Rule 3).

If anything in this skill conflicts with `docs/rust-style-guide.md`, treat the source doc as authoritative and update both.

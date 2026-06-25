# Ownership, async, and performance — detail

## Smart pointer selection

| Pointer       | Use when                                                                 |
| ------------- | ------------------------------------------------------------------------ |
| `Box<T>`      | Known-but-unsized values, recursive types, trait objects (`Box<dyn T>`). |
| `Rc<T>`       | Single-threaded shared ownership; graph-like structures.                 |
| `Arc<T>`      | Cross-thread shared ownership; long-lived shared state.                  |
| `Arc<Mutex>`  | Cross-thread shared _mutable_ state when contention is low.              |
| `Arc<RwLock>` | Cross-thread shared mutable state with many readers, few writers.        |
| `RefCell<T>`  | Interior mutability of last resort; document the invariants if used.     |

Reach for the simplest one that works. `Arc<Mutex<HashMap>>` is fine for a registry; if you're tempted to wrap that in another `Arc`, step back.

## Move, borrow, clone

```rust
// Move when the caller doesn't need the value anymore.
let data = expensive_computation();
process(data);

// Borrow by default in function signatures.
fn process(data: &Data) -> Result<(), Error> { ... }

// Clone explicitly — it should be visible at the call site.
let shared = data.clone();
```

If you find yourself cloning to "satisfy the borrow checker" in a hot path, the structure is usually wrong — restructure so the data flows through ownership rather than around it.

## Async patterns

```rust
// Bad — blocks the entire executor thread.
async fn slow() {
    std::thread::sleep(Duration::from_secs(1));
}

// Good — yields to the executor.
async fn slow() {
    tokio::time::sleep(Duration::from_secs(1)).await;
}
```

For trait methods, this codebase prefers native `impl Future<Output = …> + Send` returns:

```rust
pub trait Repository: Clone + Send + Sync + 'static {
    fn find(&self, id: Id)
        -> impl Future<Output = Result<Option<Entity>, Error>> + Send;
}
```

This avoids the `#[async_trait]` macro and the implicit `Box<dyn Future>` it generates. Follow the surrounding code if a module already uses `#[async_trait]` — don't mix styles in one file.

Other async rules:

- Don't call blocking filesystem or network APIs inside async functions. Use `tokio::fs`, `reqwest`, `sqlx`, etc.
- For CPU-bound work, use `tokio::task::spawn_blocking`.
- `tokio::select!` for racing futures; `futures::join!` or `tokio::try_join!` for waiting on independent futures.

## Performance

1. **Measure first.** Use `cargo bench`, `criterion`, or production profiling before optimizing. Debug-mode timings mislead.
2. **Algorithmic improvements beat micro-optimizations.** A better data structure dominates `#[inline]` and friends.
3. **Allocate thoughtfully.** Pre-allocate with `Vec::with_capacity`. Reuse buffers in tight loops. Consider `Cow<'a, T>` when most paths don't need to allocate.
4. **Profile in release.** Debug builds have extra bounds checks and disabled optimizations — their timings are misleading.

```rust
// Pre-allocate.
let mut vec = Vec::with_capacity(expected_size);

// Reuse buffers in a loop.
let mut buffer = String::new();
for item in items {
    buffer.clear();
    write_to_buffer(&mut buffer, item);
    process(&buffer);
}

// Iterator chains avoid intermediate Vec allocation.
let sum: i32 = values
    .iter()
    .filter(|x| x.is_valid())
    .map(|x| x.value())
    .sum();
```

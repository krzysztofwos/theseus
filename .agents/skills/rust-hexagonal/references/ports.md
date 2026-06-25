# Ports — detail

## Repository traits

```rust
pub trait OrderRepository: Clone + Send + Sync + 'static {
    fn save(&self, order: &Order)
        -> impl Future<Output = Result<(), SaveError>> + Send;

    fn find_by_id(&self, id: OrderId)
        -> impl Future<Output = Result<Option<Order>, FindError>> + Send;

    fn find_by_customer(&self, customer_id: CustomerId, limit: usize)
        -> impl Future<Output = Result<Vec<Order>, FindError>> + Send;
}
```

- `Clone + Send + Sync + 'static` — cheap to share across tasks, easy to inject into services and handlers. Repositories typically wrap a pool/handle and are cheap to clone (just bumps an `Arc` refcount internally).
- `impl Future<Output = …> + Send` — native async-in-traits without `#[async_trait]`.
- Errors are port-owned types (`SaveError`, `FindError`), not the adapter's underlying error.
- Method names reflect domain operations (`find_by_customer`), not storage operations (`select_orders_where_customer_id_eq`).

## External-service ports

Same shape as repositories — the difference is just what's behind them:

```rust
pub trait PaymentGateway: Clone + Send + Sync + 'static {
    fn process_payment(&self, request: &PaymentRequest)
        -> impl Future<Output = Result<PaymentResult, PaymentError>> + Send;

    fn refund(&self, transaction_id: TransactionId, amount: Money)
        -> impl Future<Output = Result<RefundResult, RefundError>> + Send;
}
```

The trait lives next to the domain; the adapter that talks to Stripe / FilecoinPay / whatever lives in `adapters/outbound/`.

## Port error design

Port errors are public API. They describe what _can_ go wrong from the caller's perspective, not what _does_ go wrong in this implementation:

```rust
#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    #[error("entity already exists with id {id}")]
    AlreadyExists { id: String },

    #[error("optimistic lock failure")]
    ConcurrentModification,

    #[error(transparent)]
    Unknown(anyhow::Error),
}
```

- Named variants for cases the caller might want to handle (`AlreadyExists` → return 409; `ConcurrentModification` → retry).
- `Unknown(anyhow::Error)` for unforeseen underlying errors — keeps the public API structured while still wrapping arbitrary sources.
- No `sqlx::Error` or `reqwest::Error` variants — those are adapter implementation details.

When a specific underlying error keeps showing up under `Unknown`, promote it to a named variant. The taxonomy evolves with what callers actually need to distinguish.

## Naming

A repository trait describes a _role_, not a _storage technology_:

- Good: `OrderRepository`, `IdentityIndex`, `SponsorQueue`.
- Bad: `PostgresOrderStore`, `SledIdentityCache`.

The concrete adapter type names the technology (`PostgresOrderRepository`, `SledIdentityIndex`). The trait does not.

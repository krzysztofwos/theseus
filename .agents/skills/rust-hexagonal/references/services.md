# Services — detail

## Service shape

```rust
#[derive(Clone)]
pub struct OrderService<R, P, N>
where
    R: OrderRepository,
    P: PaymentGateway,
    N: NotificationService,
{
    repository: R,
    payment: P,
    notifier: N,
}
```

- Generic over each port, with bounds at the type definition. The bounds reappear on the `impl` block, which is verbose but explicit.
- `Clone` so the service can be shared across axum handlers (each handler gets its own clone).
- Concrete adapters are injected at construction (`OrderService::new(repo, payment, notifier)`).

## Orchestration

```rust
impl<R, P, N> OrderService<R, P, N>
where
    R: OrderRepository,
    P: PaymentGateway,
    N: NotificationService,
{
    pub async fn place_order(
        &self,
        request: CreateOrderRequest,
    ) -> Result<Order, PlaceOrderError> {
        let validated = request.validate()?;

        let order = Order::new(
            validated.customer_id,
            validated.items,
        );

        let payment_request = self.build_payment_request(&order);
        let payment_result = self.payment
            .process_payment(&payment_request)
            .await?;

        self.repository.save(&order).await?;

        // Optional concern: log on failure, don't propagate.
        if let Err(e) = self.notifier.send_order_confirmation(&order).await {
            tracing::warn!("Failed to send confirmation: {}", e);
        }

        Ok(order)
    }
}
```

Notes:

- The service is the only place that knows the full sequence of side effects. Each adapter sees just its own slice.
- Optional side effects (notifications, analytics) fail soft and log. Core writes propagate the error.
- The service never sees `sqlx::Error` or `reqwest::Error` — they've been translated at the adapter to port-specific errors.

## Service error type

```rust
#[derive(Debug, thiserror::Error)]
pub enum PlaceOrderError {
    #[error(transparent)]
    Validation(#[from] ValidationError),

    #[error("payment failed: {reason}")]
    PaymentFailed { reason: String },

    #[error(transparent)]
    Repository(#[from] SaveError),

    #[error(transparent)]
    Unknown(anyhow::Error),
}
```

- One enum per service operation (or per service, if variants are largely shared). Use `#[from]` so port errors flow up through `?`.
- The inbound adapter maps this to a transport-level error (`ApiError`) via `From`.

## Transaction boundaries

If a service operation needs to write to multiple repositories atomically, the transaction boundary belongs to the port:

```rust
pub trait UnitOfWork: Send + Sync {
    fn execute<F, T>(&self, f: F)
        -> impl Future<Output = Result<T, UnitOfWorkError>> + Send
    where
        F: FnOnce(&mut Tx) -> ... + Send;
}
```

Don't leak `sqlx::Transaction` into the service. The adapter implements the transaction discipline; the service just describes the work.

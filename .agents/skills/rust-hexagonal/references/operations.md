# Wiring, migration, inter-domain — detail

## Application bootstrap

Wire dependencies once, at startup. One construction point, explicit graph.

```rust
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = Config::from_env()?;

    let db_pool = PgPool::connect(&config.database_url).await?;
    let order_repo = PostgresOrderRepository::new(db_pool.clone());
    let payment_gateway = StripePaymentGateway::new(&config.stripe_key);
    let notifier = EmailNotificationService::new(&config.smtp_config);

    let order_service = OrderService::new(
        order_repo,
        payment_gateway,
        notifier,
    );

    let app = Router::new()
        .route("/orders", post(create_order_handler))
        .with_state(Arc::new(order_service));

    let listener = TcpListener::bind(&config.server_address).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
```

- `main` is allowed to use `anyhow::Result` — it's binary code, and the audience for these errors is a human reading logs.
- Each service gets exactly one set of adapters at startup. No global registry, no lazy initialization.
- Config loads from environment with validation up front; the service never re-reads config at runtime.

## Identifying domain boundaries

Draw boundaries based on:

1. **Transactional consistency** — entities that must change atomically belong in the same domain.
2. **Business capabilities** — distinct business functions are distinct domains.
3. **Team ownership** — organizational boundaries are real boundaries.
4. **Change velocity** — components that evolve at different rates benefit from separation.

For SmithKit: identity, relay, package registry, sponsor queue, and the wasmi extension surface are distinct domains. They share the `crates/smithkit-node` binary but their ports and services are independent.

## Inter-domain communication

Synchronous: another domain's service exposes a port the caller depends on.

```rust
pub trait CustomerService: Clone + Send + Sync + 'static {
    fn get_customer(&self, id: CustomerId)
        -> impl Future<Output = Result<Customer, GetCustomerError>> + Send;
}
```

Asynchronous: events, typically over a queue or a stream.

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderPlacedEvent {
    pub order_id: OrderId,
    pub customer_id: CustomerId,
    pub total: Money,
    pub timestamp: DateTime<Utc>,
}
```

Events are part of the domain's public surface — name them after what happened (past tense), include enough context that the consumer doesn't have to call back for more data, and version them explicitly when the shape changes.

## Migrating an existing module

For tightly-coupled code (handler reads request, queries DB directly, sends email, returns response):

1. **Pick one bounded context.** Don't try to migrate everything at once.
2. **Extract domain models** from the existing struct soup. Make value types where there used to be `String`.
3. **Define ports.** What does this code need from the outside world? Each external dependency becomes a trait.
4. **Create a service** that orchestrates the work using those ports.
5. **Wrap the existing infrastructure** in adapters that implement the new ports. The adapters can keep the old query / HTTP-call code temporarily — what matters is that the service no longer sees it.
6. **Route traffic through the new architecture.** Initially the new path is feature-flagged or proxied; once it's been live for a while, delete the old handler.

```rust
// Before — handler does everything.
async fn create_order_old(db: &PgPool, req: HttpRequest) -> HttpResponse {
    let order = parse_order(req)?;
    sqlx::query!("INSERT INTO orders...").execute(db).await?;
    send_email(&order).await?;
    HttpResponse::Created()
}

// After — handler delegates, service orchestrates, adapter persists.
async fn create_order_new<S: OrderService>(
    State(service): State<Arc<S>>,
    Json(request): Json<CreateOrderHttpRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let domain_request = request.try_into_domain()?;
    let order = service.place_order(domain_request).await?;
    Ok((StatusCode::CREATED, Json(OrderHttpResponse::from(&order))))
}
```

## Configuration

```rust
pub struct ServiceConfig {
    pub database: DatabaseConfig,
    pub payment: PaymentConfig,
    pub notifications: NotificationConfig,
}

impl ServiceConfig {
    pub fn from_env() -> Result<Self, config::Error> {
        // Validate at load time, not on use.
    }
}
```

Config validates fully at startup. If a required value is missing or malformed, the binary fails to boot — never run with partial config and discover the gap on the first request.

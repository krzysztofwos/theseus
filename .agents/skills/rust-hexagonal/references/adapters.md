# Adapters — detail

## Outbound: implementing a port

```rust
#[derive(Clone)]
pub struct PostgresOrderRepository {
    pool: PgPool,
}

impl PostgresOrderRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    async fn save_internal(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        order: &Order,
    ) -> Result<(), sqlx::Error> {
        sqlx::query!(
            "INSERT INTO orders (id, customer_id, status, created_at)
             VALUES ($1, $2, $3, $4)",
            order.id().as_uuid(),
            order.customer_id().as_uuid(),
            order.status().as_str(),
            order.created_at(),
        )
        .execute(&mut **tx)
        .await?;

        for item in order.items() {
            self.save_order_item(tx, order.id(), item).await?;
        }

        Ok(())
    }
}

impl OrderRepository for PostgresOrderRepository {
    async fn save(&self, order: &Order) -> Result<(), SaveError> {
        let mut tx = self.pool
            .begin()
            .await
            .map_err(|e| SaveError::Unknown(anyhow::Error::from(e)))?;

        self.save_internal(&mut tx, order)
            .await
            .map_err(|e| {
                if is_unique_violation(&e) {
                    SaveError::AlreadyExists {
                        id: order.id().to_string(),
                    }
                } else {
                    SaveError::Unknown(anyhow::Error::from(e))
                }
            })?;

        tx.commit()
            .await
            .map_err(|e| SaveError::Unknown(anyhow::Error::from(e)))
    }

    // Other trait methods...
}
```

Notes:

- The adapter is the only module that imports `sqlx`. Domain code stays clean.
- Error translation happens here, once. The semantically meaningful case (unique violation → `AlreadyExists`) gets a real variant; the rest fall through to `Unknown`.
- Private helpers (`save_internal`, `save_order_item`) return the underlying error type. The public `impl OrderRepository` is the only place that translates.

## Inbound: HTTP handler

```rust
pub async fn create_order_handler<S: OrderService>(
    State(service): State<Arc<S>>,
    Json(request): Json<CreateOrderHttpRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let domain_request = request.try_into_domain()?;

    let order = service
        .place_order(domain_request)
        .await
        .map_err(ApiError::from)?;

    let response = OrderHttpResponse::from(&order);

    Ok((StatusCode::CREATED, Json(response)))
}
```

The handler does three things, in order:

1. Convert wire format (`CreateOrderHttpRequest`) → domain request via `try_into_domain()`.
2. Call the service.
3. Convert the domain response → wire format (`OrderHttpResponse::from(&order)`).

If any of those steps would belong to the service ("validate items", "calculate total"), they don't go in the handler.

## Wire-format types

`CreateOrderHttpRequest` and `OrderHttpResponse` live in the inbound adapter, not in the domain. Their fields use `snake_case` (per CLAUDE.md wire-format rule for SmithKit), and their `try_into_domain` / `From<&Order>` conversions are the seam where casing translation happens. Don't leak `serde` attributes onto domain types.

## Error mapping

```rust
impl From<PlaceOrderError> for ApiError {
    fn from(err: PlaceOrderError) -> Self {
        match err {
            PlaceOrderError::Validation(e) => ApiError::BadRequest(e.to_string()),
            PlaceOrderError::PaymentFailed { reason } => ApiError::PaymentRequired(reason),
            PlaceOrderError::Repository(SaveError::AlreadyExists { id }) => {
                ApiError::Conflict(id)
            }
            PlaceOrderError::Repository(_) | PlaceOrderError::Unknown(_) => {
                ApiError::InternalServer
            }
        }
    }
}
```

The match is exhaustive on purpose — adding a new service-error variant forces a decision about the HTTP mapping. Don't fall through to a catch-all that silently maps everything to 500.

# Testing — detail

## In-memory adapters as test doubles

Write an `InMemoryOrderRepository` that implements the same port as `PostgresOrderRepository`. The service is generic over the port, so swapping in the in-memory implementation in tests is trivial.

```rust
#[cfg(test)]
mod test_doubles {
    use super::*;

    #[derive(Clone)]
    pub struct InMemoryOrderRepository {
        orders: Arc<RwLock<HashMap<OrderId, Order>>>,
    }

    impl InMemoryOrderRepository {
        pub fn new() -> Self {
            Self {
                orders: Arc::new(RwLock::new(HashMap::new())),
            }
        }
    }

    impl OrderRepository for InMemoryOrderRepository {
        async fn save(&self, order: &Order) -> Result<(), SaveError> {
            let mut orders = self.orders.write().await;
            if orders.contains_key(order.id()) {
                return Err(SaveError::AlreadyExists {
                    id: order.id().to_string(),
                });
            }
            orders.insert(order.id().clone(), order.clone());
            Ok(())
        }

        // Other trait methods...
    }
}
```

Why this matters in this repo (per CLAUDE.md Hard Rule 1): no mocks in production code. `#[cfg(test)]` ensures the in-memory adapter is never compiled into a release binary — it's a real implementation of the port, just one whose backing store is a `HashMap` instead of Postgres. That's structurally different from a mock and doesn't tempt anyone to ship it.

## Service-level tests

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_place_order_success() {
        let repository = InMemoryOrderRepository::new();
        let payment = StubPaymentGateway::new()
            .with_response(PaymentResult::Success);
        let notifier = StubNotificationService::new();

        let service = OrderService::new(repository, payment, notifier);

        let request = CreateOrderRequest::new(
            CustomerId::generate(),
            vec![/* items */],
        );

        let result = service.place_order(request).await;

        assert!(result.is_ok());
        let order = result.unwrap();
        assert_eq!(order.status(), OrderStatus::Confirmed);
    }
}
```

- `Stub*` types are the test-double versions of payment/notification ports. Same shape as `InMemoryOrderRepository`: a real `impl` of the port, with controllable behaviour.
- Each test constructs its own service. No global state, no shared fixtures unless they're truly read-only.

## What not to mock

- Don't mock the runtime (tokio). Use `tokio::test`.
- Don't mock structured logging. Let it run; assert on output only if behaviour depends on it.
- Don't mock pure functions (parsers, validators). Call them.
- For SmithKit demos and dev builds (per CLAUDE.md Hard Rule 1), the real-services rule applies even outside tests — the in-memory adapter is for `#[cfg(test)]`, not for "demo mode."

## Integration tests against real services

Per CLAUDE.md, dev and demo paths hit real services (real Curio SP on Calibration, real `FilecoinPay.deposit`, real WebAuthn PRF). Integration tests that touch those should live under `tests/` and gate on environment (e.g. require `SMITHKIT_SPONSOR_PRIV_KEY` to be set). The hexagonal split makes this easy: the test wires the service against the real outbound adapter rather than the in-memory one.

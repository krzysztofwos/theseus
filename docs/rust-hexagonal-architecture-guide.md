# Rust Hexagonal Architecture Guide

This guide provides patterns and practices for implementing hexagonal architecture in production Rust applications. It complements our Rust Style Guide by focusing on architectural boundaries, domain modeling, and dependency management.

## 1. Core Architecture Principles

### Fundamental Rules

**Domains own their types and boundaries:**

- Domain models never leak implementation details
- All validation happens at domain boundaries
- External types never flow through domain logic

**Dependencies flow inward:**

- Adapters depend on ports
- Ports are owned by the domain
- Domain logic depends on nothing external

**Every external dependency needs an abstraction:**

- Databases, message queues, HTTP clients require trait boundaries
- Exceptions: runtime dependencies (tokio), ubiquitous utilities (anyhow)
- Third-party types never cross module boundaries

### Architectural Layers

```rust
// Domain layer - zero external dependencies
pub mod domain {
    pub mod models;    // Domain entities and value objects
    pub mod ports;     // Trait definitions for external services
    pub mod services;  // Business logic orchestration
}

// Adapter layer - implements domain ports
pub mod adapters {
    pub mod inbound {  // HTTP handlers, CLI, message consumers
        pub mod http;
        pub mod grpc;
    }
    pub mod outbound { // Database, external APIs, message producers
        pub mod postgres;
        pub mod redis;
    }
}
```

## 2. Domain Modeling

### Entity Design

Domain entities encapsulate business invariants and guarantee validity:

```rust
// Good: Entity with enforced invariants
#[derive(Debug, Clone)]
pub struct Order {
    id: OrderId,
    customer_id: CustomerId,
    items: NonEmpty<OrderItem>,
    status: OrderStatus,
    created_at: DateTime<Utc>,
}

impl Order {
    pub fn new(customer_id: CustomerId, items: NonEmpty<OrderItem>) -> Self {
        Self {
            id: OrderId::generate(),
            customer_id,
            items,
            status: OrderStatus::Pending,
            created_at: Utc::now(),
        }
    }

    pub fn add_item(&mut self, item: OrderItem) -> Result<(), OrderError> {
        if self.status != OrderStatus::Pending {
            return Err(OrderError::InvalidStatus);
        }
        self.items.push(item);
        Ok(())
    }
}
```

### Value Objects

Wrap primitives to enforce domain constraints:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomerId(Uuid);

impl CustomerId {
    pub fn parse(s: &str) -> Result<Self, ValidationError> {
        let uuid = Uuid::parse_str(s)
            .map_err(|_| ValidationError::InvalidFormat)?;
        Ok(CustomerId(uuid))
    }

    pub fn generate() -> Self {
        CustomerId(Uuid::new_v4())
    }
}
```

### Request/Response Models

Separate creation requests from entities:

```rust
// Request model for entity creation
#[derive(Debug, Clone)]
pub struct CreateOrderRequest {
    customer_id: CustomerId,
    items: Vec<OrderItemRequest>,
}

impl CreateOrderRequest {
    pub fn validate(self) -> Result<ValidatedCreateOrderRequest, ValidationError> {
        let items = NonEmpty::from_vec(self.items)
            .ok_or(ValidationError::EmptyOrder)?;
        Ok(ValidatedCreateOrderRequest {
            customer_id: self.customer_id,
            items,
        })
    }
}
```

## 3. Port Definitions

### Repository Traits

Define repository contracts at the domain boundary:

```rust
pub trait OrderRepository: Clone + Send + Sync + 'static {
    fn save(
        &self,
        order: &Order,
    ) -> impl Future<Output = Result<(), SaveError>> + Send;

    fn find_by_id(
        &self,
        id: OrderId,
    ) -> impl Future<Output = Result<Option<Order>, FindError>> + Send;

    fn find_by_customer(
        &self,
        customer_id: CustomerId,
        limit: usize,
    ) -> impl Future<Output = Result<Vec<Order>, FindError>> + Send;
}
```

### External Service Ports

Abstract external dependencies behind traits:

```rust
pub trait PaymentGateway: Clone + Send + Sync + 'static {
    fn process_payment(
        &self,
        request: &PaymentRequest,
    ) -> impl Future<Output = Result<PaymentResult, PaymentError>> + Send;

    fn refund(
        &self,
        transaction_id: TransactionId,
        amount: Money,
    ) -> impl Future<Output = Result<RefundResult, RefundError>> + Send;
}
```

### Error Design for Ports

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

## 4. Service Layer

### Service Implementation

Services orchestrate domain logic and coordinate between ports:

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
        // Validate request
        let validated = request.validate()?;

        // Create domain entity
        let order = Order::new(
            validated.customer_id,
            validated.items,
        );

        // Process payment
        let payment_request = self.build_payment_request(&order);
        let payment_result = self.payment
            .process_payment(&payment_request)
            .await?;

        // Save order
        self.repository.save(&order).await?;

        // Send notifications (failure non-critical)
        if let Err(e) = self.notifier.send_order_confirmation(&order).await {
            tracing::warn!("Failed to send confirmation: {}", e);
        }

        Ok(order)
    }
}
```

### Service Error Handling

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

## 5. Adapter Implementation

### Outbound Adapters

Implement domain ports with concrete technologies:

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

        // Save order items
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

### Inbound Adapters

Handle external requests and delegate to services:

```rust
pub async fn create_order_handler<S: OrderService>(
    State(service): State<Arc<S>>,
    Json(request): Json<CreateOrderHttpRequest>,
) -> Result<impl IntoResponse, ApiError> {
    // Convert HTTP request to domain request
    let domain_request = request.try_into_domain()?;

    // Call service
    let order = service
        .place_order(domain_request)
        .await
        .map_err(ApiError::from)?;

    // Convert domain response to HTTP response
    let response = OrderHttpResponse::from(&order);

    Ok((StatusCode::CREATED, Json(response)))
}
```

## 6. Testing Strategies

### Mock Implementations

Create test doubles for ports:

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

### Service Testing

Test business logic in isolation:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_place_order_success() {
        // Arrange
        let repository = InMemoryOrderRepository::new();
        let payment = MockPaymentGateway::new()
            .with_response(PaymentResult::Success);
        let notifier = MockNotificationService::new();

        let service = OrderService::new(repository, payment, notifier);
        let request = CreateOrderRequest::new(
            CustomerId::generate(),
            vec![/* items */],
        );

        // Act
        let result = service.place_order(request).await;

        // Assert
        assert!(result.is_ok());
        let order = result.unwrap();
        assert_eq!(order.status(), OrderStatus::Confirmed);
    }
}
```

## 7. Dependency Injection

### Application Bootstrap

Wire dependencies in main:

```rust
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = Config::from_env()?;

    // Initialize adapters
    let db_pool = PgPool::connect(&config.database_url).await?;
    let order_repo = PostgresOrderRepository::new(db_pool.clone());
    let payment_gateway = StripePaymentGateway::new(&config.stripe_key);
    let notifier = EmailNotificationService::new(&config.smtp_config);

    // Create services
    let order_service = OrderService::new(
        order_repo,
        payment_gateway,
        notifier,
    );

    // Start server
    let app = Router::new()
        .route("/orders", post(create_order_handler))
        .with_state(Arc::new(order_service));

    let listener = TcpListener::bind(&config.server_address).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
```

## 8. Domain Boundaries

### Identifying Boundaries

Draw boundaries based on:

1. **Transactional consistency** - Entities that must change atomically
2. **Business capabilities** - Distinct business functions
3. **Team ownership** - Clear organizational boundaries
4. **Change velocity** - Components that evolve at different rates

### Inter-Domain Communication

```rust
// Synchronous communication via service traits
pub trait CustomerService: Clone + Send + Sync + 'static {
    fn get_customer(
        &self,
        id: CustomerId,
    ) -> impl Future<Output = Result<Customer, GetCustomerError>> + Send;
}

// Asynchronous communication via events
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderPlacedEvent {
    pub order_id: OrderId,
    pub customer_id: CustomerId,
    pub total: Money,
    pub timestamp: DateTime<Utc>,
}
```

## 9. Migration Patterns

### Gradual Hexagonal Migration

For existing codebases:

1. **Identify a bounded context** - Start with a single domain
2. **Extract domain models** - Create pure domain types
3. **Define ports** - Abstract external dependencies
4. **Create service layer** - Orchestrate domain logic
5. **Implement adapters** - Wrap existing infrastructure
6. **Migrate incrementally** - Route traffic through new architecture

### Refactoring Example

```rust
// Before: Tightly coupled handler
async fn create_order_old(db: &PgPool, req: HttpRequest) -> HttpResponse {
    let order = parse_order(req)?;
    sqlx::query!("INSERT INTO orders...").execute(db).await?;
    send_email(&order).await?;
    HttpResponse::Created()
}

// After: Hexagonal architecture
async fn create_order_new<S: OrderService>(
    State(service): State<Arc<S>>,
    Json(request): Json<CreateOrderHttpRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let domain_request = request.try_into_domain()?;
    let order = service.place_order(domain_request).await?;
    Ok((StatusCode::CREATED, Json(OrderHttpResponse::from(&order))))
}
```

## 10. Common Patterns

### Result Transformation

Convert between layer-specific error types:

```rust
impl From<ServiceError> for ApiError {
    fn from(err: ServiceError) -> Self {
        match err {
            ServiceError::NotFound { id } => ApiError::NotFound(id),
            ServiceError::InvalidRequest(msg) => ApiError::BadRequest(msg),
            ServiceError::Unknown(_) => ApiError::InternalServer,
        }
    }
}
```

### Configuration Management

```rust
pub struct ServiceConfig {
    pub database: DatabaseConfig,
    pub payment: PaymentConfig,
    pub notifications: NotificationConfig,
}

impl ServiceConfig {
    pub fn from_env() -> Result<Self, config::Error> {
        // Load from environment with validation
    }
}
```

## Summary

This guide emphasizes:

- **Clear boundaries** between domain logic and infrastructure
- **Type safety** through domain modeling and trait definitions
- **Testability** via dependency injection and mocking
- **Flexibility** through ports and adapters pattern
- **Maintainability** via consistent project structure

Remember: Hexagonal architecture trades upfront complexity for long-term maintainability. Use it when your application complexity and team size justify the investment.

# Domain modeling — detail

## Entity design

Entities encapsulate business invariants. They are constructed valid; no operation can leave them in an invalid state.

```rust
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

- Fields are private; mutation goes through methods that enforce invariants.
- Use `NonEmpty<T>` (or similar) when emptiness is invalid by definition — let the type system enforce it.
- Time is injected (`created_at`) or generated at construction; never read mid-method.

## Value objects

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

See the `rust-style` skill's `references/types.md` for the full newtype pattern.

## Request / response models

Separate creation requests from entities. A request is data from the outside world; an entity is data that has been validated and accepted into the domain.

```rust
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

The `Validated*` form is what the service accepts. It is structurally impossible to construct except through `validate()` — anyone calling the service has proof that validation has run.

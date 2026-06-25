---
name: rust-hexagonal
description: Apply hexagonal architecture for Rust domain code in this repo — the domain layer owns its types and ports (traits), adapters implement those ports, services orchestrate. Use when adding a new domain module, defining a repository or external-service trait, writing an outbound adapter (Postgres, sled, HTTP, Synapse, alloy), wiring an inbound HTTP handler (axum) to a service, designing error types that cross trust boundaries, or refactoring tightly-coupled code (handler ↔ DB ↔ external call) into ports and adapters. Reach for this skill whenever the change touches the shape of a module boundary, not just its internals.
---

# Rust Hexagonal Architecture

How to structure domain code so business logic is independent of databases, queues, and HTTP transports.

The long-form guide is at `docs/rust-hexagonal-architecture-guide.md`; the companion style guide is at `docs/rust-style-guide.md` (and its skill is `rust-style`). This skill is the operational summary with pointers to detailed examples.

## Three layers

```
domain/        // entities, value objects, ports (traits), services (orchestration)
adapters/
  inbound/     // axum handlers, CLI, sidecar consumers — call services
  outbound/    // Postgres, sled, HTTP, Synapse, alloy — implement domain ports
```

**Rules:**

1. The domain depends on nothing external. No `sqlx::PgPool` in a domain function signature; no `reqwest::Client`; no `axum::Json`.
2. Ports (traits) are owned by the domain.
3. Adapters depend on ports — never the other way around.
4. Services orchestrate domain logic and call ports.
5. Inbound adapters convert wire format → domain request, call a service, convert domain response → wire format.

## Domain modeling

Entities encapsulate invariants. They are constructed valid and cannot be mutated into invalid states:

```rust
#[derive(Debug, Clone)]
pub struct Order {
    id: OrderId,
    customer_id: CustomerId,
    items: NonEmpty<OrderItem>,
    status: OrderStatus,
    created_at: DateTime<Utc>,
}
```

Value objects (`CustomerId`, `OrderId`) wrap primitives with parse-don't-validate constructors — see the `rust-style` skill, `references/types.md`.

Separate creation requests from entities (`CreateOrderRequest::validate() -> ValidatedCreateOrderRequest`). The validated form is what services accept.

Full domain examples: [references/domain.md](references/domain.md).

## Ports

Standard trait shape for a port:

```rust
pub trait OrderRepository: Clone + Send + Sync + 'static {
    fn save(&self, order: &Order)
        -> impl Future<Output = Result<(), SaveError>> + Send;

    fn find_by_id(&self, id: OrderId)
        -> impl Future<Output = Result<Option<Order>, FindError>> + Send;
}
```

- Native `impl Future<…> + Send`, not `#[async_trait]`.
- Each method returns a port-specific structured error (`SaveError`, `FindError`), not `anyhow::Error`. A `Unknown(anyhow::Error)` variant is the acceptable catch-all.
- Bounds `Clone + Send + Sync + 'static` make ports shareable across tasks and easy to inject.

Detail on port error design and external-service ports: [references/ports.md](references/ports.md).

## Services

Services are generic over their ports — that's the seam that makes them testable:

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

The service is where business invariants are enforced and side effects sequenced. Optional concerns (notifications) fail soft and log; core writes don't.

Full service implementation and error shape: [references/services.md](references/services.md).

## Adapters

**Outbound** — implement a port with a concrete technology (`PostgresOrderRepository`, `SynapseStorageAdapter`, `AlloyUserOpRelay`). The adapter translates technology errors (`sqlx::Error`, `reqwest::Error`) into the port's structured error, distinguishing semantically meaningful cases (unique-violation → `AlreadyExists`) from the catch-all `Unknown(anyhow::Error)`.

**Inbound** — convert HTTP/JSON → domain request, delegate to the service, convert domain response → HTTP. Errors map via `impl From<ServiceError> for ApiError`.

Full Postgres + axum examples: [references/adapters.md](references/adapters.md).

## Testing

Write an in-memory adapter that implements the port — that's your test double. Then exercise the service against it without a mocking framework.

Per CLAUDE.md Hard Rule 1: production code uses real services, no mocks. But `#[cfg(test)] mod test_doubles { … }` next to the port is exactly the right scope — it never ships in the release binary.

Pattern: [references/testing.md](references/testing.md).

## Wiring and migration

Bootstrap composes concrete adapters and services in `main` (or in the node's startup module). One construction point, explicit dependencies. For existing tightly-coupled code, the migration is incremental: extract domain models first, define ports, wrap existing infrastructure in adapters, then route traffic through the new service.

Detail: [references/operations.md](references/operations.md).

## Key rules

1. Domain modules never import `sqlx`, `reqwest`, `axum`, `alloy`, `sled`, etc. If they do, you have a port leak.
2. Ports return structured errors, never `anyhow::Error` directly (the `Unknown(anyhow::Error)` variant is fine as the catch-all).
3. Third-party types do not cross domain boundaries. Translate at the adapter.
4. New external dependencies need a port. Exceptions: the tokio runtime, ubiquitous utilities.
5. SmithKit-specific (per CLAUDE.md): the sealed-surface invariants for `@smithkit/core` and the identity flows depend on this split. Priv material must never leak past the portal boundary; the hexagonal layout is how that boundary is enforced in code rather than convention.

If anything in this skill conflicts with `docs/rust-hexagonal-architecture-guide.md`, treat the source doc as authoritative and update both.

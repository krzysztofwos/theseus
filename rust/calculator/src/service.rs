//! The authored calculator: the adapter implementing the generated contract.
//!
//! [`Calculator`] is a stateless adapter. An operation without a handler here falls
//! through to the trait's `unimplemented` default, and Theseus's coverage check
//! reports it. The structured-edit tooling writes the handlers into this file.

use crate::generated::CalculatorService;

/// The calculator adapter.
pub struct Calculator;

#[async_trait::async_trait]
impl CalculatorService for Calculator {
    async fn divide(&self, request: crate::generated::Operands) -> anyhow::Result<String> {
        Ok(format!("{}", request.a / request.b))
    }

    async fn multiply(&self, request: crate::generated::Operands) -> anyhow::Result<String> {
        Ok(format!("{}", request.a * request.b))
    }

    async fn subtract(&self, request: crate::generated::Operands) -> anyhow::Result<String> {
        Ok(format!("{}", request.a - request.b))
    }

    async fn add(&self, request: crate::generated::Operands) -> anyhow::Result<String> {
        Ok(format!("{}", request.a + request.b))
    }
}

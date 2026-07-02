//! A calculator service: arithmetic over a pair of operands (L1).
//!
//! The [`generated`] module holds the model-rendered contract — the
//! [`CalculatorService`] trait and its [`Operands`] request. [`Calculator`] is the
//! authored adapter that implements the contract. Theseus exposes this service
//! through its CLI by calling the contract across an in-process port.

mod generated;
mod service;

pub use generated::{CalculatorService, Operands, Unimplemented};
pub use service::Calculator;

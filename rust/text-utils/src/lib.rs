//! The TextUtils service

mod generated;
mod service;

pub use generated::{
    CapitalizeRequest, SlugifyRequest, TextUtilsService, TruncateRequest, WordCountRequest,
};
pub use service::TextUtils;

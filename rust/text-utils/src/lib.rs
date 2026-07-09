//! The TextUtils service

mod generated;
mod service;

pub use generated::{
    CapitalizeRequest, Refused, SlugifyRequest, TextUtilsService, TruncateRequest, Unimplemented,
    WordCountRequest,
};
pub use service::TextUtils;

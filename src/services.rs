//! Business logic: what a request means, independent of HTTP shaping.

pub mod admission;
pub(crate) mod capture_request;
pub mod openai;
pub mod request_policy;
pub mod request_quota;
pub mod sse;
pub mod streaming;
pub mod task_decomposition;

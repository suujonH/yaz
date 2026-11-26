//! AI model access for the model center: providers (interface type, EndPoint, API Key, models)
//! and the client that sends requests in the format agreed by `zulip_plugin_sdk::model_center`
//! (synchronous, call on a worker thread).

pub mod client;
pub mod provider;

pub use client::{ModelError, handle, list_models, test_request};
pub use provider::{ALL_TYPES, DECISIONS, Provider, resource_of};

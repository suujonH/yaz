//! Zulip REST 客户端与数据结构。不依赖界面；所有请求都是异步的。

mod client;
mod config;
pub mod dto;
mod endpoints;
mod error;
mod form;
mod narrow;
mod trace;
mod transfer;

pub use client::{ClientBuilder, ZulipClient};
pub use config::{CLIENT_NAME, ClientConfig, DESKTOP_CLIENT_NAME, WEB_CLIENT_NAME, user_agent_for};
pub use dto::*;
pub use error::ApiError;
pub use form::Form;
pub use narrow::{NarrowTerm, dm_narrow, topic_narrow};
pub use reqwest::Method;
pub use tokio_util::sync::CancellationToken;
pub use trace::{LogEntry, RequestLog, TraceRequest, TraceResponse, TraceSink};

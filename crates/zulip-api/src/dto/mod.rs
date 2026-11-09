//! Data structures of the Zulip API. Field names match the server (snake case); missing fields take
//! defaults.

mod message;
mod misc;
mod register;
mod stream;
mod user;

use std::collections::HashMap;

use serde::{Deserialize, Deserializer};

pub use message::*;
pub use misc::*;
pub use register::*;
pub use stream::*;
pub use user::*;

/// Non-optional fields the server may return as `null`; `null` takes the default.
pub(crate) fn null_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

/// JSON object keys are strings; this restores integer keys such as user ids back to integers.
pub(crate) fn int_key_map<'de, D, T>(deserializer: D) -> Result<HashMap<i64, T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    let raw = Option::<HashMap<String, T>>::deserialize(deserializer)?.unwrap_or_default();
    raw.into_iter()
        .map(|(key, value)| {
            key.parse::<i64>()
                .map(|id| (id, value))
                .map_err(|_| serde::de::Error::custom(format!("无效的 id：{key}")))
        })
        .collect()
}

/// Integer fields that the server occasionally returns as numbers with a fractional part.
pub(crate) fn lenient_i64<'de, D: Deserializer<'de>>(deserializer: D) -> Result<i64, D::Error> {
    let value = serde_json::Number::deserialize(deserializer)?;
    value
        .as_i64()
        .or_else(|| value.as_f64().map(|f| f as i64))
        .ok_or_else(|| serde::de::Error::custom("数字超出范围"))
}

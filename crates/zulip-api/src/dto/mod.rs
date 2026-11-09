//! Zulip 接口的数据结构。字段名与服务器一致（蛇形小写），缺失的字段取默认值。

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

/// 服务器可能返回 `null` 的非可选字段，`null` 时取默认值。
pub(crate) fn null_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

/// JSON 对象的键是字符串，这里把用户 id 之类的整数键还原为整数。
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

/// 整数字段，服务器偶尔以带小数的数字返回。
pub(crate) fn lenient_i64<'de, D: Deserializer<'de>>(deserializer: D) -> Result<i64, D::Error> {
    let value = serde_json::Number::deserialize(deserializer)?;
    value
        .as_i64()
        .or_else(|| value.as_f64().map(|f| f as i64))
        .ok_or_else(|| serde::de::Error::custom("数字超出范围"))
}

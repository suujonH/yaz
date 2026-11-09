use serde::Serialize;
use serde_json::{Value, json};

/// narrow 的一项，序列化为 `{"operator":…,"operand":…,"negated":…}`。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NarrowTerm {
    pub operator: String,
    pub operand: Value,
    pub negated: bool,
}

impl NarrowTerm {
    fn new(operator: &str, operand: Value) -> Self {
        Self {
            operator: operator.to_owned(),
            operand,
            negated: false,
        }
    }

    pub fn channel(id: i64) -> Self {
        Self::new("channel", json!(id))
    }

    pub fn topic(topic: &str) -> Self {
        Self::new("topic", json!(topic))
    }

    pub fn dm(user_ids: &[i64]) -> Self {
        Self::new("dm", json!(user_ids))
    }

    /// 如 `starred`、`unread`、`mentioned`。
    pub fn is(value: &str) -> Self {
        Self::new("is", json!(value))
    }

    /// 如 `attachment`、`image`、`link`。
    pub fn has(value: &str, negated: bool) -> Self {
        Self {
            negated,
            ..Self::new("has", json!(value))
        }
    }

    pub fn search(text: &str) -> Self {
        Self::new("search", json!(text))
    }

    pub fn sender(user_id: i64) -> Self {
        Self::new("sender", json!(user_id))
    }
}

/// 话题会话的 narrow。`topic` 使用原始大小写。
pub fn topic_narrow(stream_id: i64, topic: &str) -> Vec<NarrowTerm> {
    vec![NarrowTerm::channel(stream_id), NarrowTerm::topic(topic)]
}

/// 私聊会话的 narrow。`other_user_ids` 不含自己；与自己的私聊（为空）为 `dm:[自己]`。
pub fn dm_narrow(other_user_ids: &[i64], self_id: i64) -> Vec<NarrowTerm> {
    if other_user_ids.is_empty() {
        vec![NarrowTerm::dm(&[self_id])]
    } else {
        vec![NarrowTerm::dm(other_user_ids)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn to_json(terms: &[NarrowTerm]) -> String {
        serde_json::to_string(terms).unwrap()
    }

    #[test]
    fn topic_serializes() {
        assert_eq!(
            to_json(&topic_narrow(4, "构建失败")),
            r#"[{"operator":"channel","operand":4,"negated":false},{"operator":"topic","operand":"构建失败","negated":false}]"#
        );
    }

    #[test]
    fn topic_keeps_original_case() {
        assert!(to_json(&topic_narrow(4, "Hello World")).contains("\"Hello World\""));
    }

    #[test]
    fn dm_and_self_dm() {
        assert_eq!(
            to_json(&dm_narrow(&[9, 10], 8)),
            r#"[{"operator":"dm","operand":[9,10],"negated":false}]"#
        );
        assert_eq!(
            to_json(&dm_narrow(&[], 8)),
            r#"[{"operator":"dm","operand":[8],"negated":false}]"#
        );
    }

    #[test]
    fn negated_terms() {
        assert_eq!(
            to_json(&[NarrowTerm::has("image", true), NarrowTerm::search("a b")]),
            r#"[{"operator":"has","operand":"image","negated":true},{"operator":"search","operand":"a b","negated":false}]"#
        );
    }
}

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use serde::Serialize;

const ENCODE: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~');

/// Request parameters. For GET they are appended to the query string; other methods send
/// `application/x-www-form-urlencoded`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Form {
    pairs: Vec<(String, String)>,
}

impl Form {
    pub fn new() -> Self {
        Self::default()
    }

    /// Booleans are written as `true` / `false`.
    pub fn add(mut self, name: &str, value: impl ToString) -> Self {
        self.pairs.push((name.to_owned(), value.to_string()));
        self
    }

    /// The value is serialized to JSON and written (arrays, objects, etc.).
    pub fn add_json<T: Serialize + ?Sized>(self, name: &str, value: &T) -> Self {
        let json = serde_json::to_string(value).expect("表单参数必须可序列化为 JSON");
        self.add(name, json)
    }

    pub fn pairs(&self) -> &[(String, String)] {
        &self.pairs
    }

    pub fn is_empty(&self) -> bool {
        self.pairs.is_empty()
    }

    pub fn to_query_string(&self) -> String {
        let mut out = String::new();
        for (name, value) in &self.pairs {
            if !out.is_empty() {
                out.push('&');
            }
            out.extend(utf8_percent_encode(name, ENCODE));
            out.push('=');
            out.extend(utf8_percent_encode(value, ENCODE));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_values() {
        let form = Form::new()
            .add("topic", "开发 & 测试")
            .add("num", 42)
            .add("flag", true)
            .add_json("to", &[1, 2]);
        assert_eq!(
            form.to_query_string(),
            "topic=%E5%BC%80%E5%8F%91%20%26%20%E6%B5%8B%E8%AF%95&num=42&flag=true&to=%5B1%2C2%5D"
        );
        assert_eq!(form.pairs().len(), 4);
    }

    #[test]
    fn keeps_unreserved_characters() {
        let form = Form::new().add("k", "a-b_c.d~e");
        assert_eq!(form.to_query_string(), "k=a-b_c.d~e");
    }
}

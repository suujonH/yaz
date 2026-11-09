//! UTF-8 字节偏移与 UTF-16 偏移（WebView 中的 `selectionStart` 等）的换算。

/// UTF-16 偏移转为字节偏移；落在代理对中间时取该字符的起点，超出末尾时为文字长度。
pub fn from_utf16(text: &str, utf16_offset: usize) -> usize {
    let mut units = 0;
    for (i, c) in text.char_indices() {
        units += c.len_utf16();
        if units > utf16_offset {
            return i;
        }
    }
    text.len()
}

/// 字节偏移转为 UTF-16 偏移。
pub fn to_utf16(text: &str, byte_offset: usize) -> usize {
    text[..text.floor_char_boundary(byte_offset)]
        .encode_utf16()
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let text = "a中😄b";
        for (utf16, byte) in [(0, 0), (1, 1), (2, 4), (4, 8), (5, 9)] {
            assert_eq!(from_utf16(text, utf16), byte);
            assert_eq!(to_utf16(text, byte), utf16);
        }
        assert_eq!(from_utf16(text, 3), 4);
        assert_eq!(from_utf16(text, 99), 9);
        assert_eq!(to_utf16(text, 99), 5);
    }
}

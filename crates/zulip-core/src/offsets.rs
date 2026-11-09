//! Conversion between UTF-8 byte offsets and UTF-16 offsets (`selectionStart` etc. in the WebView).

/// Converts a UTF-16 offset to a byte offset; inside a surrogate pair it takes the start of that character, and past the end it is the text length.
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

/// Converts a byte offset to a UTF-16 offset.
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

//! Lowercase hex encoding for digest output.

const HEX_DIGITS: [u8; 16] = *b"0123456789abcdef";

/// Encode bytes as a lowercase hex string, two characters per byte.
pub fn encode_lowercase(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX_DIGITS[(byte >> 4) as usize] as char);
        out.push(HEX_DIGITS[(byte & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_lowercase() {
        assert_eq!(encode_lowercase(&[]), "");
        assert_eq!(encode_lowercase(&[0x00, 0x0f, 0xa5, 0xff]), "000fa5ff");
    }

    #[test]
    fn test_encode_lowercase_covers_every_byte() {
        let all: Vec<u8> = (0..=255).collect();
        let encoded = encode_lowercase(&all);
        assert_eq!(encoded.len(), 512);
        assert_eq!(&encoded[0..4], "0001");
        assert_eq!(&encoded[508..512], "feff");
    }
}

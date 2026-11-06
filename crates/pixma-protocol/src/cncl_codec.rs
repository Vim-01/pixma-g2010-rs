//! Canon CNCL cipher encoder and decoder.
//!
//! Canon PPD capability strings and configuration tokens use a lightweight
//! obfuscation cipher based on bitwise XOR `0x39`, nibble encoding to `'a'..'p'`,
//! an `'='` separator, and an 8-character big-endian 32-bit byte sum checksum.

use crate::errors::ProtocolError;

const XOR_KEY: u8 = 0x39;

/// Encodes arbitrary raw bytes into a Canon CNCL obfuscated string.
///
/// Each input byte $b$ is XORed with `0x39` and expanded into two ASCII characters
/// in the range `'a'..='p'` (representing high and low nibbles). The payload is
/// terminated with an `'='` delimiter, followed by an 8-character representation
/// of the 32-bit unsigned byte sum (big-endian, without XOR).
pub fn encode_cncl_string(raw: &[u8]) -> String {
    let mut out = String::with_capacity(raw.len() * 2 + 1 + 8);
    let mut checksum: u32 = 0;

    for &b in raw {
        checksum = checksum.wrapping_add(b as u32);
        let x = b ^ XOR_KEY;
        let hi = b'a' + ((x >> 4) & 0x0F);
        let lo = b'a' + (x & 0x0F);
        out.push(hi as char);
        out.push(lo as char);
    }

    out.push('=');

    for &s in &checksum.to_be_bytes() {
        let hi = b'a' + ((s >> 4) & 0x0F);
        let lo = b'a' + (s & 0x0F);
        out.push(hi as char);
        out.push(lo as char);
    }

    out
}

/// Decodes a Canon CNCL obfuscated string back into raw bytes, verifying the checksum.
///
/// Returns `Err(ProtocolError)` if:
/// - The `'='` delimiter is missing.
/// - The checksum section is not exactly 8 characters.
/// - The data section does not contain an even number of characters.
/// - Any character outside `'a'..='p'` is encountered.
/// - The decoded 32-bit unsigned byte sum does not match the checksum.
pub fn decode_cncl_string(encoded: &str) -> Result<Vec<u8>, ProtocolError> {
    let (data_part, csum_part) = encoded
        .split_once('=')
        .ok_or_else(|| ProtocolError::CnclInvalidFormat("Missing '=' delimiter".into()))?;

    if csum_part.len() != 8 {
        return Err(ProtocolError::CnclInvalidFormat(format!(
            "Checksum must be exactly 8 characters, got {}",
            csum_part.len()
        )));
    }

    if data_part.len() % 2 != 0 {
        return Err(ProtocolError::CnclInvalidFormat(format!(
            "Data length must be even, got {}",
            data_part.len()
        )));
    }

    // Parse 8-character checksum into 4 big-endian bytes
    let mut csum_bytes = [0u8; 4];
    let csum_raw = csum_part.as_bytes();
    for i in 0..4 {
        let hi = decode_nibble(csum_raw[i * 2])?;
        let lo = decode_nibble(csum_raw[i * 2 + 1])?;
        csum_bytes[i] = (hi << 4) | lo;
    }
    let expected_checksum = u32::from_be_bytes(csum_bytes);

    // Decode data pairs
    let data_raw = data_part.as_bytes();
    let num_bytes = data_raw.len() / 2;
    let mut decoded = Vec::with_capacity(num_bytes);
    let mut actual_checksum: u32 = 0;

    for i in 0..num_bytes {
        let hi = decode_nibble(data_raw[i * 2])?;
        let lo = decode_nibble(data_raw[i * 2 + 1])?;
        let x = (hi << 4) | lo;
        let b = x ^ XOR_KEY;
        actual_checksum = actual_checksum.wrapping_add(b as u32);
        decoded.push(b);
    }

    if actual_checksum != expected_checksum {
        return Err(ProtocolError::CnclChecksumMismatch {
            expected: expected_checksum,
            actual: actual_checksum,
        });
    }

    Ok(decoded)
}

#[inline]
fn decode_nibble(byte: u8) -> Result<u8, ProtocolError> {
    if (b'a'..=b'p').contains(&byte) {
        Ok(byte - b'a')
    } else {
        Err(ProtocolError::CnclInvalidCharacter(byte as char))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_string_round_trip() {
        let encoded = encode_cncl_string(b"");
        assert_eq!(encoded, "=aaaaaaaa");
        let decoded = decode_cncl_string(&encoded).unwrap();
        assert!(decoded.is_empty());
    }

    #[test]
    fn single_byte_encoding() {
        let encoded = encode_cncl_string(b"A");
        assert_eq!(encoded, "hi=aaaaaaeb");
        let decoded = decode_cncl_string(&encoded).unwrap();
        assert_eq!(decoded, b"A");
    }

    #[test]
    fn binary_round_trip() {
        let sample = b"Canon PIXMA G2010 High Resolution Profile \x00\xFF\x42\x13";
        let encoded = encode_cncl_string(sample);
        assert!(encoded.contains('='));
        let decoded = decode_cncl_string(&encoded).unwrap();
        assert_eq!(decoded.as_slice(), sample);
    }

    #[test]
    fn rejects_tampered_checksum() {
        let encoded = encode_cncl_string(b"Hello Canon");
        // Tamper with the last character of checksum
        let mut tampered = encoded.into_bytes();
        let last = tampered.last_mut().unwrap();
        *last = if *last == b'a' { b'b' } else { b'a' };
        let tampered_str = String::from_utf8(tampered).unwrap();

        assert!(matches!(
            decode_cncl_string(&tampered_str),
            Err(ProtocolError::CnclChecksumMismatch { .. })
        ));
    }

    #[test]
    fn rejects_invalid_characters() {
        assert!(matches!(
            decode_cncl_string("zz=aaaaaaaa"),
            Err(ProtocolError::CnclInvalidCharacter('z'))
        ));
        assert!(matches!(
            decode_cncl_string("hi=aaaaaaez"),
            Err(ProtocolError::CnclInvalidCharacter('z'))
        ));
    }

    #[test]
    fn rejects_missing_delimiter_or_odd_length() {
        assert!(matches!(
            decode_cncl_string("hiaaaaaaeb"),
            Err(ProtocolError::CnclInvalidFormat(_))
        ));
        assert!(matches!(
            decode_cncl_string("h=aaaaaaeb"),
            Err(ProtocolError::CnclInvalidFormat(_))
        ));
    }
}

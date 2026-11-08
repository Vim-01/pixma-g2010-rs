//! Canon BJL (Bubble Jet Level) protocol framing and command envelopes.
//!
//! BJL is an ASCII-based control protocol embedded within ESC/P printer data streams.
//! Commands are framed by `BJLSTART` and `BJLEND` delimiters, optionally preceded
//! by the 7-byte ESC/P entry sequence `\x1b[K\x02\x00\x00\x1f`.

/// 7-byte ESC/P command sequence used to switch printer firmware into BJL mode.
///
/// Bytes: `[0x1B, 0x5B, 0x4B, 0x02, 0x00, 0x00, 0x1F]` (`\x1b[K\x02\x00\x00\x1f`).
pub const BJL_ENTRY_PREFIX: [u8; 7] = [0x1b, b'[', b'K', 0x02, 0x00, 0x00, 0x1f];

/// Builder and generator for BJL protocol commands and envelopes.
#[derive(Debug, Default, Clone)]
pub struct BjlBuilder {
    include_prefix: bool,
    commands: Vec<String>,
}

impl BjlBuilder {
    /// Creates a new empty BJL builder without the 7-byte ESC/P prefix.
    pub fn new() -> Self {
        Self::default()
    }

    /// Configures the builder to prepend the 7-byte ESC/P entry prefix.
    pub fn with_prefix(mut self) -> Self {
        self.include_prefix = true;
        self
    }

    /// Appends a command line to the BJL envelope body.
    pub fn command(mut self, cmd: impl Into<String>) -> Self {
        self.commands.push(cmd.into());
        self
    }

    /// Returns the static 7-byte ESC/P BJL entry prefix.
    pub const fn entry_prefix() -> [u8; 7] {
        BJL_ENTRY_PREFIX
    }

    /// Generates the exact 62-byte time synchronization payload.
    ///
    /// Formatted with CRLF line endings:
    /// ```text
    /// BJLSTART\r\n
    /// ControlMode=Common\r\n
    /// SetTime=YYYYMMDDHHMMSS\r\n
    /// BJLEND\r\n
    /// ```
    /// When `timestamp` has 14 digits (e.g. `"20261106173528"`), the output length
    /// is precisely 62 bytes (10 + 20 + 24 + 8 = 62).
    pub fn time_sync_payload(timestamp: &str) -> Vec<u8> {
        let mut buf = Vec::with_capacity(62);
        buf.extend_from_slice(b"BJLSTART\r\n");
        buf.extend_from_slice(b"ControlMode=Common\r\n");
        buf.extend_from_slice(b"SetTime=");
        buf.extend_from_slice(timestamp.as_bytes());
        buf.extend_from_slice(b"\r\n");
        buf.extend_from_slice(b"BJLEND\r\n");
        buf
    }

    /// Generates the time synchronization payload using LF line endings (58 bytes).
    pub fn time_sync_payload_lf(timestamp: &str) -> Vec<u8> {
        let mut buf = Vec::with_capacity(58);
        buf.extend_from_slice(b"BJLSTART\n");
        buf.extend_from_slice(b"ControlMode=Common\n");
        buf.extend_from_slice(b"SetTime=");
        buf.extend_from_slice(timestamp.as_bytes());
        buf.extend_from_slice(b"\n");
        buf.extend_from_slice(b"BJLEND\n");
        buf
    }

    /// Generates the time synchronization sequence prepended with the 7-byte entry prefix.
    pub fn time_sync_with_prefix(timestamp: &str) -> Vec<u8> {
        let mut buf = Vec::with_capacity(7 + 62);
        buf.extend_from_slice(&BJL_ENTRY_PREFIX);
        buf.extend_from_slice(&Self::time_sync_payload(timestamp));
        buf
    }

    /// Generates the time synchronization sequence with LF endings prepended with the 7-byte entry prefix.
    pub fn time_sync_lf_with_prefix(timestamp: &str) -> Vec<u8> {
        let mut buf = Vec::with_capacity(7 + 58);
        buf.extend_from_slice(&BJL_ENTRY_PREFIX);
        buf.extend_from_slice(&Self::time_sync_payload_lf(timestamp));
        buf
    }

    /// Builds a framed BJL command envelope from a slice of command strings.
    pub fn build_envelope(commands: &[&str]) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"BJLSTART\r\n");
        for cmd in commands {
            buf.extend_from_slice(cmd.as_bytes());
            buf.extend_from_slice(b"\r\n");
        }
        buf.extend_from_slice(b"BJLEND\r\n");
        buf
    }

    /// Builds a framed BJL command envelope prepended with the 7-byte entry prefix.
    pub fn build_envelope_with_prefix(commands: &[&str]) -> Vec<u8> {
        let mut buf = Vec::with_capacity(BJL_ENTRY_PREFIX.len() + 32);
        buf.extend_from_slice(&BJL_ENTRY_PREFIX);
        buf.extend_from_slice(&Self::build_envelope(commands));
        buf
    }

    /// Finalizes and serializes the configured BJL payload.
    pub fn finish(self) -> Vec<u8> {
        let mut buf = Vec::new();
        if self.include_prefix {
            buf.extend_from_slice(&BJL_ENTRY_PREFIX);
        }
        buf.extend_from_slice(b"BJLSTART\r\n");
        for cmd in &self.commands {
            buf.extend_from_slice(cmd.as_bytes());
            buf.extend_from_slice(b"\r\n");
        }
        buf.extend_from_slice(b"BJLEND\r\n");
        buf
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bjl_entry_prefix_matches_specification() {
        assert_eq!(BJL_ENTRY_PREFIX, [0x1b, b'[', b'K', 0x02, 0x00, 0x00, 0x1f]);
        assert_eq!(BjlBuilder::entry_prefix(), BJL_ENTRY_PREFIX);
    }

    #[test]
    fn time_sync_payload_exact_62_bytes() {
        let timestamp = "20261106173528";
        let payload = BjlBuilder::time_sync_payload(timestamp);
        assert_eq!(
            payload.len(),
            62,
            "BJL time sync payload must be exactly 62 bytes"
        );
        let expected = b"BJLSTART\r\nControlMode=Common\r\nSetTime=20261106173528\r\nBJLEND\r\n";
        assert_eq!(payload.as_slice(), expected);
    }

    #[test]
    fn time_sync_with_prefix_length() {
        let timestamp = "20261106173528";
        let with_prefix = BjlBuilder::time_sync_with_prefix(timestamp);
        assert_eq!(with_prefix.len(), 7 + 62);
        assert_eq!(&with_prefix[0..7], &BJL_ENTRY_PREFIX);
        assert_eq!(&with_prefix[7..], &BjlBuilder::time_sync_payload(timestamp));
    }

    #[test]
    fn general_envelope_construction() {
        let envelope = BjlBuilder::build_envelope(&["ControlMode=Common", "StatusRequest=Ink"]);
        let text = std::str::from_utf8(&envelope).unwrap();
        assert!(text.starts_with("BJLSTART\r\n"));
        assert!(text.contains("ControlMode=Common\r\n"));
        assert!(text.contains("StatusRequest=Ink\r\n"));
        assert!(text.ends_with("BJLEND\r\n"));
    }

    #[test]
    fn fluent_builder_construction() {
        let payload = BjlBuilder::new()
            .with_prefix()
            .command("SetTime=20261106173528")
            .finish();

        assert_eq!(&payload[0..7], &BJL_ENTRY_PREFIX);
        let text = std::str::from_utf8(&payload[7..]).unwrap();
        assert_eq!(text, "BJLSTART\r\nSetTime=20261106173528\r\nBJLEND\r\n");
    }
}

//! The public JSON contract for `/v1/*`, grouped by domain.
//!
//! Deliberately identical to the Arc cloud gateway's data-plane contract, so the
//! same SDK/curl works against a self-hosted `brain-edge` and the hosted API —
//! only the base URL changes. 128-bit ids are decimal strings (they exceed the
//! JS safe-integer range); 16-byte ids are hyphenated UUID strings.

pub mod entity;
pub mod graph;
pub mod identity;
pub mod memory;
pub mod reasoning;
pub mod relation;
pub mod schema;
pub mod statement;

use brain_db_sdk::wire::types::WireMemoryId;

/// Parse a decimal memory-id string to a 128-bit wire id.
pub(crate) fn parse_memory_id(s: &str) -> Result<WireMemoryId, String> {
    s.trim()
        .parse::<WireMemoryId>()
        .map_err(|_| format!("`{s}` is not a valid memory id"))
}

/// Format a 16-byte big-endian memory id as its 128-bit decimal string —
/// the same form `WireMemoryId` renders, so a memory row and a graph memory
/// node share one id representation.
pub(crate) fn mem_id_decimal(b: &[u8; 16]) -> String {
    u128::from_be_bytes(*b).to_string()
}

/// Lowercase-hex encode opaque bytes (keyset cursors) for a URL query param.
/// Dependency-free; cursors are short (≤ ~60 bytes).
pub(crate) fn hex_encode(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for byte in b {
        s.push_str(&format!("{byte:02x}"));
    }
    s
}

/// Decode a lowercase/uppercase-hex cursor string back to bytes.
pub(crate) fn hex_decode(s: &str) -> Result<Vec<u8>, String> {
    let s = s.trim();
    if s.len() % 2 != 0 {
        return Err("cursor: odd-length hex".into());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|_| "cursor: invalid hex".to_string()))
        .collect()
}

/// Parse a canonical UUID string (hyphenated or not) into a raw 16-byte id.
/// Dependency-free counterpart to [`uuid_string`]; entity / statement / relation
/// path ids arrive as UUID strings and resolve back to the wire's 16-byte form.
pub(crate) fn parse_uuid(s: &str) -> Result<[u8; 16], String> {
    let hex: String = s.trim().chars().filter(|c| *c != '-').collect();
    if hex.len() != 32 {
        return Err(format!("`{s}` is not a valid UUID"));
    }
    let mut out = [0u8; 16];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)
            .map_err(|_| format!("`{s}` is not a valid UUID"))?;
    }
    Ok(out)
}

/// Format a 16-byte id as a canonical hyphenated UUID string (no `uuid` dep).
pub(crate) fn uuid_string(b: &[u8; 16]) -> String {
    let h = |i: usize| format!("{:02x}", b[i]);
    format!(
        "{}{}{}{}-{}{}-{}{}-{}{}-{}{}{}{}{}{}",
        h(0), h(1), h(2), h(3),
        h(4), h(5),
        h(6), h(7),
        h(8), h(9),
        h(10), h(11), h(12), h(13), h(14), h(15),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips() {
        let bytes = vec![0x2a, 0x00, 0xff, 0x18, 0x7b];
        let s = hex_encode(&bytes);
        assert_eq!(s, "2a00ff187b");
        assert_eq!(hex_decode(&s).unwrap(), bytes);
    }

    #[test]
    fn hex_decode_rejects_odd_and_nonhex() {
        assert!(hex_decode("abc").is_err());
        assert!(hex_decode("zz").is_err());
    }

    #[test]
    fn mem_id_decimal_is_be_u128() {
        let mut b = [0u8; 16];
        b[15] = 1;
        assert_eq!(mem_id_decimal(&b), "1");
        b = [0xffu8; 16];
        assert_eq!(mem_id_decimal(&b), u128::MAX.to_string());
    }

    #[test]
    fn uuid_parse_round_trips() {
        let mut b = [0u8; 16];
        b[0] = 0x11;
        b[15] = 0x2a;
        let s = uuid_string(&b);
        assert_eq!(s, "11000000-0000-0000-0000-00000000002a");
        assert_eq!(parse_uuid(&s).unwrap(), b);
        // Hyphen-free form parses identically.
        assert_eq!(parse_uuid("110000000000000000000000000000 2a".replace(' ', "").as_str()).unwrap(), b);
    }

    #[test]
    fn uuid_parse_rejects_bad_shapes() {
        assert!(parse_uuid("not-a-uuid").is_err());
        assert!(parse_uuid("11000000-0000-0000-0000-00000000002").is_err()); // 31 hex
        assert!(parse_uuid("zz000000-0000-0000-0000-00000000002a").is_err()); // non-hex
    }
}

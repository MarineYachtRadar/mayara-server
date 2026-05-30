// Various common functions

use std::fmt;

use serde::de::DeserializeOwned;

/// Decode a bincode-encoded slice into `T` using the legacy
/// (bincode v1) wire format: little-endian, fixed-int encoding, no
/// length limit. Every brand-specific report parser in this crate
/// uses the same `T: serde::Deserialize` path, so funnel them all
/// through one helper that hides bincode v2's `decode_from_slice`
/// signature (which returns `(T, usize)` and takes a config).
pub(crate) fn decode_bin<T: DeserializeOwned>(
    bytes: &[u8],
) -> Result<T, bincode::error::DecodeError> {
    bincode::serde::decode_from_slice(bytes, bincode::config::legacy()).map(|(value, _)| value)
}

pub(crate) fn c_string(bytes: &[u8]) -> Option<&str> {
    let bytes_without_null = match bytes.iter().position(|&b| b == 0) {
        Some(ix) => &bytes[..ix],
        None => bytes,
    };

    std::str::from_utf8(bytes_without_null).ok()
}
pub(crate) fn c_wide_string(bytes: &[u8]) -> String {
    let mut res = String::new();

    let mut i = bytes.iter();
    while let (Some(lo), Some(hi)) = (i.next(), i.next()) {
        let c = *lo as u32 + ((*hi as u32) << 8);
        if c == 0 {
            break;
        }
        if let Some(c) = std::char::from_u32(c) {
            res.push(c);
        }
    }
    res
}

pub(crate) struct PrintableSlice<'a>(&'a [u8]);

impl<'a> PrintableSlice<'a> {
    pub(crate) fn new<T>(data: &'a T) -> PrintableSlice<'a>
    where
        T: ?Sized + AsRef<[u8]> + 'a,
    {
        PrintableSlice(data.as_ref())
    }
}

// You can choose to implement multiple traits, like Lower and UpperPrintable

impl fmt::Display for PrintableSlice<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut sep: &str = "[";

        for byte in self.0 {
            if *byte >= 32 && *byte < 127 {
                write!(f, "{} {}", sep, *byte as char)?;
            } else {
                write!(f, "{} .", sep)?;
            }
            sep = "  ";
        }
        write!(f, "]")?;
        Ok(())
    }
}

pub(crate) struct PrintableSpoke<'a>(&'a [u8]);

impl<'a> PrintableSpoke<'a> {
    pub(crate) fn new<T>(data: &'a T) -> PrintableSpoke<'a>
    where
        T: ?Sized + AsRef<[u8]> + 'a,
    {
        PrintableSpoke(data.as_ref())
    }
}
impl fmt::Display for PrintableSpoke<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut sum: u32 = 0;
        let mut count: u32 = 0;

        write!(f, "[")?;
        for byte in self.0 {
            sum += *byte as u32;
            count += 1;

            if count == 8 {
                write!(
                    f,
                    "{}",
                    match sum {
                        0 => ' ',
                        1..512 => '.',
                        _ => '*',
                    }
                )?;
                count = 0;
                sum = 0;
            }
        }
        if count > 4 {
            write!(
                f,
                "{}",
                match sum {
                    0..8 => ' ',
                    8..512 => '.',
                    _ => '*',
                }
            )?;
        }
        write!(f, "]")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Serialize;

    #[derive(Debug, Serialize, serde::Deserialize, PartialEq)]
    struct WireSample {
        value: u32,
        flag: bool,
    }

    /// Round-tripping through bincode's serde encode + decode_bin
    /// verifies the helper accepts the legacy wire format and
    /// reconstructs the original value exactly. Brand wire structs
    /// use the same `T: Deserialize` path, so a generic shape here
    /// is enough to pin the contract.
    #[test]
    fn decode_bin_round_trips_legacy_wire_format() {
        let expected = WireSample {
            value: 0xDEADBEEF,
            flag: true,
        };
        let encoded =
            bincode::serde::encode_to_vec(&expected, bincode::config::legacy())
                .expect("legacy serde encode must succeed");
        let decoded: WireSample = decode_bin(&encoded).expect("decode_bin must succeed");
        assert_eq!(decoded, expected);
    }

    /// Garbage bytes shorter than the expected struct must surface
    /// as an Err rather than panicking or silently zero-padding.
    #[test]
    fn decode_bin_rejects_truncated_input() {
        let truncated = [0u8; 3];
        let result: Result<WireSample, _> = decode_bin(&truncated);
        assert!(
            result.is_err(),
            "decode_bin must reject a 3-byte buffer for a 5+ byte struct",
        );
    }
}

// Various common functions

use std::fmt;

/// Decode a packet that must fill `bytes` exactly. A packet of any other
/// length has a layout we do not know, so it is rejected rather than
/// half-read.
pub(crate) fn decode_exact<'a, T: deku::DekuContainerRead<'a>>(
    bytes: &'a [u8],
) -> Result<T, anyhow::Error> {
    let ((rest, _), value) = T::from_bytes((bytes, 0))?;
    if !rest.is_empty() {
        anyhow::bail!("{} unexpected bytes after the packet", rest.len());
    }
    Ok(value)
}

/// Decode the head of `bytes`, ignoring whatever follows it. Use
/// [`decode_exact`] where the packet must fill the slice exactly; use this
/// where a header is followed by a payload this decoder does not describe.
pub(crate) fn decode_head<'a, T: deku::DekuContainerRead<'a>>(
    bytes: &'a [u8],
) -> Result<T, anyhow::Error> {
    Ok(T::from_bytes((bytes, 0))?.1)
}

/// Write a fixed-size packet into memory, which has nothing to fail on. The
/// reading side is [`decode_exact`] and [`decode_head`].
pub(crate) fn encode(packet: &impl deku::DekuContainerWrite) -> Vec<u8> {
    packet
        .to_bytes()
        .expect("a fixed-size packet written into memory")
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

/// Deserialize an optional number that a client may have sent as a string.
///
/// Web clients build requests out of form and slider values, which are strings
/// whatever they hold — an `<input type="range">` has no numeric type at all.
/// A control value has always been accepted either way, so the numbers
/// alongside it are read the same way rather than failing the whole request
/// over a pair of quotes.
///
/// Anything that is not a number is still refused, as are the non-finite
/// values a string can express but JSON cannot.
pub fn deserialize_optional_number<'de, D>(deserializer: D) -> Result<Option<f64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize;
    use serde::de::{Error, Unexpected};

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum NumberOrString {
        Number(f64),
        String(String),
    }

    let expected = "a number, or a string holding one";
    let number = match Option::<NumberOrString>::deserialize(deserializer)? {
        None => return Ok(None),
        Some(NumberOrString::Number(n)) => n,
        Some(NumberOrString::String(s)) => s
            .trim()
            .parse()
            .map_err(|_| D::Error::invalid_value(Unexpected::Str(&s), &expected))?,
    };

    if !number.is_finite() {
        return Err(D::Error::invalid_value(
            Unexpected::Float(number),
            &expected,
        ));
    }

    Ok(Some(number))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, serde::Deserialize, PartialEq)]
    struct Sample {
        #[serde(default, deserialize_with = "deserialize_optional_number")]
        n: Option<f64>,
    }

    fn parse(json: &str) -> Result<Option<f64>, serde_json::Error> {
        serde_json::from_str::<Sample>(json).map(|s| s.n)
    }

    /// A slider hands its position over as a string, so the number a client
    /// means has to survive the quotes it arrives in.
    #[test]
    fn optional_number_reads_numbers_written_as_strings() {
        assert_eq!(parse(r#"{"n": -26}"#).unwrap(), Some(-26.));
        assert_eq!(parse(r#"{"n": "-26"}"#).unwrap(), Some(-26.));
        assert_eq!(parse(r#"{"n": "50"}"#).unwrap(), Some(50.));
        assert_eq!(parse(r#"{"n": "1.5"}"#).unwrap(), Some(1.5));
        assert_eq!(parse(r#"{"n": " 7 "}"#).unwrap(), Some(7.));
    }

    /// Absent and null both mean "not given", and must not become zero — a
    /// control left out of a request has to stay untouched.
    #[test]
    fn optional_number_keeps_absent_apart_from_zero() {
        assert_eq!(parse(r#"{}"#).unwrap(), None);
        assert_eq!(parse(r#"{"n": null}"#).unwrap(), None);
        assert_eq!(parse(r#"{"n": 0}"#).unwrap(), Some(0.));
        assert_eq!(parse(r#"{"n": "0"}"#).unwrap(), Some(0.));
    }

    /// Tolerating strings must not extend to values that are not numbers at
    /// all, nor to the non-finite ones JSON itself cannot express.
    #[test]
    fn optional_number_still_refuses_what_is_not_a_number() {
        assert!(parse(r#"{"n": "abc"}"#).is_err());
        assert!(parse(r#"{"n": ""}"#).is_err());
        assert!(parse(r#"{"n": true}"#).is_err());
        assert!(parse(r#"{"n": []}"#).is_err());
        assert!(parse(r#"{"n": "NaN"}"#).is_err());
        assert!(parse(r#"{"n": "inf"}"#).is_err());
    }
}

//! Minimal AMF0 encoder/decoder for RTMP command messages.
//!
//! RTMP command messages (`connect`, `createStream`, `publish`, …) are encoded
//! as a sequence of AMF0 values. This module implements the subset of AMF0
//! needed to parse those commands and to serialize the server's responses
//! (`_result`, `onStatus`, …).
//!
//! Supported markers: Number (0x00), Boolean (0x01), String (0x02),
//! Object (0x03), Null (0x05), Undefined (0x06), ECMA Array (0x08),
//! Object End (0x09), Strict Array (0x0A).

/// A decoded AMF0 value.
#[derive(Debug, Clone, PartialEq)]
pub enum Amf0Value {
    /// IEEE-754 double.
    Number(f64),
    /// Boolean.
    Boolean(bool),
    /// UTF-8 string.
    String(String),
    /// Anonymous object: ordered key/value pairs.
    Object(Vec<(String, Amf0Value)>),
    /// Null.
    Null,
    /// Undefined.
    Undefined,
    /// ECMA (associative) array: treated like an object.
    EcmaArray(Vec<(String, Amf0Value)>),
    /// Strict (dense) array.
    StrictArray(Vec<Amf0Value>),
}

impl Amf0Value {
    /// Returns the string contents if this is a `String`.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Amf0Value::String(s) => Some(s),
            _ => None,
        }
    }

    /// Returns the number if this is a `Number`.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Amf0Value::Number(n) => Some(*n),
            _ => None,
        }
    }

    /// Looks up a property by key in an `Object` or `EcmaArray`.
    pub fn get(&self, key: &str) -> Option<&Amf0Value> {
        match self {
            Amf0Value::Object(props) | Amf0Value::EcmaArray(props) => {
                props.iter().find(|(k, _)| k == key).map(|(_, v)| v)
            }
            _ => None,
        }
    }
}

/// Decode a sequence of AMF0 values from `data` until the buffer is exhausted.
pub fn decode_all(data: &[u8]) -> Result<Vec<Amf0Value>, AmfError> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < data.len() {
        let (value, consumed) = decode_value(&data[pos..])?;
        out.push(value);
        if consumed == 0 {
            break;
        }
        pos += consumed;
    }
    Ok(out)
}

/// Errors that occur while decoding AMF0.
#[derive(Debug, thiserror::Error)]
pub enum AmfError {
    /// Ran out of bytes mid-value.
    #[error("unexpected end of AMF0 data")]
    Truncated,
    /// Encountered an AMF0 marker we do not support.
    #[error("unsupported AMF0 marker: {0:#x}")]
    UnsupportedMarker(u8),
    /// String was not valid UTF-8.
    #[error("invalid UTF-8 in AMF0 string")]
    InvalidUtf8,
}

/// Decode a single AMF0 value, returning `(value, bytes_consumed)`.
pub fn decode_value(data: &[u8]) -> Result<(Amf0Value, usize), AmfError> {
    let marker = *data.first().ok_or(AmfError::Truncated)?;
    let rest = &data[1..];
    match marker {
        0x00 => {
            // Number
            let bytes: [u8; 8] = rest
                .get(..8)
                .ok_or(AmfError::Truncated)?
                .try_into()
                .unwrap();
            Ok((Amf0Value::Number(f64::from_be_bytes(bytes)), 1 + 8))
        }
        0x01 => {
            // Boolean
            let b = *rest.first().ok_or(AmfError::Truncated)?;
            Ok((Amf0Value::Boolean(b != 0), 1 + 1))
        }
        0x02 => {
            // String (u16 length prefix)
            let (s, n) = decode_string(rest)?;
            Ok((Amf0Value::String(s), 1 + n))
        }
        0x03 => {
            // Object
            let (props, n) = decode_object_properties(rest)?;
            Ok((Amf0Value::Object(props), 1 + n))
        }
        0x05 => Ok((Amf0Value::Null, 1)),
        0x06 => Ok((Amf0Value::Undefined, 1)),
        0x08 => {
            // ECMA array: u32 count, then the same as object properties.
            let _count = u32::from_be_bytes(
                rest.get(..4)
                    .ok_or(AmfError::Truncated)?
                    .try_into()
                    .unwrap(),
            );
            let (props, n) = decode_object_properties(&rest[4..])?;
            Ok((Amf0Value::EcmaArray(props), 1 + 4 + n))
        }
        0x0A => {
            // Strict array: u32 count then that many values.
            let count = u32::from_be_bytes(
                rest.get(..4)
                    .ok_or(AmfError::Truncated)?
                    .try_into()
                    .unwrap(),
            ) as usize;
            let mut pos = 4;
            let mut items = Vec::with_capacity(count);
            for _ in 0..count {
                let (v, c) = decode_value(&rest[pos..])?;
                items.push(v);
                pos += c;
            }
            Ok((Amf0Value::StrictArray(items), 1 + pos))
        }
        other => Err(AmfError::UnsupportedMarker(other)),
    }
}

/// Decode a u16-length-prefixed UTF-8 string (no marker byte).
fn decode_string(data: &[u8]) -> Result<(String, usize), AmfError> {
    let len = u16::from_be_bytes(
        data.get(..2)
            .ok_or(AmfError::Truncated)?
            .try_into()
            .unwrap(),
    ) as usize;
    let bytes = data.get(2..2 + len).ok_or(AmfError::Truncated)?;
    let s = std::str::from_utf8(bytes)
        .map_err(|_| AmfError::InvalidUtf8)?
        .to_string();
    Ok((s, 2 + len))
}

/// Decode object properties up to (and including) the object-end marker
/// (empty key followed by 0x09).
fn decode_object_properties(data: &[u8]) -> Result<(Vec<(String, Amf0Value)>, usize), AmfError> {
    let mut props = Vec::new();
    let mut pos = 0;
    loop {
        // Property key: u16-length-prefixed string.
        let (key, key_len) = decode_string(&data[pos..])?;
        pos += key_len;
        if key.is_empty() {
            // Expect the object-end marker 0x09.
            let end = *data.get(pos).ok_or(AmfError::Truncated)?;
            pos += 1;
            if end == 0x09 {
                break;
            }
            // Otherwise it's a value with an empty key; treat defensively.
        }
        let (value, val_len) = decode_value(&data[pos..])?;
        pos += val_len;
        props.push((key, value));
    }
    Ok((props, pos))
}

// ---------------------------------------------------------------------------
// Encoding
// ---------------------------------------------------------------------------

/// Encode a single AMF0 value, appending to `out`.
pub fn encode_value(out: &mut Vec<u8>, value: &Amf0Value) {
    match value {
        Amf0Value::Number(n) => {
            out.push(0x00);
            out.extend_from_slice(&n.to_be_bytes());
        }
        Amf0Value::Boolean(b) => {
            out.push(0x01);
            out.push(if *b { 1 } else { 0 });
        }
        Amf0Value::String(s) => {
            out.push(0x02);
            encode_string_body(out, s);
        }
        Amf0Value::Object(props) | Amf0Value::EcmaArray(props) => {
            out.push(0x03); // always serialize as a plain object
            for (k, v) in props {
                encode_string_body(out, k);
                encode_value(out, v);
            }
            // object end: empty string + 0x09
            out.extend_from_slice(&[0x00, 0x00, 0x09]);
        }
        Amf0Value::Null => out.push(0x05),
        Amf0Value::Undefined => out.push(0x06),
        Amf0Value::StrictArray(items) => {
            out.push(0x0A);
            out.extend_from_slice(&(items.len() as u32).to_be_bytes());
            for v in items {
                encode_value(out, v);
            }
        }
    }
}

/// Encode a sequence of AMF0 values into a fresh buffer.
pub fn encode_all(values: &[Amf0Value]) -> Vec<u8> {
    let mut out = Vec::new();
    for v in values {
        encode_value(&mut out, v);
    }
    out
}

fn encode_string_body(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(&(s.len() as u16).to_be_bytes());
    out.extend_from_slice(s.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_number_string_bool() {
        let values = vec![
            Amf0Value::String("connect".into()),
            Amf0Value::Number(1.0),
            Amf0Value::Boolean(true),
            Amf0Value::Null,
        ];
        let encoded = encode_all(&values);
        let decoded = decode_all(&encoded).unwrap();
        assert_eq!(decoded, values);
    }

    #[test]
    fn roundtrip_object() {
        let obj = Amf0Value::Object(vec![
            ("app".into(), Amf0Value::String("live".into())),
            ("tcUrl".into(), Amf0Value::String("rtmp://host/live".into())),
            ("fpad".into(), Amf0Value::Boolean(false)),
        ]);
        let encoded = encode_all(std::slice::from_ref(&obj));
        let decoded = decode_all(&encoded).unwrap();
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].get("app").and_then(|v| v.as_str()), Some("live"));
    }

    #[test]
    fn decodes_connect_command() {
        // Emulate a minimal 'connect' command: command name, transaction id,
        // command object.
        let cmd = vec![
            Amf0Value::String("connect".into()),
            Amf0Value::Number(1.0),
            Amf0Value::Object(vec![("app".into(), Amf0Value::String("live".into()))]),
        ];
        let bytes = encode_all(&cmd);
        let decoded = decode_all(&bytes).unwrap();
        assert_eq!(decoded[0].as_str(), Some("connect"));
        assert_eq!(decoded[1].as_f64(), Some(1.0));
        assert_eq!(decoded[2].get("app").and_then(|v| v.as_str()), Some("live"));
    }

    #[test]
    fn truncated_string_errors() {
        // marker 0x02, length says 5 but no bytes follow
        let bytes = [0x02, 0x00, 0x05];
        assert!(matches!(decode_value(&bytes), Err(AmfError::Truncated)));
    }

    #[test]
    fn unsupported_marker_errors() {
        let bytes = [0xFE];
        assert!(matches!(
            decode_value(&bytes),
            Err(AmfError::UnsupportedMarker(0xFE))
        ));
    }
}

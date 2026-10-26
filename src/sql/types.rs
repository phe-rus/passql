//! SQL value types and encoding.
//!
//! Values are stored in a typed binary form inside the v4 record payload.
//! The encoding is self-describing: a 1-byte type tag precedes each value.

use std::io::{self, ErrorKind};

/// A typed SQL value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

impl Value {
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Null => "NULL",
            Self::Integer(_) => "INTEGER",
            Self::Real(_) => "REAL",
            Self::Text(_) => "TEXT",
            Self::Blob(_) => "BLOB",
        }
    }

    /// Encode a value as `(type_tag, payload_bytes)`.
    /// The caller is responsible for writing the tag and length prefix.
    pub(crate) fn encode(&self) -> Vec<u8> {
        match self {
            Self::Null => Vec::new(),
            Self::Integer(value) => value.to_le_bytes().to_vec(),
            Self::Real(value) => value.to_le_bytes().to_vec(),
            Self::Text(value) => value.as_bytes().to_vec(),
            Self::Blob(value) => value.clone(),
        }
    }

    /// Decode a value of known type from a payload slice.
    pub(crate) fn decode(tag: u8, payload: &[u8]) -> Result<Self, ValueError> {
        match tag {
            0 => Ok(Self::Null),
            1 => Ok(Self::Integer(i64::from_le_bytes(
                payload
                    .try_into()
                    .map_err(|_| ValueError::InvalidIntegerLength(payload.len()))?,
            ))),
            2 => {
                Ok(Self::Real(f64::from_le_bytes(payload.try_into().map_err(
                    |_| ValueError::InvalidRealLength(payload.len()),
                )?)))
            }
            3 => Ok(Self::Text(
                String::from_utf8(payload.to_vec()).map_err(|_| ValueError::InvalidUtf8)?,
            )),
            4 => Ok(Self::Blob(payload.to_vec())),
            _ => Err(ValueError::UnknownTypeTag(tag)),
        }
    }

    pub fn as_integer(&self) -> Option<i64> {
        match self {
            Self::Integer(value) => Some(*value),
            _ => None,
        }
    }

    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(value) => Some(value),
            _ => None,
        }
    }
}

impl From<i64> for Value {
    fn from(value: i64) -> Self {
        Self::Integer(value)
    }
}

impl From<&str> for Value {
    fn from(value: &str) -> Self {
        Self::Text(value.to_owned())
    }
}

impl From<String> for Value {
    fn from(value: String) -> Self {
        Self::Text(value)
    }
}

impl From<Vec<u8>> for Value {
    fn from(value: Vec<u8>) -> Self {
        Self::Blob(value)
    }
}

impl From<f64> for Value {
    fn from(value: f64) -> Self {
        Self::Real(value)
    }
}

/// Errors raised while encoding or decoding SQL values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValueError {
    InvalidIntegerLength(usize),
    InvalidRealLength(usize),
    InvalidUtf8,
    UnknownTypeTag(u8),
}

impl std::fmt::Display for ValueError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidIntegerLength(len) => {
                write!(f, "invalid integer length: {len}")
            }
            Self::InvalidRealLength(len) => {
                write!(f, "invalid real length: {len}")
            }
            Self::InvalidUtf8 => write!(f, "invalid UTF-8 in TEXT value"),
            Self::UnknownTypeTag(tag) => write!(f, "unknown value type tag: {tag}"),
        }
    }
}

impl std::error::Error for ValueError {}

impl From<ValueError> for io::Error {
    fn from(error: ValueError) -> Self {
        io::Error::new(ErrorKind::InvalidData, error.to_string())
    }
}

//! Clean-slate protocol primitives for nanoctl 4.
//!
//! The crate is deliberately free of operating-system, networking, codec, and async
//! dependencies. Every parser is total over byte slices and every state machine is
//! deterministic so transport and platform implementations can be tested against one
//! canonical contract.

pub mod input;
pub mod session;
pub mod video;

pub const WIRE_VERSION: u8 = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireError {
    Empty,
    Truncated,
    UnsupportedVersion(u8),
    UnknownKind(u8),
    InvalidLength,
    InvalidFlags,
    InvalidUtf8,
    InvalidValue(&'static str),
}

impl core::fmt::Display for WireError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Empty => f.write_str("empty wire message"),
            Self::Truncated => f.write_str("truncated wire message"),
            Self::UnsupportedVersion(version) => write!(f, "unsupported wire version {version}"),
            Self::UnknownKind(kind) => write!(f, "unknown wire message kind {kind:#x}"),
            Self::InvalidLength => f.write_str("invalid wire message length"),
            Self::InvalidFlags => f.write_str("invalid wire message flags"),
            Self::InvalidUtf8 => f.write_str("wire message is not valid UTF-8"),
            Self::InvalidValue(value) => write!(f, "invalid wire value: {value}"),
        }
    }
}

impl std::error::Error for WireError {}

fn versioned(
    bytes: &[u8],
    expected_namespace: std::ops::RangeInclusive<u8>,
) -> Result<u8, WireError> {
    if bytes.is_empty() {
        return Err(WireError::Empty);
    }
    if bytes.len() < 2 {
        return Err(WireError::Truncated);
    }
    if bytes[0] != WIRE_VERSION {
        return Err(WireError::UnsupportedVersion(bytes[0]));
    }
    let kind = bytes[1];
    if !expected_namespace.contains(&kind) {
        return Err(WireError::UnknownKind(kind));
    }
    Ok(kind)
}

fn u16_at(bytes: &[u8], offset: usize) -> Result<u16, WireError> {
    let value = bytes.get(offset..offset + 2).ok_or(WireError::Truncated)?;
    Ok(u16::from_le_bytes([value[0], value[1]]))
}

fn u32_at(bytes: &[u8], offset: usize) -> Result<u32, WireError> {
    let value = bytes.get(offset..offset + 4).ok_or(WireError::Truncated)?;
    Ok(u32::from_le_bytes([value[0], value[1], value[2], value[3]]))
}

fn u64_at(bytes: &[u8], offset: usize) -> Result<u64, WireError> {
    let value = bytes.get(offset..offset + 8).ok_or(WireError::Truncated)?;
    Ok(u64::from_le_bytes([
        value[0], value[1], value[2], value[3], value[4], value[5], value[6], value[7],
    ]))
}

fn f32_at(bytes: &[u8], offset: usize) -> Result<f32, WireError> {
    Ok(f32::from_bits(u32_at(bytes, offset)?))
}

fn push_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_f32(out: &mut Vec<u8>, value: f32) {
    push_u32(out, value.to_bits());
}

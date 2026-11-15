//! Length prefixes for the byte formats that are hashed or signed.
//!
//! The data being framed is held in memory, so a length past the prefix's range cannot occur in practice.
//! The helpers still saturate instead of panicking: a saturated prefix never matches the bytes that
//! follow, so a reader rejects the frame as corrupt rather than misparsing it.

/// `n` as a big-endian `u32` prefix, saturating at `u32::MAX`.
pub(crate) fn be_len32(n: usize) -> [u8; 4] {
    u32::try_from(n).unwrap_or(u32::MAX).to_be_bytes()
}

/// `n` as a big-endian `u16` prefix, saturating at `u16::MAX`.
pub(crate) fn be_len16(n: usize) -> [u8; 2] {
    u16::try_from(n).unwrap_or(u16::MAX).to_be_bytes()
}

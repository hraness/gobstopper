/// Rough chars-per-token divisor used when a transcript carries no
/// provider usage accounting. Deliberately conservative (over-estimates).
pub const CHARS_PER_TOKEN: u64 = 4;

pub fn estimate_tokens(text_len: usize) -> u64 {
    (text_len as u64).div_ceil(CHARS_PER_TOKEN).max(1)
}

//! Revision 1 portable names; preserve spelling and compare canonical caseless keys.
use crate::format::{FsError, Result};
use caseless::Caseless;
use unicode_normalization::UnicodeNormalization;

pub const MAX_KEY: usize = 16384;
pub fn validate_name(bytes: &[u8]) -> Result<&str> {
    let s = std::str::from_utf8(bytes).map_err(|_| FsError::InvalidName)?;
    if s.is_empty()
        || bytes.len() > 255
        || s == "."
        || s == ".."
        || s.ends_with([' ', '.'])
        || s.chars()
            .any(|c| c.is_control() || "<>:\"/\\|?*".contains(c))
    {
        return Err(FsError::InvalidName);
    }
    let stem = s
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end_matches([' ', '.'])
        .to_uppercase();
    if ["CON", "PRN", "AUX", "NUL", "CLOCK$", "CONIN$", "CONOUT$"].contains(&stem.as_str())
        || ["COM", "LPT"].iter().any(|p| {
            stem.strip_prefix(p).is_some_and(|n| {
                ["1", "2", "3", "4", "5", "6", "7", "8", "9", "¹", "²", "³"].contains(&n)
            })
        })
    {
        return Err(FsError::InvalidName);
    }
    Ok(s)
}
pub fn comparison_key(bytes: &[u8]) -> Result<String> {
    let s = validate_name(bytes)?;
    let mut out = String::new();
    for c in s.nfd().default_case_fold().nfd() {
        if out.len() + c.len_utf8() > MAX_KEY {
            return Err(FsError::ResourceLimit);
        }
        out.push(c);
    }
    Ok(out)
}

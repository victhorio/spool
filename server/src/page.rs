//! Paging shared by every listing: page sizes, and opaque cursors that resume one.

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Serialize, de::DeserializeOwned};

use crate::error::ApiError;

/// The cursor format. Each cursor's fields and variant order are part of it:
/// change this before reordering them.
const VERSION: u8 = 2;

/// A page size from 1 to `maximum`, `default` when absent.
pub(crate) fn limit(value: Option<u16>, default: u16, maximum: u16) -> Result<u16, ApiError> {
    let value = value.unwrap_or(default);
    if (1..=maximum).contains(&value) {
        Ok(value)
    } else {
        Err(ApiError::malformed())
    }
}

/// Pages fetch one row past `limit` to learn whether another follows. Drops it
/// and returns the last row kept, which the next page resumes after, if so.
pub(crate) fn last<T>(rows: &mut Vec<T>, limit: u16) -> Option<&T> {
    if rows.len() <= usize::from(limit) {
        return None;
    }
    rows.truncate(limit.into());
    rows.last()
}

pub(crate) fn encode(cursor: &impl Serialize) -> String {
    URL_SAFE_NO_PAD.encode(postcard::to_extend(cursor, vec![VERSION]).expect("serializable cursor"))
}

pub(crate) fn decode<T: DeserializeOwned>(value: &str) -> Result<T, ApiError> {
    if value.len() > 8192 {
        return Err(ApiError::malformed());
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| ApiError::malformed())?;
    let Some((&VERSION, encoded)) = bytes.split_first() else {
        return Err(ApiError::malformed());
    };
    match postcard::take_from_bytes(encoded) {
        Ok((cursor, [])) => Ok(cursor),
        _ => Err(ApiError::malformed()),
    }
}

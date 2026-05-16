//! The wall clock, in Unix milliseconds.

use std::time::{SystemTime, UNIX_EPOCH};

/// Returns the current time in milliseconds since the Unix epoch.
pub(crate) fn now_ms() -> u64 {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
}

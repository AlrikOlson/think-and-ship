//! In-memory session bookkeeping.

use super::history::DeliberateHistory;

/// Session entry tracks the per-session history with a last-access timestamp.
#[derive(Debug, Clone)]
pub struct SessionEntry {
    pub history: DeliberateHistory,
    /// Unix millis of last access.
    pub last_accessed: u128,
}

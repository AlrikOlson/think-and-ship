//! In-memory session bookkeeping.

use super::history::ThinkHistory;

/// Session entry tracks the per-session history with a last-access timestamp.
#[derive(Debug, Clone)]
pub struct SessionEntry {
    pub history: ThinkHistory,
    /// Unix millis of last access.
    pub last_accessed: u128,
}

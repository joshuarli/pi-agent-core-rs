//! Helpers for projecting typed transcript entries into stable snapshot labels.

use crate::app::TranscriptEntry;

/// Return the semantic text used by normalized renderer snapshots.
pub fn label(entry: &TranscriptEntry) -> &str {
    match entry {
        TranscriptEntry::Welcome { text }
        | TranscriptEntry::User { text }
        | TranscriptEntry::Assistant { text, .. }
        | TranscriptEntry::Notice { text, .. }
        | TranscriptEntry::Error { text } => text,
        TranscriptEntry::Tool(tool) => tool
            .settled_result
            .as_deref()
            .or(tool.latest_progress.as_deref())
            .unwrap_or(tool.tool_name.as_str()),
    }
}

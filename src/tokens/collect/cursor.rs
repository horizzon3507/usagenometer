//! Cursor session files — TODO: implemented by a sibling PR.

use crate::tokens::TokenEvent;
use crate::tokens::collect::ScanOffsets;

/// TODO: parse Cursor local session/log files once the on-disk format is
/// confirmed. Returns no events until then.
pub fn collect(_offsets: &mut ScanOffsets) -> Vec<TokenEvent> {
    Vec::new()
}

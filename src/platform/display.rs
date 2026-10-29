//! Our display settings, kept in the game's registry key: fullscreen (borderless, on the
//! window's display) is REG_DWORD `Fullscreen` (default off); Alt+Enter switches and saves it,
//! the window starts that way. The game's own resolution is not touched.

use super::advapi32;

/// The saved fullscreen choice.
pub fn fullscreen() -> bool {
    advapi32::value("", "Fullscreen").is_some_and(|(_, d)| d.first().is_some_and(|&b| b != 0))
}

/// Save the fullscreen choice (Alt+Enter: the window already switched).
pub fn remember_fullscreen(on: bool) {
    advapi32::set_value("", "Fullscreen", 4, &(on as u32).to_le_bytes());
}

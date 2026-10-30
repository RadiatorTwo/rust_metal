//! Our display settings, kept in the game's registry key: fullscreen (borderless, on the
//! window's display) is REG_DWORD `Fullscreen` (default on; off: a window with a frame the size of the
//! game's picture); Alt+Enter switches and saves it, the window starts that way. Never
//! exclusive fullscreen: the display mode stays the desktop's, the game's resolution is its
//! back buffer's, shown with its aspect ratio (`d3d9::letterbox`).

use super::advapi32;

/// The saved fullscreen choice.
pub fn fullscreen() -> bool {
    advapi32::value("", "Fullscreen").is_none_or(|(_, d)| d.first().is_none_or(|&b| b != 0))
}

/// Save the fullscreen choice (Alt+Enter: the window already switched).
pub fn remember_fullscreen(on: bool) {
    advapi32::set_value("", "Fullscreen", 4, &(on as u32).to_le_bytes());
}

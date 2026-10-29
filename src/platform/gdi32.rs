//! GDI32: the little GDI the game uses: a stock brush for its window class (GetStockObject),
//! DeleteObject. The window is drawn by Direct3D 9, never by GDI.

use super::Import;
use crate::rt::Cpu;

/// The screen's device context (GetDC).
pub const SCREEN_DC: u32 = 0xdc00;
/// The stock objects: one handle each, 0x6600 + index.
const STOCK: u32 = 0x6600;

fn get_stock_object(c: &mut Cpu) -> u32 {
    STOCK + (super::arg(c, 0) & 0xff)
}

fn ret1(_: &mut Cpu) -> u32 {
    1
}

pub const IMPORTS: &[Import] = &[("GetStockObject", 1, get_stock_object), ("DeleteObject", 1, ret1)];

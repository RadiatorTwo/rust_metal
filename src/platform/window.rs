//! The game window (winit): a real window for Direct3D 9 to present into (`host_hwnd`), and
//! the window system's input as the game's terms. The game's own window is guest bookkeeping
//! in `user32`; its window procedure stays guest code. Everything here lives on the game's
//! main thread: the game creates its window and pumps its messages there.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};
use std::thread::ThreadId;
use std::time::Duration;
use winit::application::ApplicationHandler;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::platform::pump_events::EventLoopExtPumpEvents;
use winit::window::{Window, WindowId};

/// Input for the game, in game terms: Windows virtual-key codes and client pixels of the size
/// the game gave its window.
#[derive(Clone, Copy, Debug)]
pub enum Input {
    /// `scan`: the set-1 scan code with bit 7 for extended keys (right Ctrl and Alt, arrows,
    /// the block above them, keypad Enter and /), which is DirectInput's DIK code
    Key { vk: u8, scan: u8, down: bool, repeat: bool },
    Char(u8),
    Move(i32, i32),
    /// 0 left, 1 right, 2 middle
    Button(u8, bool),
    /// mouse wheel: lines, positive away from the user
    Wheel(i32),
    Close,
    Focus(bool),
    /// scripted only: a dword written to guest memory (address, value)
    Poke(u32, u32),
}

struct App {
    /// the window to open at the next pump: title and client size
    want: Option<(String, u32, u32)>,
    /// the game made its window a popup (no frame and title bar)
    popup: bool,
    /// a fullscreen Direct3D device sets `game_size` (its back buffer), not SetWindowPos
    device_fullscreen: bool,
    window: Option<Window>,
    /// the client size the game asked for (input positions are in its pixels)
    game_size: (u32, u32),
    input: VecDeque<Input>,
    cursor: PhysicalPosition<f64>,
    /// an Alt key is held (Alt+Enter toggles fullscreen)
    alt: bool,
    /// times `keep_fullscreen` had to make the window fullscreen again
    refits: u32,
}

struct Host {
    event_loop: EventLoop<()>,
    app: App,
}

thread_local! {
    static HOST: RefCell<Option<Host>> = const { RefCell::new(None) };
}

/// The thread that owns the window (the game's main thread).
static MAIN: OnceLock<ThreadId> = OnceLock::new();

/// The window's HWND once it exists (for other threads, `host_hwnd`).
static HWND: Mutex<isize> = Mutex::new(0);

fn on_main() -> bool {
    *MAIN.get_or_init(|| std::thread::current().id()) == std::thread::current().id()
}

fn with_host<R>(f: impl FnOnce(&mut Host) -> R) -> R {
    HOST.with(|h| {
        let mut h = h.borrow_mut();
        let host = h.get_or_insert_with(|| {
            #[allow(unused_mut)]
            let mut b = EventLoop::builder();
            #[cfg(windows)]
            winit::platform::windows::EventLoopBuilderExtWindows::with_any_thread(&mut b, true);
            Host {
                event_loop: b.build().expect("cannot open the window system"),
                app: App {
                    want: None,
                    popup: false,
                    device_fullscreen: false,
                    window: None,
                    game_size: (640, 480),
                    input: VecDeque::new(),
                    cursor: PhysicalPosition::new(0.0, 0.0),
                    alt: false,
                    refits: 0,
                },
            }
        });
        f(host)
    })
}

/// Open the window with a client area of w x h (shown at the next pump).
pub fn open(title: &str, w: u32, h: u32) {
    if !on_main() {
        return;
    }
    with_host(|host| {
        if host.app.window.is_none() {
            host.app.want = Some((title.to_string(), w.max(1), h.max(1)));
            host.app.game_size = (w.max(1), h.max(1));
        }
    });
    pump(Duration::ZERO);
}

/// The real window's HWND for the game's window `guest` (its main window; 0 for any other or
/// before the window exists), for Direct3D 9.
pub fn host_hwnd(guest: u32) -> isize {
    if guest != super::user32::main_window() {
        return 0;
    }
    *HWND.lock().unwrap()
}

/// The game's window for a real window's HWND (the main window for ours, else 0).
pub fn guest_hwnd(host: isize) -> u32 {
    if host != 0 && host == *HWND.lock().unwrap() { super::user32::main_window() } else { 0 }
}

/// The game moved or sized its main window (SetWindowPos; desktop coordinates, the desktop
/// being the display the window is on): the real window follows unless it is fullscreen, and
/// input positions are in pixels of the new size. Gun Metal makes its window a popup the
/// size of the desktop (GetSystemMetrics) before it creates its Direct3D device.
pub fn place(x: i32, y: i32, w: u32, h: u32) {
    if !on_main() {
        return;
    }
    let (w, h) = (w.max(1), h.max(1));
    with_host(|host| {
        if !host.app.device_fullscreen {
            host.app.game_size = (w, h);
        }
        if let Some(want) = &mut host.app.want {
            (want.1, want.2) = (w, h);
        }
        let Some(win) = &host.app.window else { return };
        if win.fullscreen().is_some() {
            return;
        }
        let origin = win.current_monitor().map_or(PhysicalPosition::new(0, 0), |m| m.position());
        win.set_outer_position(PhysicalPosition::new(origin.x + x, origin.y + y));
        let _ = win.request_inner_size(PhysicalSize::new(w, h));
    });
}

/// A fullscreen Direct3D device of w x h (CreateDevice, Reset): input positions are in its
/// pixels until the device is windowed again; SetWindowPos no longer changes that.
pub fn set_game_size(w: u32, h: u32) {
    if !on_main() {
        return;
    }
    with_host(|host| {
        host.app.device_fullscreen = true;
        host.app.game_size = (w.max(1), h.max(1));
    });
}

/// The Direct3D device is windowed: input positions are in pixels of the game's main window
/// again.
pub fn windowed_game_size() {
    if !on_main() {
        return;
    }
    let r = super::user32::main_rect();
    with_host(|host| {
        host.app.device_fullscreen = false;
        host.app.game_size = (((r[2] - r[0]).max(1)) as u32, ((r[3] - r[1]).max(1)) as u32);
    });
}

/// The game set its main window's style (SetWindowLongA GWL_STYLE): without a caption it is a
/// popup, and the real window loses its frame and title bar.
pub fn set_decorated(decorated: bool) {
    if !on_main() {
        return;
    }
    with_host(|host| {
        host.app.popup = !decorated;
        if let Some(win) = &host.app.window {
            win.set_decorations(decorated);
        }
    });
}

/// The size in pixels of the display the window is on (the primary one before the window
/// exists), for GetSystemMetrics.
pub fn screen_size() -> (u32, u32) {
    let monitor = |h: &mut Host| h.app.window.as_ref().and_then(|w| w.current_monitor().or_else(|| w.primary_monitor())).map(|m| (m.size().width, m.size().height));
    on_main().then(|| with_host(monitor)).flatten().unwrap_or_else(host_screen_size)
}

#[cfg(windows)]
fn host_screen_size() -> (u32, u32) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};
    unsafe { (GetSystemMetrics(SM_CXSCREEN) as u32, GetSystemMetrics(SM_CYSCREEN) as u32) }
}

#[cfg(not(windows))]
fn host_screen_size() -> (u32, u32) {
    (1920, 1080)
}

/// Handle the window system's events, waiting up to `wait` for one.
pub fn pump(wait: Duration) {
    if !on_main() {
        return;
    }
    with_host(|h| {
        let _ = h.event_loop.pump_app_events(Some(wait), &mut h.app);
        scripted(&mut h.app.input);
    });
}

/// GUNMETAL_INPUT="9000:click@484,303 12000:Enter 15000:Up*3000": input at fixed times (ms
/// after start) for tests. `click@x,y` / `rclick@x,y` (client pixels of the game's size, after
/// a move there), `move@x,y`, a key held 100 ms or `*ms` (Enter, Escape, Space, Tab, Up, Down,
/// Left, Right, Ctrl, Alt, Shift, A-Z, 0-9, Numpad0-9, F1-F12), `close` (the window's close
/// button), `blur` / `focus` (the window loses or gets the focus), `poke@511fc0=1` (a dword
/// written to guest memory, hex address and value), `text@Name` (typed characters, `_` a
/// space).
fn scripted(input: &mut VecDeque<Input>) {
    static SCRIPT: Mutex<Option<VecDeque<(u64, Input)>>> = Mutex::new(None);
    let mut s = SCRIPT.lock().unwrap();
    let script = s.get_or_insert_with(|| std::env::var("GUNMETAL_INPUT").map(|v| parse_script(&v)).unwrap_or_default());
    let now = super::kernel32::start_time().elapsed().as_millis() as u64;
    while script.front().is_some_and(|e| e.0 <= now) {
        match script.pop_front().unwrap().1 {
            Input::Poke(a, v) => super::mem().wr32(a, v),
            i => input.push_back(i),
        }
    }
}

fn parse_script(text: &str) -> VecDeque<(u64, Input)> {
    let mut ev = Vec::new();
    for item in text.split_whitespace() {
        let Some((t, what)) = item.split_once(':') else { continue };
        let Ok(t) = t.parse::<u64>() else { continue };
        let at = |s: &str| -> Option<(i32, i32)> {
            let (x, y) = s.split_once(',')?;
            Some((x.parse().ok()?, y.parse().ok()?))
        };
        if let Some(text) = what.strip_prefix("text@") {
            // typed characters (WM_CHAR), 50 ms apart; `_` is a space
            for (n, ch) in text.chars().filter(|&c| (c as u32) < 256).enumerate() {
                ev.push((t + 50 * n as u64, Input::Char(if ch == '_' { b' ' } else { ch as u8 })));
            }
        } else if let Some(p) = what.strip_prefix("poke@") {
            let hex = |s: &str| u32::from_str_radix(s, 16).ok();
            if let Some((a, v)) = p.split_once('=').and_then(|(a, v)| Some((hex(a)?, hex(v)?))) {
                ev.push((t, Input::Poke(a, v)));
            }
        } else if let Some((b, xy)) = what.split_once('@') {
            let Some((x, y)) = at(xy) else { continue };
            ev.push((t, Input::Move(x, y)));
            let b = match b {
                "click" => 0,
                "rclick" => 1,
                _ => continue,
            };
            ev.push((t + 30, Input::Button(b, true)));
            ev.push((t + 130, Input::Button(b, false)));
        } else if what == "close" {
            ev.push((t, Input::Close));
        } else if what == "blur" || what == "focus" {
            ev.push((t, Input::Focus(what == "focus")));
        } else {
            let (name, hold) = what.split_once('*').map_or((what, 100), |(n, h)| (n, h.parse().unwrap_or(100)));
            let Some((vk, scan)) = key_by_name(name).and_then(vk_of) else {
                eprintln!("GUNMETAL_INPUT: unknown key {name}");
                continue;
            };
            ev.push((t, Input::Key { vk, scan, down: true, repeat: false }));
            ev.push((t + hold, Input::Key { vk, scan, down: false, repeat: false }));
        }
    }
    ev.sort_by_key(|e| e.0);
    ev.into()
}

const FN: [KeyCode; 12] = [
    KeyCode::F1, KeyCode::F2, KeyCode::F3, KeyCode::F4, KeyCode::F5, KeyCode::F6, KeyCode::F7, KeyCode::F8, KeyCode::F9, KeyCode::F10,
    KeyCode::F11, KeyCode::F12,
];
const LETTERS: [KeyCode; 26] = {
    use KeyCode::*;
    [KeyA, KeyB, KeyC, KeyD, KeyE, KeyF, KeyG, KeyH, KeyI, KeyJ, KeyK, KeyL, KeyM, KeyN, KeyO, KeyP, KeyQ, KeyR, KeyS, KeyT, KeyU, KeyV, KeyW, KeyX, KeyY, KeyZ]
};
const DIGITS: [KeyCode; 10] = {
    use KeyCode::*;
    [Digit0, Digit1, Digit2, Digit3, Digit4, Digit5, Digit6, Digit7, Digit8, Digit9]
};
const PAD: [KeyCode; 10] = {
    use KeyCode::*;
    [Numpad0, Numpad1, Numpad2, Numpad3, Numpad4, Numpad5, Numpad6, Numpad7, Numpad8, Numpad9]
};

fn key_by_name(n: &str) -> Option<KeyCode> {
    use KeyCode::*;
    let b = n.as_bytes();
    if b.len() == 1 && b[0].is_ascii_alphabetic() {
        return Some(LETTERS[(b[0].to_ascii_uppercase() - b'A') as usize]);
    }
    if b.len() == 1 && b[0].is_ascii_digit() {
        return Some(DIGITS[(b[0] - b'0') as usize]);
    }
    if let Some(d) = n.strip_prefix("Numpad").and_then(|d| d.parse::<usize>().ok()) {
        return PAD.get(d).copied();
    }
    if let Some(d) = n.strip_prefix('F').and_then(|d| d.parse::<usize>().ok()) {
        return FN.get(d.wrapping_sub(1)).copied();
    }
    Some(match n {
        "Enter" | "Return" => Enter,
        "Escape" | "Esc" => Escape,
        "Space" => Space,
        "Tab" => Tab,
        "Up" => ArrowUp,
        "Down" => ArrowDown,
        "Left" => ArrowLeft,
        "Right" => ArrowRight,
        "Ctrl" => ControlLeft,
        "Alt" => AltLeft,
        "Shift" => ShiftLeft,
        _ => return None,
    })
}

/// The next input event, if any (call `pump` first).
pub fn next_input() -> Option<Input> {
    if !on_main() {
        return None;
    }
    with_host(|h| h.app.input.pop_front())
}

/// The window's client size in pixels (the game's size before it exists).
pub fn window_size() -> (u32, u32) {
    if !on_main() {
        return (640, 480);
    }
    with_host(|h| {
        let fallback = h.app.game_size;
        h.app.window.as_ref().map(|w| w.inner_size()).filter(|s| s.width > 0 && s.height > 0).map_or(fallback, |s| (s.width, s.height))
    })
}

/// Borderless fullscreen on the display the window is on.
fn borderless(w: &Window) -> winit::window::Fullscreen {
    winit::window::Fullscreen::Borderless(w.current_monitor())
}

/// Is the window fullscreen?
pub fn is_fullscreen() -> bool {
    on_main() && with_host(|h| h.app.window.as_ref().is_some_and(|w| w.fullscreen().is_some()))
}

/// The HWND of a winit window.
fn hwnd_of(w: &Window) -> isize {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match w.window_handle().map(|h| h.as_raw()) {
        Ok(RawWindowHandle::Win32(h)) => h.hwnd.get(),
        _ => 0,
    }
}

impl App {
    fn create(&mut self, el: &ActiveEventLoop) {
        let Some((title, w, h)) = self.want.take() else { return };
        // GUNMETAL_WINDOW=1920x1080: the window's starting size in pixels (tests); else the
        // size the game asked for
        let size = std::env::var("GUNMETAL_WINDOW")
            .ok()
            .and_then(|v| v.split_once('x').and_then(|(w, h)| Some((w.parse::<u32>().ok()?, h.parse::<u32>().ok()?))))
            .unwrap_or((w, h));
        let attrs = Window::default_attributes().with_title(title).with_inner_size(PhysicalSize::new(size.0, size.1)).with_decorations(!self.popup);
        // GUNMETAL_MONITOR=1: opened on that display (0 the first; tests)
        let attrs = match std::env::var("GUNMETAL_MONITOR").ok().and_then(|v| v.parse::<usize>().ok()).and_then(|i| el.available_monitors().nth(i)) {
            Some(m) => attrs.with_position(PhysicalPosition::new(m.position().x + 50, m.position().y + 50)),
            None => attrs,
        };
        let win = el.create_window(attrs).expect("cannot open the window");
        // fullscreen once the window is where Windows put it (made fullscreen right away, it
        // got the main display's size and could still be moved to another one)
        if super::display::fullscreen() {
            win.set_fullscreen(Some(borderless(&win)));
        }
        // to the front, while our start window still has the foreground (Windows lets only the
        // foreground's process bring a window forward)
        win.focus_window();
        *HWND.lock().unwrap() = hwnd_of(&win);
        self.window = Some(win);
    }

    /// Fullscreen but not filling the display: Windows gave the window its first size and place
    /// after it went fullscreen (the start position on another display, from the terminal that
    /// started us). Fullscreen again, from a window (winit leaves the same fullscreen as it is).
    fn keep_fullscreen(&mut self) {
        let Some(w) = &self.window else { return };
        let Some(m) = w.current_monitor() else { return };
        let (pos, size) = (w.outer_position().ok(), w.inner_size());
        if w.fullscreen().is_some() && (size != m.size() || pos != Some(m.position())) && self.refits < 8 {
            self.refits += 1;
            w.set_fullscreen(None);
            w.set_fullscreen(Some(borderless(w)));
        }
    }

    /// The cursor in client pixels of the game's size (the picture fills the window).
    fn game_pos(&self) -> (i32, i32) {
        let Some(win) = &self.window else { return (0, 0) };
        let size = win.inner_size();
        let (w, h) = self.game_size;
        let x = (self.cursor.x * w as f64 / size.width.max(1) as f64).clamp(0.0, w as f64 - 1.0);
        let y = (self.cursor.y * h as f64 / size.height.max(1) as f64).clamp(0.0, h as f64 - 1.0);
        (x as i32, y as i32)
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        self.create(el);
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        self.create(el);
    }

    fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, e: WindowEvent) {
        match e {
            WindowEvent::CloseRequested => self.input.push_back(Input::Close),
            WindowEvent::Focused(f) => self.input.push_back(Input::Focus(f)),
            WindowEvent::Resized(_) => self.keep_fullscreen(),
            // moved to another display while fullscreen: fill that one
            WindowEvent::Moved(_) => self.keep_fullscreen(),
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = position;
                let (x, y) = self.game_pos();
                self.input.push_back(Input::Move(x, y));
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let lines = match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => y.round() as i32,
                    winit::event::MouseScrollDelta::PixelDelta(p) => (p.y / 40.0).round() as i32,
                };
                if lines != 0 {
                    self.input.push_back(Input::Wheel(lines));
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let b = match button {
                    MouseButton::Left => 0,
                    MouseButton::Right => 1,
                    MouseButton::Middle => 2,
                    _ => return,
                };
                self.input.push_back(Input::Button(b, state == ElementState::Pressed));
            }
            WindowEvent::ModifiersChanged(m) => self.alt = m.state().alt_key(),
            WindowEvent::KeyboardInput { event, .. } => {
                let down = event.state == ElementState::Pressed;
                if self.alt && matches!(event.physical_key, PhysicalKey::Code(KeyCode::Enter | KeyCode::NumpadEnter)) {
                    // Alt+Enter: borderless fullscreen on the window's display, or back
                    if down && !event.repeat {
                        if let Some(w) = &self.window {
                            let full = w.fullscreen().is_some();
                            w.set_fullscreen((!full).then(|| borderless(w)));
                            super::display::remember_fullscreen(!full);
                        }
                    }
                    return;
                }
                if let PhysicalKey::Code(code) = event.physical_key {
                    if let Some((vk, scan)) = vk_of(code) {
                        self.input.push_back(Input::Key { vk, scan, down, repeat: event.repeat });
                    }
                }
                if down {
                    if let Some(t) = &event.text {
                        for ch in t.chars().filter(|&c| (c as u32) < 256) {
                            self.input.push_back(Input::Char(ch as u8));
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// Windows virtual-key code and scan code (bit 7: extended, so the DIK code) for a key.
pub fn vk_of(k: KeyCode) -> Option<(u8, u8)> {
    use KeyCode::*;
    const LETTER_SCAN: [u8; 26] =
        [0x1e, 0x30, 0x2e, 0x20, 0x12, 0x21, 0x22, 0x23, 0x17, 0x24, 0x25, 0x26, 0x32, 0x31, 0x18, 0x19, 0x10, 0x13, 0x1f, 0x14, 0x16, 0x2f, 0x11, 0x2d, 0x15, 0x2c];
    if let Some(i) = LETTERS.iter().position(|&l| l == k) {
        return Some((b'A' + i as u8, LETTER_SCAN[i]));
    }
    if let Some(i) = DIGITS.iter().position(|&d| d == k) {
        return Some((b'0' + i as u8, if i == 0 { 0x0b } else { 1 + i as u8 }));
    }
    const PAD_SCAN: [u8; 10] = [0x52, 0x4f, 0x50, 0x51, 0x4b, 0x4c, 0x4d, 0x47, 0x48, 0x49];
    if let Some(i) = PAD.iter().position(|&d| d == k) {
        return Some((0x60 + i as u8, PAD_SCAN[i]));
    }
    if let Some(i) = FN.iter().position(|&d| d == k) {
        return Some((0x70 + i as u8, if i < 10 { 0x3b + i as u8 } else { 0x57 + (i as u8 - 10) }));
    }
    Some(match k {
        Escape => (0x1b, 0x01),
        Enter => (0x0d, 0x1c),
        NumpadEnter => (0x0d, 0x9c),
        Space => (0x20, 0x39),
        Tab => (0x09, 0x0f),
        Backspace => (0x08, 0x0e),
        ShiftLeft => (0x10, 0x2a),
        ShiftRight => (0x10, 0x36),
        ControlLeft => (0x11, 0x1d),
        ControlRight => (0x11, 0x9d),
        AltLeft => (0x12, 0x38),
        AltRight => (0x12, 0xb8),
        ArrowLeft => (0x25, 0xcb),
        ArrowUp => (0x26, 0xc8),
        ArrowRight => (0x27, 0xcd),
        ArrowDown => (0x28, 0xd0),
        Insert => (0x2d, 0xd2),
        Delete => (0x2e, 0xd3),
        Home => (0x24, 0xc7),
        End => (0x23, 0xcf),
        PageUp => (0x21, 0xc9),
        PageDown => (0x22, 0xd1),
        NumpadAdd => (0x6b, 0x4e),
        NumpadSubtract => (0x6d, 0x4a),
        NumpadMultiply => (0x6a, 0x37),
        NumpadDivide => (0x6f, 0xb5),
        NumpadDecimal => (0x6e, 0x53),
        Minus => (0xbd, 0x0c),
        Equal => (0xbb, 0x0d),
        Comma => (0xbc, 0x33),
        Period => (0xbe, 0x34),
        Slash => (0xbf, 0x35),
        Semicolon => (0xba, 0x27),
        Quote => (0xde, 0x28),
        BracketLeft => (0xdb, 0x1a),
        BracketRight => (0xdd, 0x1b),
        Backslash => (0xdc, 0x2b),
        Backquote => (0xc0, 0x29),
        Pause => (0x13, 0x45),
        _ => return None,
    })
}

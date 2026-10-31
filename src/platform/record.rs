//! `GUNMETAL_RECORD=file.txt`: the player's input is written there as `GUNMETAL_INPUT` items, one
//! a line, to be played again with `GUNMETAL_INPUT=@file.txt` (as rustschweine's HOGS_RECORD).
//!
//! Outside missions the items are at ms since start; in a mission at mission ticks (`t<N>:`,
//! durations in ticks), which a replay delivers at the same point of the game's logic however
//! long the loading took. Recorded: keys with how long they were held, mouse buttons the same way
//! (`LButton*n`, `RButton`, `MButton`, `XButton1`, `XButton2`), the mouse's raw movement
//! (`motion@dx,dy`, summed per tick, or per 50 ms outside missions), the wheel (`wheel@n`), the
//! window's focus and its close button. Not typed characters (the game reads none).
//!
//! F11 marks a moment: a comment line with the time and tick, and the picture shown is written to
//! `file.txt.<ms>.png`. F11 does not reach the game while recording.

use super::log;
use super::window::Input;
use std::io::Write;
use std::sync::Mutex;

/// When an item happens: ms since start, or a mission tick.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum At {
    Ms(u64),
    Tick(u32),
}

impl At {
    fn item(self, what: &str) -> String {
        match self {
            At::Ms(t) => format!("{t}:{what}"),
            At::Tick(t) => format!("t{t}:{what}"),
        }
    }

    /// How long from `self` to `end`, in `self`'s unit (ticks; else ms); at least 1.
    fn until(self, end: At) -> u64 {
        match (self, end) {
            (At::Tick(a), At::Tick(b)) => (b.saturating_sub(a) as u64).max(1),
            (At::Ms(a), At::Ms(b)) => b.saturating_sub(a).max(1),
            // pressed before a mission and let go in it (or the other way): a short hold
            (At::Tick(_), At::Ms(_)) => 1,
            (At::Ms(_), At::Tick(_)) => 100,
        }
    }

    /// The bucket raw motion is summed in.
    fn bucket(self) -> At {
        match self {
            At::Ms(t) => At::Ms(t / 50 * 50),
            t => t,
        }
    }
}

/// The items for the input events, in order (pure: what `record` writes).
#[derive(Default)]
pub struct Recorder {
    /// keys and buttons held: their name and since when
    held: Vec<(String, At)>,
    /// raw motion not written yet: its bucket and sum
    motion: Option<(At, i32, i32)>,
}

impl Recorder {
    /// The lines for one input event at `at`.
    pub fn event(&mut self, i: &Input, at: At) -> Vec<String> {
        let mut out = Vec::new();
        if let Input::Motion(dx, dy) = *i {
            let b = at.bucket();
            match &mut self.motion {
                Some((m, x, y)) if *m == b => {
                    *x += dx;
                    *y += dy;
                }
                _ => {
                    out.extend(self.flush());
                    self.motion = Some((b, dx, dy));
                }
            }
            return out;
        }
        // movement first: it happened before this event
        out.extend(self.flush());
        let pressed = |name: String, down: bool, held: &mut Vec<(String, At)>, out: &mut Vec<String>| {
            let k = held.iter().position(|h| h.0 == name);
            match (down, k) {
                (true, None) => held.push((name, at)),
                (false, Some(k)) => {
                    let (name, since) = held.remove(k);
                    out.push(since.item(&format!("{name}*{}", since.until(at))));
                }
                _ => {}
            }
        };
        match *i {
            Input::Key { vk, down, repeat: false, .. } => {
                if let Some(name) = key_name(vk) {
                    pressed(name.to_string(), down, &mut self.held, &mut out);
                }
            }
            Input::Button(b, down) => {
                if let Some(name) = ["LButton", "RButton", "MButton", "XButton1", "XButton2"].get(b as usize) {
                    pressed(name.to_string(), down, &mut self.held, &mut out);
                }
            }
            Input::Wheel(n) => out.push(at.item(&format!("wheel@{n}"))),
            Input::Focus(f) => out.push(at.item(if f { "focus" } else { "blur" })),
            Input::Close => out.push(at.item("close")),
            _ => {}
        }
        out
    }

    /// The motion summed so far, as an item.
    pub fn flush(&mut self) -> Option<String> {
        let (at, x, y) = self.motion.take()?;
        ((x, y) != (0, 0)).then(|| at.item(&format!("motion@{x},{y}")))
    }
}

/// The script's name of a Windows virtual-key code (`window::key_by_name` reads it back).
pub fn key_name(vk: u8) -> Option<String> {
    Some(match vk {
        b'A'..=b'Z' | b'0'..=b'9' => (vk as char).to_string(),
        0x60..=0x69 => format!("Numpad{}", vk - 0x60),
        0x70..=0x7b => format!("F{}", vk - 0x6f),
        0x0d => "Enter".into(),
        0x1b => "Escape".into(),
        0x20 => "Space".into(),
        0x09 => "Tab".into(),
        0x26 => "Up".into(),
        0x28 => "Down".into(),
        0x25 => "Left".into(),
        0x27 => "Right".into(),
        0x11 | 0xa2 => "Ctrl".into(),
        0x12 | 0xa4 => "Alt".into(),
        0x10 | 0xa0 => "Shift".into(),
        0x08 => "Backspace".into(),
        _ => return None,
    })
}

struct File {
    file: std::fs::File,
    path: String,
    rec: Recorder,
}

static RECORD: std::sync::LazyLock<Mutex<Option<File>>> = std::sync::LazyLock::new(|| {
    Mutex::new((|| {
        let path = std::env::var("GUNMETAL_RECORD").ok().filter(|p| !p.is_empty())?;
        let file = std::fs::File::create(&path).map_err(|e| log!("GUNMETAL_RECORD: cannot write {path}: {e}")).ok()?;
        log!("GUNMETAL_RECORD: the input goes to {path} (F11: a mark with a picture)");
        Some(File { file, path, rec: Recorder::default() })
    })())
});

/// Recording is on.
pub fn on() -> bool {
    std::env::var_os("GUNMETAL_RECORD").is_some_and(|p| !p.is_empty())
}

fn now() -> At {
    match super::ticks::mission_tick() {
        // the event is seen before the next tick reads the input
        Some(t) => At::Tick(t + 1),
        None => At::Ms(super::kernel32::start_time().elapsed().as_millis() as u64),
    }
}

/// A real input event (before it reaches the game).
pub fn input(i: &Input) {
    let mut r = RECORD.lock().unwrap();
    let Some(f) = r.as_mut() else { return };
    let at = now();
    let lines = f.rec.event(i, at);
    for l in lines {
        let _ = writeln!(f.file, "{l}");
    }
    if matches!(i, Input::Close) {
        let _ = f.file.flush();
    }
}

/// F11: a mark (a comment) and a picture of what is shown.
pub fn mark() {
    let mut r = RECORD.lock().unwrap();
    let Some(f) = r.as_mut() else { return };
    let ms = super::kernel32::start_time().elapsed().as_millis();
    let tick = super::ticks::mission_tick().map_or("no mission".to_string(), |t| format!("mission tick {t}"));
    if let Some(l) = f.rec.flush() {
        let _ = writeln!(f.file, "{l}");
    }
    let _ = writeln!(f.file, "# mark {ms} ms, {tick}");
    let _ = f.file.flush();
    let shot = format!("{}.{ms:06}.png", f.path);
    log!("GUNMETAL_RECORD: mark at {ms} ms, {tick}: {shot}");
    super::d3d9::shot::request(shot);
}

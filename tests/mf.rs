//! Media Foundation decoding (`platform::mf`). The WAV test writes its own file; the WMA test
//! uses the game's music and skips without game files.
#![cfg(windows)]

use rust_metal::platform::mf;
use std::path::Path;

/// A canonical 16-bit PCM WAV file.
fn wav(rate: u32, channels: u16, samples: &[i16]) -> Vec<u8> {
    let data: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
    let mut w = Vec::new();
    w.extend(b"RIFF");
    w.extend((36 + data.len() as u32).to_le_bytes());
    w.extend(b"WAVEfmt ");
    w.extend(16u32.to_le_bytes());
    w.extend(1u16.to_le_bytes());
    w.extend(channels.to_le_bytes());
    w.extend(rate.to_le_bytes());
    w.extend((rate * 2 * channels as u32).to_le_bytes());
    w.extend((2 * channels).to_le_bytes());
    w.extend(16u16.to_le_bytes());
    w.extend(b"data");
    w.extend((data.len() as u32).to_le_bytes());
    w.extend(data);
    w
}

#[test]
fn decodes_wav() {
    let ramp: Vec<i16> = (0..800).map(|k| (k * 40 - 16000) as i16).collect();
    let path = std::env::temp_dir().join(format!("rust_metal_mf_{}.wav", std::process::id()));
    std::fs::write(&path, wav(8000, 1, &ramp)).unwrap();
    let pcm = mf::decode(&path);
    std::fs::remove_file(&path).ok();
    let pcm = pcm.unwrap();
    assert_eq!((pcm.rate, pcm.channels), (8000, 1));
    assert_eq!(pcm.samples, ramp);
}

#[test]
fn decodes_game_wma() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("game_files/data/Music/mission_01.wma");
    if !path.exists() {
        eprintln!("skipped: no {}", path.display());
        return;
    }
    let pcm = mf::decode(&path).unwrap();
    assert!(pcm.rate > 0);
    assert!(pcm.samples.len() > pcm.rate as usize, "{} samples at {} Hz", pcm.samples.len(), pcm.rate);
}

#[test]
fn missing_file() {
    assert!(mf::decode(Path::new("nope.wma")).is_err());
}

#[test]
fn gain_from_hundredths_of_db() {
    use rust_metal::platform::music::gain;
    assert_eq!(gain(0), 1.0);
    assert_eq!(gain(-10000), 0.0);
    assert!((gain(-2000) - 0.1).abs() < 1e-6);
}

/// The music player: Run plays to the end, EC_COMPLETE waits and sets the graph's event; a
/// seek to 0 plays it again (the game's loop), also a seek while paused. Skips without a sound
/// device (no player is made then).
#[test]
fn player_ends_and_loops() {
    use rust_metal::platform::dshow::Events;
    use rust_metal::platform::{init_memory, kernel32, music::Player};
    init_memory();
    let ramp: Vec<i16> = (0..800).map(|k| (k * 40 - 16000) as i16).collect();
    let path = std::env::temp_dir().join(format!("rust_metal_player_{}.wav", std::process::id()));
    std::fs::write(&path, wav(8000, 1, &ramp)).unwrap();
    let h = Events::default().handle();
    let Some(p) = Player::open(path.clone(), h, -600) else {
        std::fs::remove_file(&path).ok();
        eprintln!("skipped: no sound device");
        return;
    };
    let ends = |p: &Player| {
        let t = std::time::Instant::now();
        while kernel32::wait(&[h], false, 0) != 0 {
            assert!(t.elapsed().as_secs() < 5, "no EC_COMPLETE");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(p.take_complete());
        assert!(!p.take_complete());
        kernel32::set_event_state(h, false);
    };
    // nothing before Run
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert_eq!(kernel32::wait(&[h], false, 0), 0x102);
    p.run();
    ends(&p);
    assert!(p.ended());
    p.seek(0);
    assert!(!p.ended());
    ends(&p);
    // paused, a seek resets the device; the next Run must still play to the end
    p.seek(0);
    p.run();
    std::thread::sleep(std::time::Duration::from_millis(30));
    p.pause();
    p.seek(0);
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert_eq!(kernel32::wait(&[h], false, 0), 0x102, "paused: no EC_COMPLETE");
    p.run();
    ends(&p);
    drop(p);
    std::fs::remove_file(&path).ok();
}

/// A file that does not open gives no player: the graph keeps it silent, ended at once (one
/// EC_COMPLETE), instead of a player that would end again after every seek.
#[test]
fn no_player_for_a_missing_file() {
    use rust_metal::platform::music::Player;
    assert!(Player::open("nope.wma".into(), 0, 0).is_none());
}

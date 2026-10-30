//! The frame log's summary maths (`frames::stats`, `frames::summary`) on fixed inputs. Needs no
//! game files.

use rust_metal::platform::frames::{Stats, stats, summary};

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

#[test]
fn average_percentile_and_maximum() {
    let v: Vec<f64> = (1..=100).map(f64::from).collect();
    let s = stats(&v);
    assert_eq!(s.n, 100);
    assert!(close(s.avg, 50.5));
    // nearest rank: the 99th of 100 values
    assert!(close(s.p99, 99.0));
    assert!(close(s.max, 100.0));
    // order does not matter
    let mut r = v.clone();
    r.reverse();
    assert_eq!(stats(&r), s);
}

#[test]
fn percentile_of_few_values_is_the_largest() {
    let s = stats(&[5.0, 1.0, 3.0]);
    assert!(close(s.p99, 5.0) && close(s.max, 5.0) && close(s.avg, 3.0));
    let s = stats(&[7.5]);
    assert!(close(s.p99, 7.5) && close(s.avg, 7.5));
    assert_eq!(stats(&[]), Stats::default());
    // 200 values: rank 198
    let v: Vec<f64> = (1..=200).map(f64::from).collect();
    assert!(close(stats(&v).p99, 198.0));
}

#[test]
fn split_by_ticks() {
    // 5 ms pictures, every fifth one with a tick taking 12 ms; 2 ticks once
    let mut p: Vec<(f64, u32)> = Vec::new();
    for k in 0..100 {
        p.push(if k % 5 == 4 { (12.0, 1 + (k == 99) as u32) } else { (5.0, 0) });
    }
    let s = summary(&p, 0.64);
    assert_eq!((s.all.n, s.tick.n, s.no_tick.n), (100, 20, 80));
    assert!(close(s.tick.avg, 12.0) && close(s.tick.max, 12.0) && close(s.tick.p99, 12.0));
    assert!(close(s.no_tick.avg, 5.0) && close(s.no_tick.p99, 5.0));
    assert!(close(s.all.avg, 6.4));
    assert!(close(s.all.p99, 12.0) && close(s.all.max, 12.0));
    assert!(close(s.fps, 100.0 / 0.64));
    // no time: no rate
    assert!(close(summary(&p, 0.0).fps, 0.0));
}

#[test]
fn percentile_rank_in_integers() {
    // nearest rank ceil(0.99 n): 101 values -> the 100th, 1000 -> the 990th, 99 -> the 99th
    for (n, rank) in [(101, 100.0), (1000, 990.0), (99, 99.0), (150, 149.0)] {
        let v: Vec<f64> = (1..=n).map(f64::from).collect();
        assert!(close(stats(&v).p99, rank), "{n} values");
    }
}

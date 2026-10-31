//! Publishes moving test levels for a device id, to look at the editor's
//! meters without audio: `fake_levels <device id> <seconds> <in counts> / <busses> / <out counts>`,
//! e.g. `fake_levels desktop 10 2 2 / 10 / 2 14`.
use std::time::{Duration, Instant};

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let (id, secs) = (&a[0], a[1].parse::<f64>().unwrap());
    let parts: Vec<Vec<u32>> = a[2..].split(|s| s == "/").map(|p| p.iter().map(|s| s.parse().unwrap()).collect()).collect();
    let shape = lc_meter::Shape { inputs: parts[0].clone(), busses: parts[1][0], outputs: parts[2].clone() };
    let levels = lc_meter::Levels::create(&lc_meter::path(id)).unwrap();
    levels.set_shape(&shape);
    let start = Instant::now();
    while start.elapsed().as_secs_f64() < secs {
        let t = start.elapsed().as_secs_f64();
        for i in 0..shape.values() {
            // −60…+1 dBFS, each channel on its own slow sweep.
            let db = -30.0 + 31.0 * (t * (0.7 + 0.13 * i as f64) + i as f64).sin();
            levels.raise(i, 10f64.powf(db / 20.0) as f32);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

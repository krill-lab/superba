//! Frames of the ported session migration scene, composited on the CPU (no
//! texts), for comparing with the SVG played by a browser at the same times.
//!
//!     cargo run -p superba-gui --example movers_frames -- OUT_DIR DENSITY T1 [T2 ...]
//!
//! Writes `OUT_DIR/port-<t>.png` for each time `t` in seconds (`still` for the
//! reduced-motion frame), `DENSITY` pixels per scene unit (1 = 1440 x 900).

use std::path::PathBuf;

use superba_gui::movers::{pose::Pose, render};

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(out), Some(density)) = (args.next(), args.next()) else {
        eprintln!("usage: movers_frames OUT_DIR DENSITY T1 [T2 ...]");
        std::process::exit(2);
    };
    let out = PathBuf::from(out);
    let density: f32 = density.parse().expect("density");
    std::fs::create_dir_all(&out).expect("create the output directory");
    for t in args {
        let pose = if t == "still" {
            Pose::still()
        } else {
            Pose::at(t.parse::<f64>().expect("time in seconds"))
        };
        let started = std::time::Instant::now();
        let pixmap = render::composite(&pose, density).expect("render the scene");
        let took = started.elapsed();
        let path = out.join(format!("port-{t}.png"));
        pixmap.save_png(&path).expect("write the frame");
        println!("{} ({} ms)", path.display(), took.as_millis());
    }
}

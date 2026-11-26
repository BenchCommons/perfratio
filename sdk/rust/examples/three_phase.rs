//! Three phases of sleep; only the middle one is the region "middle".
//!
//! Run: perfratio --region=middle ./three_phase   (expected region time: 0.3 s)
//! Modes: three_phase [exit|panic]
//!   exit   leaves the region open and calls std::process::exit(3) (flush at exit)
//!   panic  panics inside the region (the guard stops it while unwinding)
use std::thread::sleep;
use std::time::Duration;

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_default();
    sleep(Duration::from_millis(100));
    {
        let _region = perfratio_region::region("middle");
        sleep(Duration::from_millis(300));
        match mode.as_str() {
            "exit" => std::process::exit(3),
            "panic" => panic!("boom"),
            _ => {}
        }
    }
    sleep(Duration::from_millis(100));
}

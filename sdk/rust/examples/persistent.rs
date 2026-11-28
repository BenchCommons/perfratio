//! Persistent mode: work that is slow in its first iterations and fast afterwards.
//!
//! Run: perfratio --persistent -w 0 -r 40 ./persistent   (without perfratio it runs once)
use std::thread::sleep;
use std::time::Duration;

fn main() {
    let mut calls = 0;
    perfratio_region::serve(|| {
        calls += 1;
        // a cold start that settles after 5 calls, like a JIT-compiled function
        sleep(Duration::from_millis(if calls <= 5 { 40 } else { 10 }));
    });
}

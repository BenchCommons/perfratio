#![no_main]
//! Feeds arbitrary bytes to the `--import-json` parser and validator.
//! Errors are expected; panics and hangs are bugs.

use libfuzzer_sys::fuzz_target;
use std::io::Write;

fuzz_target!(|data: &[u8]| {
    let mut file = tempfile::NamedTempFile::new().expect("temp file");
    file.write_all(data).expect("write input");
    file.flush().expect("flush input");
    let path = file.path().to_str().expect("utf-8 path").to_owned();
    let _ = perfratio::import::import_json(&path);
});

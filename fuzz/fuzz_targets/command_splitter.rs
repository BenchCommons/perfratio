#![no_main]
//! Feeds arbitrary strings to the command-line splitter used for
//! `--shell=none` and to the `--shell` option parser.
//! Errors are expected; panics are bugs.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &str| {
    let _ = perfratio::command::Command::new(None, data).get_command();
    let _ = perfratio::options::Shell::parse_from_str(data);
});

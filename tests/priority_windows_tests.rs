//! `--priority` read-back on Windows (#414): the binary reads the class back
//! with `GetPriorityClass` and warns when Windows applied a different one.

#![cfg(windows)]

mod common;
use common::perfratio;

use std::io::Write;

use predicates::prelude::*;

/// Whether this process runs with an elevated (administrator) token.
fn is_elevated() -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::Security::{
        GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: the pseudo handle of the current process is always valid and
    // `token` is a valid out-pointer.
    let opened = unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) };
    assert_ne!(
        opened,
        0,
        "OpenProcessToken: {}",
        std::io::Error::last_os_error()
    );
    let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
    let mut returned = 0u32;
    // SAFETY: `token` was opened with TOKEN_QUERY; the buffer is a
    // TOKEN_ELEVATION and its size is passed.
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            (&mut elevation as *mut TOKEN_ELEVATION).cast(),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        )
    };
    let error = std::io::Error::last_os_error();
    // SAFETY: `token` is a handle we opened above and close exactly once.
    unsafe { CloseHandle(token) };
    assert_ne!(ok, 0, "GetTokenInformation: {error}");
    elevation.TokenIsElevated != 0
}

/// Evidence line for the CI log. Written straight to the stderr handle because
/// libtest hides `eprintln!` output of passing tests.
fn evidence(line: &str) {
    let _ = writeln!(std::io::stderr(), "[priority-evidence] {line}");
}

fn run(priority: &str) -> assert_cmd::assert::Assert {
    perfratio()
        .args(["-N", "--runs=1", "--style=basic", "--priority", priority])
        .arg("cmd /C exit 0")
        .assert()
        .success()
}

fn stderr_of(assert: &assert_cmd::assert::Assert) -> String {
    String::from_utf8_lossy(&assert.get_output().stderr).into_owned()
}

#[test]
fn high_is_applied_without_warning() {
    let assert = run("high");
    let stderr = stderr_of(&assert);
    evidence(&format!(
        "high: elevated={} stderr={stderr:?}",
        is_elevated()
    ));
    assert.stderr(
        predicate::str::contains("Windows applied")
            .not()
            .and(predicate::str::contains("read back").not()),
    );
}

#[test]
fn realtime_is_applied_when_elevated_or_warns_about_the_downgrade() {
    let elevated = is_elevated();
    let assert = run("realtime");
    let stderr = stderr_of(&assert);
    evidence(&format!("realtime: elevated={elevated} stderr={stderr:?}"));
    if elevated {
        assert.stderr(
            predicate::str::contains("Windows applied")
                .not()
                .and(predicate::str::contains("read back").not()),
        );
    } else {
        assert.stderr(
            predicate::str::contains(
                "'--priority realtime' requested REALTIME_PRIORITY_CLASS \
                 but Windows applied HIGH_PRIORITY_CLASS",
            )
            .and(predicate::str::contains("administrator")),
        );
    }
}

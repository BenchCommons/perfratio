use std::process::Command;

pub fn perfratio_raw_command() -> Command {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("perfratio"));
    cmd.current_dir("tests/");
    cmd
}

#[allow(dead_code)]
pub fn perfratio() -> assert_cmd::Command {
    assert_cmd::Command::from_std(perfratio_raw_command())
}

#[allow(dead_code)]
pub fn is_rosetta() -> bool {
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        let mut ret: libc::c_int = 0;
        let mut size = std::mem::size_of::<libc::c_int>();
        let name = std::ffi::CString::new("sysctl.proc_translated").unwrap();
        // SAFETY: sysctlbyname is a standard safe Darwin API.
        let res = unsafe {
            libc::sysctlbyname(
                name.as_ptr(),
                &mut ret as *mut _ as *mut libc::c_void,
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if res == 0 && ret == 1 {
            return true;
        }
    }
    false
}

/// The example collector plugin (`examples/collector_plugin.rs`). `cargo test` builds it;
/// with `--test <name>` it is built here on first use.
#[allow(dead_code)]
pub fn example_collector() -> std::path::PathBuf {
    static BUILT: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| {
            // <target>/<profile>/perfratio -> <target>/<profile>/examples/collector_plugin
            let bin = assert_cmd::cargo::cargo_bin!("perfratio");
            let path = bin
                .parent()
                .unwrap()
                .join("examples")
                .join(format!("collector_plugin{}", std::env::consts::EXE_SUFFIX));
            if !path.exists() {
                let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
                let mut build = Command::new(cargo);
                build.args(["build", "--example", "collector_plugin"]);
                if !cfg!(debug_assertions) {
                    build.arg("--release");
                }
                let status = build
                    .current_dir(env!("CARGO_MANIFEST_DIR"))
                    .status()
                    .unwrap();
                assert!(status.success(), "could not build the example collector");
            }
            assert!(path.exists(), "{path:?} is missing");
            path
        })
        .clone()
}

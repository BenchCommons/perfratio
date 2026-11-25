//! Deprecated names kept for one release after the rebrand to perfratio.

use std::ffi::OsString;

/// Deprecated aliases of `PERFRATIO_ITERATION`, still exported to child processes.
pub const LEGACY_ITERATION_VARS: [&str; 2] = ["JOULEX_ITERATION", "HYPERFINE_ITERATION"];

/// Whether `text` expands the variable `name`: `$NAME`, `${NAME}` or `%NAME%`.
/// A longer name that merely starts with `name` (`$NAME_2`) or a bare mention
/// (a file called `NAME.txt`) is not a reference.
fn references_var(text: &str, name: &str) -> bool {
    if text.contains(&format!("${{{name}}}")) || text.contains(&format!("%{name}%")) {
        return true;
    }
    let needle = format!("${name}");
    text.match_indices(&needle).any(|(i, _)| {
        !text[i + needle.len()..]
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// Names of deprecated iteration variables that occur in the command line.
fn used_legacy_iteration_vars<I: IntoIterator<Item = OsString>>(args: I) -> Vec<&'static str> {
    let args: Vec<String> = args
        .into_iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    LEGACY_ITERATION_VARS
        .into_iter()
        .filter(|var| args.iter().any(|a| references_var(a, var)))
        .collect()
}

/// Print one deprecation notice if the command line refers to a legacy
/// iteration variable (in a command, `--prepare`, `--conclude`, ...).
pub fn warn_legacy_iteration_vars<I: IntoIterator<Item = OsString>>(args: I) {
    let used = used_legacy_iteration_vars(args);
    if !used.is_empty() {
        eprintln!(
            "Warning: ${} {} deprecated and will be removed in the next release; use $PERFRATIO_ITERATION.",
            used.join(", $"),
            if used.len() == 1 { "is" } else { "are" }
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn detects_legacy_iteration_vars() {
        assert_eq!(
            used_legacy_iteration_vars(os(&["--prepare", "echo $JOULEX_ITERATION"])),
            ["JOULEX_ITERATION"]
        );
        assert_eq!(
            used_legacy_iteration_vars(os(&["%HYPERFINE_ITERATION%", "$JOULEX_ITERATION"])),
            ["JOULEX_ITERATION", "HYPERFINE_ITERATION"]
        );
        assert!(used_legacy_iteration_vars(os(&["echo $PERFRATIO_ITERATION"])).is_empty());
        assert_eq!(
            used_legacy_iteration_vars(os(&["echo ${HYPERFINE_ITERATION}x"])),
            ["HYPERFINE_ITERATION"]
        );
    }

    #[test]
    fn ignores_mentions_that_are_not_references() {
        for arg in [
            "JOULEX_ITERATION.txt",
            "cat HYPERFINE_ITERATION",
            "--export-json=$JOULEX_ITERATION_2.json",
            "--help",
            "grep JOULEX_ITERATION file",
        ] {
            assert!(used_legacy_iteration_vars(os(&[arg])).is_empty(), "{arg}");
        }
    }
}

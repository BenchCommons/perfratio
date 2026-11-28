//! Raw PMU events and the event registry of `--counters` and `--list-events`
//! (#377).
//!
//! Syntax accepted next to the built-in event names:
//!
//! * `raw:r01c2` or `r01c2`: `PERF_TYPE_RAW` with the hexadecimal config
//!   `0x01c2`, like `perf stat -e r01c2`.
//! * `cpu/event=0x3c,umask=0x01/`: the terms are placed into the config words
//!   with the bit ranges of `<root>/<pmu>/format/<term>`, and the perf type is
//!   `<root>/<pmu>/type`. `config`, `config1` and `config2` set a word
//!   directly.
//! * `cpu/mem-loads/` or the bare `mem-loads`: a named event, defined by
//!   `<root>/<pmu>/events/<name>`. A bare name is looked up in `cpu`, then
//!   `cpu_core`.
//!
//! Everything here is pure logic over a sysfs root (`/sys/bus/event_source/devices`
//! by default), so it is tested with a fake tree on every platform. The kernel
//! interface is documented in `perf_event_open(2)` and
//! `Documentation/ABI/testing/sysfs-bus-event_source-devices-*`.

use std::fs;
use std::path::Path;
use std::sync::Mutex;

/// Where the kernel lists the PMUs.
pub const SYSFS_DEVICES: &str = "/sys/bus/event_source/devices";

/// PMUs a bare event name is looked up in, in order.
const CORE_PMUS: [&str; 2] = ["cpu", "cpu_core"];

/// `PERF_TYPE_RAW`
pub const PERF_TYPE_RAW: u32 = 4;

/// A fully resolved perf event: what to put in `perf_event_attr`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RawEvent {
    /// JSON key and terminal name, without a scope suffix
    pub name: &'static str,
    pub perf_type: u32,
    pub config: u64,
    pub config1: u64,
    pub config2: u64,
}

/// The name of a [`RawEvent`] lives as long as the process, which keeps the
/// event `Copy`. A run creates a handful of them.
fn intern(name: &str) -> &'static str {
    static NAMES: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
    let mut names = NAMES.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(&known) = names.iter().find(|&&n| n == name) {
        return known;
    }
    let leaked: &'static str = Box::leak(name.to_string().into_boxed_str());
    names.push(leaked);
    leaked
}

/// A PMU or event name from the command line becomes a path component, so it
/// is restricted to what the kernel uses.
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        && name != "."
        && name != ".."
}

/// Hexadecimal (`0x` optional) or decimal number.
fn parse_number(text: &str) -> Result<u64, String> {
    let t = text.trim();
    let parsed = match t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        Some(hex) => u64::from_str_radix(hex, 16),
        None => t.parse::<u64>(),
    };
    parsed.map_err(|_| format!("'{text}' is not a number"))
}

/// `r01c2` (lowercase or uppercase hex digits).
fn parse_raw_hex(text: &str) -> Option<u64> {
    let digits = text.strip_prefix('r')?;
    if digits.is_empty() || digits.len() > 16 || !digits.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    u64::from_str_radix(digits, 16).ok()
}

/// `event=0x3c,umask=0x1,inv` as `(term, value)`; a term without a value is
/// a flag and counts as 1.
fn parse_terms(text: &str) -> Result<Vec<(String, u64)>, String> {
    let mut terms = Vec::new();
    for part in text.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (name, value) = match part.split_once('=') {
            Some((n, v)) => (
                n.trim(),
                parse_number(v).map_err(|e| format!("{e} (term '{part}')"))?,
            ),
            None => (part, 1),
        };
        if !valid_name(name) {
            return Err(format!("invalid term '{part}'"));
        }
        terms.push((name.to_string(), value));
    }
    if terms.is_empty() {
        return Err("no event terms".to_string());
    }
    Ok(terms)
}

/// Bit ranges of a `format/<term>` file: `config:0-7`, `config1:0-63`,
/// `config:0-7,32-35`. Returns the config word (0, 1 or 2) and the ranges.
fn parse_format(text: &str) -> Result<(usize, Vec<(u32, u32)>), String> {
    let text = text.trim();
    let (word, ranges) = text
        .split_once(':')
        .ok_or_else(|| format!("unrecognised format '{text}'"))?;
    let word = match word {
        "config" => 0,
        "config1" => 1,
        "config2" => 2,
        other => return Err(format!("unsupported config word '{other}'")),
    };
    let mut out = Vec::new();
    for range in ranges.split(',') {
        let (lo, hi) = match range.split_once('-') {
            Some((l, h)) => (l, h),
            None => (range, range),
        };
        let lo: u32 = lo
            .trim()
            .parse()
            .map_err(|_| format!("bad range '{range}'"))?;
        let hi: u32 = hi
            .trim()
            .parse()
            .map_err(|_| format!("bad range '{range}'"))?;
        if lo > hi || hi > 63 {
            return Err(format!("bad range '{range}'"));
        }
        out.push((lo, hi));
    }
    Ok((word, out))
}

/// Puts `value` into the ranges, least significant bits first. `None` when
/// the value has more bits than the ranges hold.
fn place(value: u64, ranges: &[(u32, u32)]) -> Option<u64> {
    let mut out = 0u64;
    let mut used = 0u32;
    for &(lo, hi) in ranges {
        let width = hi - lo + 1;
        let mask = if width == 64 {
            u64::MAX
        } else {
            (1u64 << width) - 1
        };
        out |= ((value >> used.min(63)) & mask) << lo;
        used += width;
    }
    let fits = used >= 64 || value >> used == 0;
    fits.then_some(out)
}

fn read(root: &Path, relative: &str) -> Option<String> {
    fs::read_to_string(root.join(relative)).ok()
}

/// Resolves the terms of `pmu` against its `type` and `format/` files.
fn resolve_terms(root: &Path, pmu: &str, terms: &[(String, u64)]) -> Result<[u64; 3], String> {
    let mut words = [0u64; 3];
    for (term, value) in terms {
        let (word, bits) = match term.as_str() {
            "config" => (0, None),
            "config1" => (1, None),
            "config2" => (2, None),
            _ => {
                let text = read(root, &format!("{pmu}/format/{term}")).ok_or_else(|| {
                    let known = list_dir(&root.join(pmu).join("format")).join(", ");
                    format!(
                        "PMU '{pmu}' has no term '{term}' (known: {})",
                        if known.is_empty() { "none" } else { &known }
                    )
                })?;
                let (word, ranges) =
                    parse_format(&text).map_err(|e| format!("{pmu}/format/{term}: {e}"))?;
                (word, Some(ranges))
            }
        };
        match bits {
            None => words[word] |= value,
            Some(ranges) => {
                let placed = place(*value, &ranges).ok_or_else(|| {
                    format!(
                        "value {value:#x} of term '{term}' does not fit the bits of PMU '{pmu}'"
                    )
                })?;
                words[word] |= placed;
            }
        }
    }
    Ok(words)
}

fn pmu_type(root: &Path, pmu: &str) -> Result<u32, String> {
    let text = read(root, &format!("{pmu}/type")).ok_or_else(|| {
        format!(
            "PMU '{pmu}' not found under {} (see 'perfratio --list-events')",
            root.display()
        )
    })?;
    text.trim()
        .parse()
        .map_err(|_| format!("{pmu}/type does not hold a number"))
}

/// Resolves `pmu/body/`: `body` is a named event of the PMU or a list of terms.
pub fn resolve_pmu_event(root: &Path, pmu: &str, body: &str) -> Result<RawEvent, String> {
    if !valid_name(pmu) {
        return Err(format!("invalid PMU name '{pmu}'"));
    }
    let body = body.trim();
    let perf_type = pmu_type(root, pmu)?;
    let alias = if valid_name(body) {
        read(root, &format!("{pmu}/events/{body}"))
    } else {
        None
    };
    let terms = match alias {
        Some(definition) => {
            parse_terms(definition.trim()).map_err(|e| format!("{pmu}/events/{body}: {e}"))?
        }
        None => parse_terms(body).map_err(|e| {
            format!("cannot read '{pmu}/{body}/': {e} (not a named event of '{pmu}' either)")
        })?,
    };
    let [config, config1, config2] = resolve_terms(root, pmu, &terms)?;
    let compact: String = body.chars().filter(|c| !c.is_whitespace()).collect();
    Ok(RawEvent {
        name: intern(&format!("{pmu}/{compact}/")),
        perf_type,
        config,
        config1,
        config2,
    })
}

/// A bare event name that is not built in: a named event of the core PMU.
pub fn resolve_bare_name(root: &Path, name: &str) -> Option<RawEvent> {
    if !valid_name(name) {
        return None;
    }
    CORE_PMUS
        .iter()
        .find(|pmu| root.join(pmu).join("events").join(name).is_file())
        .and_then(|pmu| resolve_pmu_event(root, pmu, name).ok())
}

/// A raw or PMU-qualified item of `--counters`, with its `:u`/`:k` modifier.
/// `Ok(None)` when the item is neither (a built-in name).
pub fn parse_item(item: &str, root: &Path) -> Result<Option<(RawEvent, Option<String>)>, String> {
    let explicit = item.strip_prefix("raw:");
    let rest = explicit.unwrap_or(item);
    if let Some(slash) = rest.find('/') {
        let pmu = &rest[..slash];
        let end = rest.rfind('/').filter(|&e| e > slash).ok_or_else(|| {
            format!("'{item}' is missing the closing '/' (expected 'pmu/event=0x3c/')")
        })?;
        let body = &rest[slash + 1..end];
        let tail = &rest[end + 1..];
        // `perf` writes the modifier right after the slash: `cpu/event=0x3c/u`
        let modifier = tail.strip_prefix(':').unwrap_or(tail);
        let modifier = (!modifier.is_empty()).then(|| modifier.to_string());
        return resolve_pmu_event(root, pmu, body).map(|e| Some((e, modifier)));
    }
    let (head, modifier) = match rest.split_once(':') {
        Some((h, m)) => (h, Some(m.to_string())),
        None => (rest, None),
    };
    match parse_raw_hex(&head.to_ascii_lowercase()) {
        Some(config) => Ok(Some((
            RawEvent {
                name: intern(&head.to_ascii_lowercase()),
                perf_type: PERF_TYPE_RAW,
                config,
                config1: 0,
                config2: 0,
            },
            modifier,
        ))),
        None if explicit.is_some() => Err(format!(
            "'{item}' is not a raw event: use 'raw:r' followed by hexadecimal digits, e.g. 'raw:r01c2'"
        )),
        None => Ok(None),
    }
}

/// Splits a `--counters` list at the commas that are not inside `pmu/.../`.
pub fn split_items(list: &str) -> Vec<&str> {
    let mut items = Vec::new();
    let mut start = 0;
    let mut inside = false;
    for (i, c) in list.char_indices() {
        match c {
            '/' => inside = !inside,
            ',' if !inside => {
                items.push(&list[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    items.push(&list[start..]);
    items
}

fn list_dir(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.sort();
    names
}

/// One PMU of `--list-events`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PmuListing {
    pub name: String,
    pub perf_type: Option<u32>,
    /// Has a `cpumask`: an uncore or similar PMU that counts per socket, not per
    /// process, so `perf_event_open` on a benchmarked process refuses it
    pub system_wide: bool,
    /// `(name, definition)`, from `events/`; `.scale`, `.unit` and similar
    /// attribute files are not events
    pub events: Vec<(String, String)>,
    /// `(term, bits)` from `format/`
    pub formats: Vec<(String, String)>,
}

/// Discovers the PMUs under `root`, sorted by name. A missing root gives an
/// empty list.
pub fn discover(root: &Path) -> Vec<PmuListing> {
    list_dir(root)
        .into_iter()
        .filter(|n| valid_name(n))
        .map(|name| {
            let dir = root.join(&name);
            let events = list_dir(&dir.join("events"))
                .into_iter()
                .filter(|e| {
                    !["scale", "unit", "per-pkg", "snapshot"]
                        .iter()
                        .any(|s| e.ends_with(&format!(".{s}")))
                })
                .filter_map(|e| {
                    let text = fs::read_to_string(dir.join("events").join(&e)).ok()?;
                    Some((e, text.trim().to_string()))
                })
                .collect();
            let formats = list_dir(&dir.join("format"))
                .into_iter()
                .filter_map(|f| {
                    let text = fs::read_to_string(dir.join("format").join(&f)).ok()?;
                    Some((f, text.trim().to_string()))
                })
                .collect();
            PmuListing {
                perf_type: read(root, &format!("{name}/type")).and_then(|t| t.trim().parse().ok()),
                system_wide: dir.join("cpumask").is_file(),
                name,
                events,
                formats,
            }
        })
        .collect()
}

/// Text of `perfratio --list-events`.
pub fn render_list(root: &Path) -> String {
    let mut out = String::new();
    out.push_str("Built-in events (--counters=NAME):\n");
    for kind in crate::perf_counters::CounterKind::ALL {
        out.push_str(&format!("  {:<18} hardware\n", kind.name()));
    }
    for kind in crate::perf_counters::CounterKind::SOFTWARE {
        out.push_str(&format!("  {:<18} software\n", kind.name()));
    }
    out.push_str(
        "\nRaw events:\n  raw:rXXXX          PERF_TYPE_RAW with the hexadecimal config, e.g. raw:r01c2\n  \
         pmu/event=0x3c,umask=0x01/   terms are placed with the PMU's format/ bit ranges\n  \
         pmu/NAME/          a named event of the PMU; a bare NAME is looked up in cpu, then cpu_core\n  \
         A ':u' or ':k' modifier limits the scope, as with built-in events.\n",
    );
    let pmus = discover(root);
    if pmus.is_empty() {
        out.push_str(&format!(
            "\nNo PMUs found under {} (only available on Linux).\n",
            root.display()
        ));
        return out;
    }
    for pmu in &pmus {
        if pmu.events.is_empty() && pmu.formats.is_empty() {
            continue;
        }
        out.push_str(&format!(
            "\n{}{}{}:\n",
            pmu.name,
            pmu.perf_type
                .map_or(String::new(), |t| format!(" (type {t})")),
            if pmu.system_wide {
                ", system-wide: cannot be counted per process"
            } else {
                ""
            }
        ));
        if !pmu.formats.is_empty() {
            let terms: Vec<String> = pmu
                .formats
                .iter()
                .map(|(n, bits)| format!("{n}[{bits}]"))
                .collect();
            out.push_str(&format!("  terms: {}\n", terms.join(" ")));
        }
        for (name, definition) in &pmu.events {
            out.push_str(&format!("  {}/{name}/  [{definition}]\n", pmu.name));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake `/sys/bus/event_source/devices` with an Intel-like `cpu` PMU.
    fn fake_sysfs() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let w = |path: &str, text: &str| {
            let p = dir.path().join(path);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, text).unwrap();
        };
        w("cpu/type", "4\n");
        w("cpu/format/event", "config:0-7\n");
        w("cpu/format/umask", "config:8-15\n");
        w("cpu/format/edge", "config:18\n");
        w("cpu/format/cmask", "config:24-31\n");
        w("cpu/format/ldlat", "config1:0-15\n");
        w("cpu/format/split", "config:0-3,32-35\n");
        w("cpu/events/mem-loads", "event=0xcd,umask=0x1,ldlat=3\n");
        w("cpu/events/cache-misses", "event=0x2e,umask=0x41\n");
        w("cpu/events/cache-misses.scale", "1.0\n");
        w("cpu_core/type", "10\n");
        w("cpu_core/format/event", "config:0-7\n");
        w("cpu_core/format/umask", "config:8-15\n");
        w("cpu_core/events/topdown-retiring", "event=0xc2,umask=0x2\n");
        w("uncore_imc_0/type", "14\n");
        w("uncore_imc_0/cpumask", "0\n");
        w("uncore_imc_0/format/event", "config:0-7\n");
        w(
            "uncore_imc_0/events/cas_count_read",
            "event=0x04,umask=0x03\n",
        );
        w("software/type", "1\n");
        dir
    }

    #[test]
    fn raw_hex_config() {
        let root = Path::new("/nonexistent");
        let (e, m) = parse_item("raw:r01c2", root).unwrap().unwrap();
        assert_eq!(
            (e.perf_type, e.config, e.name),
            (PERF_TYPE_RAW, 0x01c2, "r01c2")
        );
        assert_eq!(m, None);
        let (e, m) = parse_item("r01C2:u", root).unwrap().unwrap();
        assert_eq!((e.config, e.name), (0x01c2, "r01c2"));
        assert_eq!(m.as_deref(), Some("u"));
    }

    #[test]
    fn raw_rejects_bad_hex_and_leaves_builtins_alone() {
        let root = Path::new("/nonexistent");
        assert!(parse_item("raw:rxyz", root).is_err());
        assert!(parse_item("raw:01c2", root).is_err());
        assert!(parse_item("raw:r", root).is_err());
        assert!(parse_item(&format!("raw:r{}", "f".repeat(17)), root).is_err());
        assert_eq!(parse_item("instructions", root).unwrap(), None);
        assert_eq!(parse_item("cycles:u", root).unwrap(), None);
        assert_eq!(parse_item("page-faults", root).unwrap(), None);
    }

    #[test]
    fn pmu_terms_use_the_format_bits() {
        let sys = fake_sysfs();
        let (e, m) = parse_item("cpu/event=0x3c,umask=0x01/", sys.path())
            .unwrap()
            .unwrap();
        assert_eq!(e.perf_type, 4);
        assert_eq!(e.config, 0x013c);
        assert_eq!(e.name, "cpu/event=0x3c,umask=0x01/");
        assert_eq!(m, None);
        let (e, _) = parse_item("cpu/event=0x3c,edge,cmask=2/", sys.path())
            .unwrap()
            .unwrap();
        assert_eq!(e.config, 0x3c | 1 << 18 | 2 << 24);
    }

    #[test]
    fn modifiers_follow_the_slash() {
        let sys = fake_sysfs();
        let (_, m) = parse_item("cpu/event=0x3c/u", sys.path()).unwrap().unwrap();
        assert_eq!(m.as_deref(), Some("u"));
        let (_, m) = parse_item("cpu/event=0x3c/:k", sys.path())
            .unwrap()
            .unwrap();
        assert_eq!(m.as_deref(), Some("k"));
    }

    #[test]
    fn split_ranges_and_config1() {
        let sys = fake_sysfs();
        let e = resolve_pmu_event(sys.path(), "cpu", "split=0xab").unwrap();
        // low nibble 0xb into bits 0-3, next nibble 0xa into bits 32-35
        assert_eq!(e.config, 0xb | 0xa << 32);
        let e = resolve_pmu_event(sys.path(), "cpu", "event=1,ldlat=0x30").unwrap();
        assert_eq!((e.config, e.config1), (1, 0x30));
        let e = resolve_pmu_event(sys.path(), "cpu", "config=0x5,config2=0x9").unwrap();
        assert_eq!((e.config, e.config2), (5, 9));
    }

    #[test]
    fn value_too_wide_for_its_field_is_an_error() {
        let sys = fake_sysfs();
        let err = resolve_pmu_event(sys.path(), "cpu", "event=0x1ff").unwrap_err();
        assert!(err.contains("does not fit"), "{err}");
    }

    #[test]
    fn named_events_come_from_the_events_directory() {
        let sys = fake_sysfs();
        let e = resolve_pmu_event(sys.path(), "cpu", "mem-loads").unwrap();
        assert_eq!((e.config, e.config1), (0xcd | 1 << 8, 3));
        assert_eq!(e.name, "cpu/mem-loads/");
        // a bare name that no built-in event has
        let bare = resolve_bare_name(sys.path(), "mem-loads").unwrap();
        assert_eq!(bare, e);
        // falls through to cpu_core
        let core = resolve_bare_name(sys.path(), "topdown-retiring").unwrap();
        assert_eq!((core.perf_type, core.config), (10, 0xc2 | 2 << 8));
        assert_eq!(resolve_bare_name(sys.path(), "nope"), None);
    }

    #[test]
    fn errors_name_the_problem() {
        let sys = fake_sysfs();
        let missing = parse_item("nopmu/event=1/", sys.path()).unwrap_err();
        assert!(missing.contains("PMU 'nopmu' not found"), "{missing}");
        let term = parse_item("cpu/bogus=1/", sys.path()).unwrap_err();
        assert!(
            term.contains("no term 'bogus'") && term.contains("umask"),
            "{term}"
        );
        let open = parse_item("cpu/event=1", sys.path()).unwrap_err();
        assert!(open.contains("closing '/'"), "{open}");
        let value = parse_item("cpu/event=zz/", sys.path()).unwrap_err();
        assert!(value.contains("not a number"), "{value}");
        assert!(parse_item("cpu/event=1/", Path::new("/nonexistent")).is_err());
    }

    #[test]
    fn pmu_names_cannot_leave_the_sysfs_root() {
        let sys = fake_sysfs();
        assert!(resolve_pmu_event(sys.path(), "..", "event=1").is_err());
        assert!(resolve_pmu_event(sys.path(), "cpu/../cpu", "event=1").is_err());
        assert_eq!(resolve_bare_name(sys.path(), "../type"), None);
    }

    #[test]
    fn items_split_outside_slashes_only() {
        assert_eq!(
            split_items("instructions,cpu/event=0x3c,umask=1/u,raw:r1,cycles"),
            [
                "instructions",
                "cpu/event=0x3c,umask=1/u",
                "raw:r1",
                "cycles"
            ]
        );
        assert_eq!(split_items("a"), ["a"]);
    }

    #[test]
    fn discovery_lists_events_and_skips_attribute_files() {
        let sys = fake_sysfs();
        let pmus = discover(sys.path());
        let names: Vec<&str> = pmus.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["cpu", "cpu_core", "software", "uncore_imc_0"]);
        let cpu = &pmus[0];
        assert_eq!(cpu.perf_type, Some(4));
        assert!(!cpu.system_wide);
        let events: Vec<&str> = cpu.events.iter().map(|e| e.0.as_str()).collect();
        assert_eq!(events, ["cache-misses", "mem-loads"]);
        assert!(pmus[3].system_wide);
        assert!(discover(Path::new("/nonexistent")).is_empty());
    }

    #[test]
    fn list_output_shows_builtin_raw_and_discovered_events() {
        let sys = fake_sysfs();
        let text = render_list(sys.path());
        assert!(text.contains("instructions"), "{text}");
        assert!(text.contains("task-clock"), "{text}");
        assert!(text.contains("raw:rXXXX"), "{text}");
        assert!(text.contains("cpu (type 4):"), "{text}");
        assert!(
            text.contains("cpu/mem-loads/  [event=0xcd,umask=0x1,ldlat=3]"),
            "{text}"
        );
        assert!(text.contains("terms: cmask[config:24-31]"), "{text}");
        assert!(
            text.contains("uncore_imc_0 (type 14), system-wide"),
            "{text}"
        );
        assert!(!text.contains(".scale"), "{text}");
        let none = render_list(Path::new("/nonexistent"));
        assert!(none.contains("No PMUs found"), "{none}");
    }
}

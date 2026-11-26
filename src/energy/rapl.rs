use std::fs;
use std::path::{Path, PathBuf};

use super::EnergySampler;

/// Linux Intel/AMD RAPL (Running Average Power Limit) Sampler via sysfs powercap interface.
/// Path: /sys/class/powercap/intel-rapl/intel-rapl:0/energy_uj
pub struct LinuxRaplSampler {
    energy_files: Vec<PathBuf>,
    /// `max_energy_range_uj` per domain (None if unreadable)
    max_energy_ranges: Vec<Option<u64>>,
    /// Readings taken by `start()`, or None if any counter could not be read
    start_readings: Option<Vec<u64>>,
    /// Per-domain breakdown (`--energy-domains`), see `docs/specs/rapl-domains.md`
    groups: Vec<DomainGroup>,
    domains_enabled: bool,
    group_start: Option<Vec<Vec<u64>>>,
    last_domains: Option<Vec<(String, f64)>>,
}

/// Counters that share one reported domain name, e.g. the `dram` zones of all packages.
/// These are only for the breakdown. They are never added to the total, because
/// `core`, `uncore` and `dram` are sub-zones of a package and `psys` overlaps it.
struct DomainGroup {
    name: &'static str,
    zones: Vec<(PathBuf, Option<u64>)>,
}

/// Domain names in the order they are reported.
const DOMAIN_ORDER: [&str; 5] = ["package", "core", "uncore", "dram", "psys"];

fn read_counter(path: &Path) -> Option<u64> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn zone_name(dir: &Path) -> Option<String> {
    fs::read_to_string(dir.join("name"))
        .ok()
        .map(|s| s.trim().to_string())
}

fn canonical_domain(zone_name: &str, is_sub_zone: bool) -> Option<&'static str> {
    match (is_sub_zone, zone_name) {
        (false, n) if n.starts_with("package-") => Some("package"),
        (false, "psys") => Some("psys"),
        (true, "core") => Some("core"),
        (true, "uncore") => Some("uncore"),
        (true, "dram") => Some("dram"),
        _ => None,
    }
}

/// Finds the readable zones of each reported domain under `base`. Zones whose
/// `energy_uj` cannot be read are left out.
fn discover_groups(base: &Path) -> Vec<DomainGroup> {
    let mut found: Vec<(&'static str, PathBuf, Option<u64>)> = Vec::new();
    let mut add = |dir: &Path, is_sub: bool| {
        let Some(name) = zone_name(dir).and_then(|n| canonical_domain(&n, is_sub)) else {
            return;
        };
        let energy = dir.join("energy_uj");
        if read_counter(&energy).is_some() {
            found.push((name, energy, read_counter(&dir.join("max_energy_range_uj"))));
        }
    };
    let mut tops: Vec<PathBuf> = fs::read_dir(base)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| is_zone_dir(p, 1))
        .collect();
    tops.sort();
    for top in tops {
        add(&top, false);
        let mut subs: Vec<PathBuf> = fs::read_dir(&top)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| is_zone_dir(p, 2))
            .collect();
        subs.sort();
        for sub in subs {
            add(&sub, true);
        }
    }
    DOMAIN_ORDER
        .iter()
        .filter_map(|&name| {
            let zones: Vec<_> = found
                .iter()
                .filter(|(n, _, _)| *n == name)
                .map(|(_, p, r)| (p.clone(), *r))
                .collect();
            (!zones.is_empty()).then_some(DomainGroup { name, zones })
        })
        .collect()
}

/// `intel-rapl:N` has one `:`, `intel-rapl:N:M` has two.
fn is_zone_dir(path: &Path, colons: usize) -> bool {
    path.file_name()
        .map(|n| n.to_string_lossy())
        .is_some_and(|n| n.starts_with("intel-rapl:") && n.matches(':').count() == colons)
}

/// The first `energy_uj` that exists but cannot be read, for the permission message.
pub fn first_unreadable(base: &Path) -> Option<PathBuf> {
    let mut tops: Vec<PathBuf> = fs::read_dir(base)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| is_zone_dir(p, 1))
        .collect();
    tops.sort();
    tops.into_iter()
        .map(|p| p.join("energy_uj"))
        .find(|p| p.exists() && read_counter(p).is_none())
}

/// Message for `--energy` when sysfs counters exist but cannot be read (PLATYPUS,
/// CVE-2020-8694, made `energy_uj` root-only).
pub fn permission_message(unreadable: &Path) -> String {
    format!(
        "Cannot read {} (permission denied). Run as root, or grant read access: \
         sudo chmod o+r /sys/class/powercap/intel-rapl*/*energy_uj \
         /sys/class/powercap/intel-rapl*/*/energy_uj (resets on reboot; the README has a udev \
         rule). Granting read access to energy_uj lets any local user infer secrets from the \
         power signal (PLATYPUS), so do it only on machines you control.",
        unreadable.display()
    )
}

struct RaplDomain {
    name: String,
    energy_file: PathBuf,
    max_range: Option<u64>,
}

/// Default location of the powercap RAPL tree.
pub const SYSFS_ROOT: &str = "/sys/class/powercap/intel-rapl";

/// True if any top-level domain has an `energy_uj` file, readable or not.
/// Used to tell `rapl_unreadable` from `rapl_not_found`.
pub fn domains_present(base: &Path) -> bool {
    fs::read_dir(base).into_iter().flatten().flatten().any(|e| {
        let name = e.file_name();
        let name = name.to_string_lossy();
        name.starts_with("intel-rapl:")
            && name.matches(':').count() == 1
            && e.path().join("energy_uj").exists()
    })
}

impl LinuxRaplSampler {
    pub fn try_new() -> Option<Self> {
        Self::try_new_at(Path::new(SYSFS_ROOT))
    }

    pub fn try_new_at(base: &Path) -> Option<Self> {
        if !base.exists() {
            return None;
        }

        let mut domains = Vec::new();

        // Check top-level package domains: intel-rapl:0, intel-rapl:1, etc.
        let entries = fs::read_dir(base).ok()?;
        for entry in entries.flatten() {
            let path = entry.path();
            let file_name = path.file_name()?.to_string_lossy();
            if file_name.starts_with("intel-rapl:") && file_name.matches(':').count() == 1 {
                let energy_uj_path = path.join("energy_uj");
                let max_range_path = path.join("max_energy_range_uj");
                let name_path = path.join("name");

                if energy_uj_path.exists() {
                    // Try to read it to check permissions
                    if let Ok(content) = fs::read_to_string(&energy_uj_path) {
                        if content.trim().parse::<u64>().is_ok() {
                            let max_range = fs::read_to_string(&max_range_path)
                                .ok()
                                .and_then(|s| s.trim().parse::<u64>().ok());

                            let name = fs::read_to_string(&name_path)
                                .map(|s| s.trim().to_string())
                                .unwrap_or_default();

                            domains.push(RaplDomain {
                                name,
                                energy_file: energy_uj_path,
                                max_range,
                            });
                        }
                    }
                }
            }
        }

        if domains.is_empty() {
            return None;
        }

        // Domain selection strategy:
        // 1. If any domain name starts with "package-", sum only package-* domains.
        //    This avoids double-counting platform energy (psys) which overlaps with package energy.
        // 2. If no package-* domain is readable but "psys" is, use "psys" alone.
        // 3. Otherwise, use all available top-level domains.
        let has_packages = domains.iter().any(|d| d.name.starts_with("package-"));
        let selected: Vec<RaplDomain> = if has_packages {
            domains
                .into_iter()
                .filter(|d| d.name.starts_with("package-"))
                .collect()
        } else if domains.iter().any(|d| d.name == "psys") {
            domains.into_iter().filter(|d| d.name == "psys").collect()
        } else {
            domains
        };

        let mut energy_files = Vec::with_capacity(selected.len());
        let mut max_energy_ranges = Vec::with_capacity(selected.len());

        for domain in selected {
            energy_files.push(domain.energy_file);
            max_energy_ranges.push(domain.max_range);
        }

        Some(Self {
            energy_files,
            max_energy_ranges,
            start_readings: None,
            groups: discover_groups(base),
            domains_enabled: false,
            group_start: None,
            last_domains: None,
        })
    }

    /// Read all energy counters, or None if any of them can't be read. A
    /// failed read must not be mistaken for a reading of 0, which would look
    /// like a counter wrap-around and produce a huge bogus energy value.
    fn read_all_uj(&self) -> Option<Vec<u64>> {
        self.energy_files
            .iter()
            .map(|path| {
                fs::read_to_string(path)
                    .ok()
                    .and_then(|s| s.trim().parse::<u64>().ok())
            })
            .collect()
    }
}

/// Energy consumed between two readings of a counter that wraps around at
/// `max_range` (the largest value it can hold, `max_energy_range_uj`).
/// Returns None for a wrap-around when the range is unknown.
fn counter_delta(start: u64, stop: u64, max_range: Option<u64>) -> Option<u64> {
    if stop >= start {
        Some(stop - start)
    } else {
        // The counter went from `start` up to `max_range`, wrapped to 0 and
        // then went up to `stop`: (max_range - start) + 1 + stop.
        let max_range = max_range?;
        Some(
            max_range
                .checked_sub(start)?
                .saturating_add(1)
                .saturating_add(stop),
        )
    }
}

impl LinuxRaplSampler {
    fn read_groups(&self) -> Option<Vec<Vec<u64>>> {
        self.groups
            .iter()
            .map(|g| g.zones.iter().map(|(p, _)| read_counter(p)).collect())
            .collect()
    }

    /// Joules per domain between the two readings. A domain whose counter cannot be
    /// converted (failed read, wrap without a known range) is left out, not reported as 0.
    fn group_joules(&self, start: &[Vec<u64>], stop: &[Vec<u64>]) -> Vec<(String, f64)> {
        let mut out = Vec::new();
        for ((group, s), e) in self.groups.iter().zip(start).zip(stop) {
            let mut total = 0.0;
            let mut ok = true;
            for (((_, range), &a), &b) in group.zones.iter().zip(s).zip(e) {
                match counter_delta(a, b, *range) {
                    Some(d) => total += d as f64,
                    None => ok = false,
                }
            }
            if ok {
                out.push((group.name.to_string(), total / 1_000_000.0));
            }
        }
        out
    }
}

impl EnergySampler for LinuxRaplSampler {
    fn start(&mut self) {
        if self.domains_enabled {
            self.group_start = self.read_groups();
        }
        self.start_readings = self.read_all_uj();
    }

    fn enable_domains(&mut self) {
        self.domains_enabled = true;
    }

    fn domain_energy(&self) -> Option<Vec<(String, f64)>> {
        self.last_domains.clone()
    }

    fn stop(&mut self) -> Option<f64> {
        self.last_domains = None;
        if self.domains_enabled {
            if let (Some(start), Some(stop)) = (self.group_start.take(), self.read_groups()) {
                self.last_domains = Some(self.group_joules(&start, &stop));
            }
        }
        // No sample if either the start or the stop reading failed, rather
        // than reporting a wrong value.
        let start_readings = self.start_readings.take()?;
        let stop_readings = self.read_all_uj()?;
        let mut total_microjoules: f64 = 0.0;

        for ((&start, &stop), &max_range) in start_readings
            .iter()
            .zip(&stop_readings)
            .zip(&self.max_energy_ranges)
        {
            total_microjoules += counter_delta(start, stop, max_range)? as f64;
        }

        // Convert microjoules to Joules (1 J = 1,000,000 uJ)
        Some(total_microjoules / 1_000_000.0)
    }

    fn is_available(&self) -> bool {
        !self.energy_files.is_empty()
    }
}

impl LinuxRaplSampler {
    #[cfg(test)]
    pub fn energy_files(&self) -> &[PathBuf] {
        &self.energy_files
    }
}

#[cfg(all(test, not(windows)))]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn create_fake_rapl_domain(
        base: &Path,
        dir_name: &str,
        domain_name: &str,
        energy_uj: u64,
        max_range_uj: Option<u64>,
    ) -> PathBuf {
        let domain_dir = base.join(dir_name);
        fs::create_dir_all(&domain_dir).unwrap();
        fs::write(domain_dir.join("name"), format!("{domain_name}\n")).unwrap();
        fs::write(domain_dir.join("energy_uj"), format!("{energy_uj}\n")).unwrap();
        if let Some(max_range) = max_range_uj {
            fs::write(
                domain_dir.join("max_energy_range_uj"),
                format!("{max_range}\n"),
            )
            .unwrap();
        }
        domain_dir
    }

    #[test]
    fn test_rapl_skips_psys_when_package_present() {
        let dir = tempdir().unwrap();
        let base = dir.path();

        let pkg0 = create_fake_rapl_domain(base, "intel-rapl:0", "package-0", 1_000_000, None);
        let _psys = create_fake_rapl_domain(base, "intel-rapl:1", "psys", 2_500_000, None);

        let mut sampler = LinuxRaplSampler::try_new_at(base).expect("sampler should be created");
        assert_eq!(sampler.energy_files().len(), 1);
        assert_eq!(sampler.energy_files()[0], pkg0.join("energy_uj"));

        sampler.start();
        fs::write(pkg0.join("energy_uj"), "2000000\n").unwrap();
        // psys changed too, but should be ignored
        fs::write(_psys.join("energy_uj"), "5000000\n").unwrap();

        let energy = sampler.stop().expect("should return energy");
        assert!((energy - 1.0).abs() < 1e-6); // 1,000,000 uJ = 1.0 J
    }

    #[test]
    fn test_rapl_sums_multiple_packages() {
        let dir = tempdir().unwrap();
        let base = dir.path();

        let pkg0 = create_fake_rapl_domain(base, "intel-rapl:0", "package-0", 1_000_000, None);
        let pkg1 = create_fake_rapl_domain(base, "intel-rapl:1", "package-1", 2_000_000, None);
        let _psys = create_fake_rapl_domain(base, "intel-rapl:2", "psys", 10_000_000, None);

        let mut sampler = LinuxRaplSampler::try_new_at(base).expect("sampler should be created");
        assert_eq!(sampler.energy_files().len(), 2);

        sampler.start();
        fs::write(pkg0.join("energy_uj"), "2000000\n").unwrap(); // +1.0 J
        fs::write(pkg1.join("energy_uj"), "3500000\n").unwrap(); // +1.5 J

        let energy = sampler.stop().expect("should return energy");
        assert!((energy - 2.5).abs() < 1e-6); // 1.0 + 1.5 = 2.5 J
    }

    #[test]
    fn test_rapl_uses_psys_when_no_packages() {
        let dir = tempdir().unwrap();
        let base = dir.path();

        let psys = create_fake_rapl_domain(base, "intel-rapl:0", "psys", 1_000_000, None);

        let mut sampler = LinuxRaplSampler::try_new_at(base).expect("sampler should be created");
        assert_eq!(sampler.energy_files().len(), 1);
        assert_eq!(sampler.energy_files()[0], psys.join("energy_uj"));

        sampler.start();
        fs::write(psys.join("energy_uj"), "2500000\n").unwrap();

        let energy = sampler.stop().expect("should return energy");
        assert!((energy - 1.5).abs() < 1e-6); // 1.5 J
    }

    #[test]
    fn test_rapl_counter_wraparound() {
        let dir = tempdir().unwrap();
        let base = dir.path();

        let pkg0 =
            create_fake_rapl_domain(base, "intel-rapl:0", "package-0", 900_000, Some(1_000_000));

        let mut sampler = LinuxRaplSampler::try_new_at(base).expect("sampler should be created");
        sampler.start();
        // Wraps around past max range 1_000_000 to 200_000:
        // diff = (1_000_000 - 900_000) + 1 + 200_000 = 300_001 uJ
        fs::write(pkg0.join("energy_uj"), "200000\n").unwrap();

        let energy = sampler.stop().expect("should return energy");
        assert!((energy - 0.300_001).abs() < 1e-9);
    }

    #[test]
    fn test_rapl_read_failure_yields_no_sample() {
        let dir = tempdir().unwrap();
        let base = dir.path();

        let pkg0 =
            create_fake_rapl_domain(base, "intel-rapl:0", "package-0", 900_000, Some(1_000_000));

        let mut sampler = LinuxRaplSampler::try_new_at(base).expect("sampler should be created");
        sampler.start();
        // An unreadable/garbled counter used to be read as 0, which looked like
        // a wrap-around and reported ~0.1 J of bogus energy.
        fs::write(pkg0.join("energy_uj"), "garbage\n").unwrap();
        assert_eq!(sampler.stop(), None);

        // A failed start reading also yields no sample
        sampler.start();
        fs::write(pkg0.join("energy_uj"), "950000\n").unwrap();
        assert_eq!(sampler.stop(), None);

        // …and the sampler recovers once the counter is readable again
        sampler.start();
        fs::write(pkg0.join("energy_uj"), "960000\n").unwrap();
        let energy = sampler.stop().expect("should return energy");
        assert!((energy - 0.01).abs() < 1e-9);
    }

    #[test]
    fn test_rapl_wraparound_with_unknown_range_yields_no_sample() {
        let dir = tempdir().unwrap();
        let base = dir.path();

        let pkg0 = create_fake_rapl_domain(base, "intel-rapl:0", "package-0", 900_000, None);

        let mut sampler = LinuxRaplSampler::try_new_at(base).expect("sampler should be created");
        sampler.start();
        fs::write(pkg0.join("energy_uj"), "200000\n").unwrap();
        assert_eq!(sampler.stop(), None);
    }

    fn sub_zone(parent: &Path, idx: &str, name: &str, uj: u64) -> PathBuf {
        create_fake_rapl_domain(
            parent,
            &format!("{}:{idx}", parent.file_name().unwrap().to_string_lossy()),
            name,
            uj,
            None,
        )
    }

    #[test]
    fn domains_are_reported_but_the_total_stays_package_only() {
        let dir = tempdir().unwrap();
        let base = dir.path();
        let pkg = create_fake_rapl_domain(base, "intel-rapl:0", "package-0", 1_000_000, None);
        let core = sub_zone(&pkg, "0", "core", 100_000);
        let uncore = sub_zone(&pkg, "1", "uncore", 10_000);
        let dram = sub_zone(&pkg, "2", "dram", 200_000);
        let psys = create_fake_rapl_domain(base, "intel-rapl:1", "psys", 5_000_000, None);

        let mut s = LinuxRaplSampler::try_new_at(base).unwrap();
        assert_eq!(s.energy_files().len(), 1);
        s.enable_domains();
        s.start();
        fs::write(pkg.join("energy_uj"), "3000000\n").unwrap(); // +2.0 J
        fs::write(core.join("energy_uj"), "1100000\n").unwrap(); // +1.0 J
        fs::write(uncore.join("energy_uj"), "60000\n").unwrap(); // +0.05 J
        fs::write(dram.join("energy_uj"), "700000\n").unwrap(); // +0.5 J
        fs::write(psys.join("energy_uj"), "9000000\n").unwrap(); // +4.0 J

        let total = s.stop().unwrap();
        assert!(
            (total - 2.0).abs() < 1e-9,
            "total is the package only: {total}"
        );
        let d = s.domain_energy().unwrap();
        let get = |n: &str| d.iter().find(|(k, _)| k == n).map(|(_, v)| *v).unwrap();
        let names: Vec<&str> = d.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(names, ["package", "core", "uncore", "dram", "psys"]);
        assert!((get("package") - 2.0).abs() < 1e-9);
        assert!((get("core") - 1.0).abs() < 1e-9);
        assert!((get("uncore") - 0.05).abs() < 1e-9);
        assert!((get("dram") - 0.5).abs() < 1e-9);
        assert!((get("psys") - 4.0).abs() < 1e-9);
    }

    #[test]
    fn domains_of_several_packages_are_summed_per_name() {
        let dir = tempdir().unwrap();
        let base = dir.path();
        let p0 = create_fake_rapl_domain(base, "intel-rapl:0", "package-0", 0, None);
        let p1 = create_fake_rapl_domain(base, "intel-rapl:1", "package-1", 0, None);
        let d0 = sub_zone(&p0, "0", "dram", 0);
        let d1 = sub_zone(&p1, "0", "dram", 0);
        let mut s = LinuxRaplSampler::try_new_at(base).unwrap();
        s.enable_domains();
        s.start();
        fs::write(d0.join("energy_uj"), "1000000\n").unwrap();
        fs::write(d1.join("energy_uj"), "500000\n").unwrap();
        fs::write(p0.join("energy_uj"), "2000000\n").unwrap();
        fs::write(p1.join("energy_uj"), "2000000\n").unwrap();
        assert!((s.stop().unwrap() - 4.0).abs() < 1e-9);
        let d = s.domain_energy().unwrap();
        let dram = d.iter().find(|(k, _)| k == "dram").unwrap().1;
        assert!((dram - 1.5).abs() < 1e-9);
    }

    #[test]
    fn domains_are_off_by_default_and_missing_zones_are_left_out() {
        let dir = tempdir().unwrap();
        let base = dir.path();
        let pkg = create_fake_rapl_domain(base, "intel-rapl:0", "package-0", 0, None);
        let mut s = LinuxRaplSampler::try_new_at(base).unwrap();
        s.start();
        fs::write(pkg.join("energy_uj"), "1000000\n").unwrap();
        s.stop().unwrap();
        assert!(s.domain_energy().is_none());
        s.enable_domains();
        s.start();
        s.stop().unwrap();
        let d = s.domain_energy().unwrap();
        assert_eq!(d.len(), 1, "only package exists: {d:?}");
    }

    #[test]
    fn permission_message_names_the_file_and_the_fix() {
        let m = permission_message(Path::new(
            "/sys/class/powercap/intel-rapl/intel-rapl:0/energy_uj",
        ));
        assert!(m.contains("intel-rapl:0/energy_uj"));
        assert!(m.contains("chmod o+r"));
        assert!(m.contains("udev"));
    }

    #[cfg(unix)]
    #[test]
    fn first_unreadable_finds_a_file_that_cannot_be_read() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempdir().unwrap();
        let pkg = create_fake_rapl_domain(dir.path(), "intel-rapl:0", "package-0", 1, None);
        let f = pkg.join("energy_uj");
        assert_eq!(first_unreadable(dir.path()), None);
        fs::set_permissions(&f, fs::Permissions::from_mode(0o000)).unwrap();
        // root can still read it
        if fs::read_to_string(&f).is_err() {
            assert_eq!(first_unreadable(dir.path()), Some(f));
        }
    }

    #[test]
    fn test_counter_delta() {
        assert_eq!(counter_delta(10, 25, None), Some(15));
        assert_eq!(counter_delta(10, 25, Some(100)), Some(15));
        assert_eq!(counter_delta(90, 5, Some(100)), Some(16)); // 90..=100 → 0..=5
        assert_eq!(counter_delta(90, 5, None), None);
        assert_eq!(counter_delta(150, 5, Some(100)), None); // start beyond range
    }
}

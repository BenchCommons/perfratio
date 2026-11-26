//! Apple Silicon energy via the IOReport "Energy Model" channel group (macOS, no root).
//!
//! Same source as `powermetrics`, macmon and asitop. The channels are cumulative energy
//! counters per SoC component; the sampler reports the delta between `start` and `stop`
//! summed over CPU, GPU, ANE and DRAM, in joules per window (same semantics as RAPL).
//! IOReport is a private API: it is loaded with `dlopen` so a missing library or
//! subscription failure becomes `unavailable` instead of a link or start-up error.

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) const GROUP: &str = "Energy Model";

/// SoC components whose energy is summed. Names seen on M1..M4: `CPU Energy`,
/// `GPU Energy`, `ANE`, `DRAM`, `DIE_<n>_CPU Energy` (Ultra), `ANE0`, `ANE0_<n>`.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn is_counted_channel(name: &str) -> bool {
    name.ends_with("CPU Energy")
        || name == "GPU Energy"
        || name.starts_with("ANE")
        || name.starts_with("DRAM")
}

/// Converts an IOReport energy value with its unit label to joules.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn to_joules(value: i64, unit: &str) -> Option<f64> {
    let scale = match unit.trim() {
        "mJ" => 1e-3,
        "uJ" | "\u{b5}J" => 1e-6,
        "nJ" => 1e-9,
        "J" => 1.0,
        _ => return None,
    };
    Some(value as f64 * scale)
}

/// Sums `(channel name, value, unit)` rows of one delta sample. `None` when no counted
/// channel was present or a unit is unknown.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn sum_energy<'a>(
    rows: impl IntoIterator<Item = (&'a str, i64, &'a str)>,
) -> Option<f64> {
    let mut total = 0.0;
    let mut seen = false;
    for (name, value, unit) in rows {
        if !is_counted_channel(name) {
            continue;
        }
        total += to_joules(value.max(0), unit)?;
        seen = true;
    }
    seen.then_some(total)
}

/// True when the CPU energy channels report a positive amount. On macOS 27 these
/// counters read 0 for processes without Apple's entitlement while GPU stays live, which
/// would make every CPU-bound run look idle. `sum_energy` alone cannot see that.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn cpu_energy_is_live<'a>(
    rows: impl IntoIterator<Item = (&'a str, i64, &'a str)>,
) -> bool {
    sum_energy(
        rows.into_iter()
            .filter(|(n, _, _)| n.ends_with("CPU Energy")),
    )
    .is_some_and(|j| j > 0.0)
}

#[cfg(target_os = "macos")]
pub use imp::{IoReportError, IoReportSampler};

#[cfg(target_os = "macos")]
mod imp {
    use super::{cpu_energy_is_live, sum_energy, GROUP};
    use crate::energy::EnergySampler;
    use std::ffi::{c_char, c_void, CString};
    use std::ptr::null;

    type CFTypeRef = *const c_void;
    type CFIndex = isize;

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFStringCreateWithCString(
            alloc: CFTypeRef,
            s: *const c_char,
            encoding: u32,
        ) -> CFTypeRef;
        fn CFRelease(cf: CFTypeRef);
        fn CFDictionaryGetValue(dict: CFTypeRef, key: CFTypeRef) -> CFTypeRef;
        fn CFDictionaryCreateMutableCopy(
            alloc: CFTypeRef,
            capacity: CFIndex,
            dict: CFTypeRef,
        ) -> CFTypeRef;
        fn CFArrayGetCount(a: CFTypeRef) -> CFIndex;
        fn CFArrayGetValueAtIndex(a: CFTypeRef, i: CFIndex) -> CFTypeRef;
        fn CFStringGetCString(s: CFTypeRef, buf: *mut c_char, size: CFIndex, encoding: u32) -> u8;
    }
    const UTF8: u32 = 0x0800_0100;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum IoReportError {
        /// Library or symbols missing, or the group has no channels (Intel Mac, VM)
        NotFound,
        /// Subscription could not be created
        Failed,
        /// The CPU energy counters stay at 0 under load (macOS 27 without entitlement)
        Frozen,
    }

    struct Api {
        copy_channels_in_group:
            unsafe extern "C" fn(CFTypeRef, CFTypeRef, u64, u64, u64) -> CFTypeRef,
        create_subscription:
            unsafe extern "C" fn(CFTypeRef, CFTypeRef, *mut CFTypeRef, u64, CFTypeRef) -> CFTypeRef,
        create_samples: unsafe extern "C" fn(CFTypeRef, CFTypeRef, CFTypeRef) -> CFTypeRef,
        samples_delta: unsafe extern "C" fn(CFTypeRef, CFTypeRef, CFTypeRef) -> CFTypeRef,
        channel_name: unsafe extern "C" fn(CFTypeRef) -> CFTypeRef,
        unit_label: unsafe extern "C" fn(CFTypeRef) -> CFTypeRef,
        integer_value: unsafe extern "C" fn(CFTypeRef, i32) -> i64,
    }

    fn load_api() -> Option<Api> {
        // SAFETY: dlopen/dlsym with NUL-terminated names; each symbol is transmuted to the
        // signature used by macmon and asitop for the same private function.
        unsafe {
            let h = libc::dlopen(c"/usr/lib/libIOReport.dylib".as_ptr(), libc::RTLD_NOW);
            if h.is_null() {
                return None;
            }
            macro_rules! sym {
                ($n:literal, $t:ty) => {{
                    let p = libc::dlsym(h, $n.as_ptr());
                    if p.is_null() {
                        return None;
                    }
                    std::mem::transmute::<*mut c_void, $t>(p)
                }};
            }
            Some(Api {
                copy_channels_in_group: sym!(
                    c"IOReportCopyChannelsInGroup",
                    unsafe extern "C" fn(CFTypeRef, CFTypeRef, u64, u64, u64) -> CFTypeRef
                ),
                create_subscription: sym!(
                    c"IOReportCreateSubscription",
                    unsafe extern "C" fn(
                        CFTypeRef,
                        CFTypeRef,
                        *mut CFTypeRef,
                        u64,
                        CFTypeRef,
                    ) -> CFTypeRef
                ),
                create_samples: sym!(
                    c"IOReportCreateSamples",
                    unsafe extern "C" fn(CFTypeRef, CFTypeRef, CFTypeRef) -> CFTypeRef
                ),
                samples_delta: sym!(
                    c"IOReportCreateSamplesDelta",
                    unsafe extern "C" fn(CFTypeRef, CFTypeRef, CFTypeRef) -> CFTypeRef
                ),
                channel_name: sym!(
                    c"IOReportChannelGetChannelName",
                    unsafe extern "C" fn(CFTypeRef) -> CFTypeRef
                ),
                unit_label: sym!(
                    c"IOReportChannelGetUnitLabel",
                    unsafe extern "C" fn(CFTypeRef) -> CFTypeRef
                ),
                integer_value: sym!(
                    c"IOReportSimpleGetIntegerValue",
                    unsafe extern "C" fn(CFTypeRef, i32) -> i64
                ),
            })
        }
    }

    unsafe fn cfstr(s: &str) -> CFTypeRef {
        let c = CString::new(s).unwrap();
        CFStringCreateWithCString(null(), c.as_ptr(), UTF8)
    }

    unsafe fn from_cfstr(s: CFTypeRef) -> String {
        if s.is_null() {
            return String::new();
        }
        let mut buf = [0 as c_char; 128];
        if CFStringGetCString(s, buf.as_mut_ptr(), buf.len() as CFIndex, UTF8) == 0 {
            return String::new();
        }
        std::ffi::CStr::from_ptr(buf.as_ptr())
            .to_string_lossy()
            .into_owned()
    }

    pub struct IoReportSampler {
        api: Api,
        subscription: CFTypeRef,
        channels: CFTypeRef,
        first: CFTypeRef,
    }

    // SAFETY: the raw CoreFoundation objects are only touched through `&mut self`.
    unsafe impl Send for IoReportSampler {}
    // SAFETY: as above; no `&self` method mutates the CoreFoundation objects.
    unsafe impl Sync for IoReportSampler {}

    impl IoReportSampler {
        pub fn try_new() -> Result<Self, IoReportError> {
            let api = load_api().ok_or(IoReportError::NotFound)?;
            // SAFETY: arguments follow the IOReport conventions used by macmon.
            unsafe {
                let group = cfstr(GROUP);
                let all = (api.copy_channels_in_group)(group, null(), 0, 0, 0);
                CFRelease(group);
                if all.is_null() {
                    return Err(IoReportError::NotFound);
                }
                let chan = CFDictionaryCreateMutableCopy(null(), 0, all);
                CFRelease(all);
                if chan.is_null() {
                    return Err(IoReportError::Failed);
                }
                let mut subscribed: CFTypeRef = null();
                let sub = (api.create_subscription)(null(), chan, &mut subscribed, 0, null());
                CFRelease(chan);
                if sub.is_null() || subscribed.is_null() {
                    return Err(IoReportError::Failed);
                }
                let s = Self {
                    api,
                    subscription: sub,
                    channels: subscribed,
                    first: null(),
                };
                // Probe: burn CPU for ~100 ms and require live CPU energy counters.
                let a = (s.api.create_samples)(s.subscription, s.channels, null());
                let end = std::time::Instant::now() + std::time::Duration::from_millis(100);
                let mut x = 0u64;
                while std::time::Instant::now() < end {
                    x = std::hint::black_box(x.wrapping_add(1));
                }
                let b = (s.api.create_samples)(s.subscription, s.channels, null());
                let rows = if a.is_null() || b.is_null() {
                    None
                } else {
                    s.rows_between(a, b)
                };
                for p in [a, b] {
                    if !p.is_null() {
                        CFRelease(p);
                    }
                }
                match rows {
                    None => Err(IoReportError::NotFound),
                    Some(r)
                        if !cpu_energy_is_live(
                            r.iter().map(|(n, v, u)| (n.as_str(), *v, u.as_str())),
                        ) =>
                    {
                        Err(IoReportError::Frozen)
                    }
                    Some(_) => Ok(s),
                }
            }
        }

        /// `(channel, value, unit)` rows of the delta between two samples.
        unsafe fn rows_between(
            &self,
            a: CFTypeRef,
            b: CFTypeRef,
        ) -> Option<Vec<(String, i64, String)>> {
            let delta = (self.api.samples_delta)(a, b, null());
            if delta.is_null() {
                return None;
            }
            let key = cfstr("IOReportChannels");
            let items = CFDictionaryGetValue(delta, key);
            CFRelease(key);
            let mut rows = Vec::new();
            if !items.is_null() {
                for i in 0..CFArrayGetCount(items) {
                    let item = CFArrayGetValueAtIndex(items, i);
                    let name = from_cfstr((self.api.channel_name)(item));
                    let unit = from_cfstr((self.api.unit_label)(item));
                    rows.push((name, (self.api.integer_value)(item, 0), unit));
                }
            }
            CFRelease(delta);
            Some(rows)
        }

        unsafe fn joules_between(&self, a: CFTypeRef, b: CFTypeRef) -> Option<f64> {
            let rows = self.rows_between(a, b)?;
            sum_energy(rows.iter().map(|(n, v, u)| (n.as_str(), *v, u.as_str())))
        }
    }

    impl Drop for IoReportSampler {
        fn drop(&mut self) {
            // SAFETY: each pointer is owned by this struct and released once.
            unsafe {
                for p in [self.first, self.channels, self.subscription] {
                    if !p.is_null() {
                        CFRelease(p);
                    }
                }
            }
        }
    }

    impl EnergySampler for IoReportSampler {
        fn start(&mut self) {
            // SAFETY: subscription and channels are valid for the sampler's lifetime.
            unsafe {
                if !self.first.is_null() {
                    CFRelease(self.first);
                }
                self.first = (self.api.create_samples)(self.subscription, self.channels, null());
            }
        }

        fn stop(&mut self) -> Option<f64> {
            if self.first.is_null() {
                return None;
            }
            // SAFETY: as above; `first` is released and cleared after use.
            unsafe {
                let second = (self.api.create_samples)(self.subscription, self.channels, null());
                let joules = if second.is_null() {
                    None
                } else {
                    self.joules_between(self.first, second)
                };
                if !second.is_null() {
                    CFRelease(second);
                }
                CFRelease(self.first);
                self.first = null();
                joules
            }
        }

        fn is_available(&self) -> bool {
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_conversion() {
        assert_eq!(to_joules(2000, "mJ"), Some(2.0));
        assert_eq!(to_joules(1_500_000, "uJ"), Some(1.5));
        assert!((to_joules(3_000_000_000, "nJ").unwrap() - 3.0).abs() < 1e-12);
        assert_eq!(to_joules(1, "bogus"), None);
    }

    #[test]
    fn sums_only_soc_components() {
        let rows = [
            ("CPU Energy", 1000, "mJ"),
            ("GPU Energy", 500, "mJ"),
            ("ANE", 100, "mJ"),
            ("DRAM", 400, "mJ"),
            ("GPU SRAM0", 99_999, "mJ"),
            ("DCS", 99_999, "mJ"),
        ];
        assert!((sum_energy(rows).unwrap() - 2.0).abs() < 1e-12);
    }

    #[test]
    fn ultra_die_channels_are_counted() {
        let rows = [("DIE_0_CPU Energy", 1, "J"), ("DIE_1_CPU Energy", 2, "J")];
        assert_eq!(sum_energy(rows), Some(3.0));
    }

    #[test]
    fn no_counted_channel_or_bad_unit_is_none() {
        assert_eq!(sum_energy([("Other", 1, "mJ")]), None);
        assert_eq!(sum_energy([("CPU Energy", 1, "xx")]), None);
        assert_eq!(sum_energy(std::iter::empty()), None);
    }

    #[test]
    fn frozen_cpu_counters_are_detected() {
        // GPU alive, CPU frozen at 0: the macOS 27 failure mode
        let frozen = [("CPU Energy", 0, "mJ"), ("GPU Energy", 44_791_467, "nJ")];
        assert!(!cpu_energy_is_live(frozen));
        let live = [("CPU Energy", 120, "mJ"), ("GPU Energy", 0, "nJ")];
        assert!(cpu_energy_is_live(live));
        assert!(!cpu_energy_is_live([("GPU Energy", 5, "nJ")]));
    }

    #[test]
    fn negative_delta_is_clamped() {
        assert_eq!(sum_energy([("CPU Energy", -5, "mJ")]), Some(0.0));
    }
}

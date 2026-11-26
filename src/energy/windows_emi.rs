//! Windows energy via the Energy Meter Interface (EMI).
//!
//! Devices that implement `GUID_DEVICE_ENERGY_METER` expose accumulated energy in
//! picowatt-hours through `IOCTL_EMI_GET_MEASUREMENT`. Opening the device usually
//! needs an elevated prompt, and few machines ship an EMI driver; both cases are
//! reported as `unavailable`. The buffer parsing and channel selection below are pure
//! and unit-tested on every platform; only the device access is `cfg(windows)`.
//! Not run on real EMI hardware (see docs/specs/energy-macos-windows.md).

#![cfg_attr(not(windows), allow(dead_code))]

/// 1 pWh = 1e-12 Wh = 3.6e-9 J.
pub(crate) fn picowatt_hours_to_joules(pwh: u64) -> f64 {
    pwh as f64 * 3.6e-9
}

/// One `EMI_CHANNEL_MEASUREMENT_DATA`: accumulated energy (pWh) and time (100 ns).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Reading {
    pub energy_pwh: u64,
    pub time_100ns: u64,
}

/// Parses `EMI_MEASUREMENT_DATA_V1/V2`: `count` consecutive 16-byte entries
/// (V1 is a single entry).
pub(crate) fn parse_measurement(buf: &[u8], count: usize) -> Option<Vec<Reading>> {
    if count == 0 || buf.len() < count * 16 {
        return None;
    }
    Some(
        (0..count)
            .map(|i| &buf[i * 16..i * 16 + 16])
            .map(|c| Reading {
                energy_pwh: u64::from_le_bytes(c[0..8].try_into().unwrap()),
                time_100ns: u64::from_le_bytes(c[8..16].try_into().unwrap()),
            })
            .collect(),
    )
}

/// Channel names from `EMI_METADATA_V2`: 2*16 WCHAR OEM/model, revision (u16),
/// channel count (u16), then per channel unit (u32), name size in bytes (u16, incl. NUL)
/// and the UTF-16 name. Returns `None` for a layout it does not understand.
pub(crate) fn parse_v2_channel_names(buf: &[u8]) -> Option<Vec<String>> {
    const HEADER: usize = 64 + 2;
    let count = u16::from_le_bytes(buf.get(HEADER..HEADER + 2)?.try_into().ok()?) as usize;
    let mut off = HEADER + 2;
    let mut names = Vec::with_capacity(count);
    for _ in 0..count {
        let unit = u32::from_le_bytes(buf.get(off..off + 4)?.try_into().ok()?);
        if unit != 0 {
            return None; // only picowatt-hours is defined
        }
        let size = u16::from_le_bytes(buf.get(off + 4..off + 6)?.try_into().ok()?) as usize;
        let raw = buf.get(off + 6..off + 6 + size)?;
        let units: Vec<u16> = (0..raw.len() / 2)
            .map(|i| u16::from_le_bytes([raw[2 * i], raw[2 * i + 1]]))
            .take_while(|&u| u != 0)
            .collect();
        names.push(String::from_utf16_lossy(&units));
        off += 6 + size;
    }
    Some(names)
}

/// Indices of the channels to sum. Mirrors the RAPL rule of using package domains only
/// so overlapping sub-domains (PP0, DRAM, ...) are not double counted: if any channel
/// name contains `PKG`, use those; otherwise use all channels.
pub(crate) fn select_channels(names: &[String]) -> Vec<usize> {
    let pkg: Vec<usize> = names
        .iter()
        .enumerate()
        .filter(|(_, n)| n.to_ascii_uppercase().contains("PKG"))
        .map(|(i, _)| i)
        .collect();
    if pkg.is_empty() {
        (0..names.len()).collect()
    } else {
        pkg
    }
}

/// Joules between two reads of the same channels. Counters wrap at 2^64, so the
/// difference uses wrapping subtraction.
pub(crate) fn delta_joules(
    before: &[Reading],
    after: &[Reading],
    selected: &[usize],
) -> Option<f64> {
    if before.len() != after.len() || selected.is_empty() {
        return None;
    }
    let mut pwh = 0f64;
    for &i in selected {
        let (b, a) = (before.get(i)?, after.get(i)?);
        pwh += a.energy_pwh.wrapping_sub(b.energy_pwh) as f64;
    }
    Some(pwh * 3.6e-9)
}

#[cfg(windows)]
pub use imp::{EmiError, EmiSampler};

#[cfg(windows)]
mod imp {
    use super::*;
    use crate::energy::EnergySampler;
    use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
        SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInterfaces, SetupDiGetClassDevsW,
        SetupDiGetDeviceInterfaceDetailW, DIGCF_DEVICEINTERFACE, DIGCF_PRESENT,
        SP_DEVICE_INTERFACE_DATA, SP_DEVICE_INTERFACE_DETAIL_DATA_W,
    };
    use windows_sys::Win32::Foundation::{CloseHandle, GENERIC_READ, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::Power::{
        GUID_DEVICE_ENERGY_METER, IOCTL_EMI_GET_MEASUREMENT, IOCTL_EMI_GET_METADATA,
        IOCTL_EMI_GET_METADATA_SIZE, IOCTL_EMI_GET_VERSION,
    };
    use windows_sys::Win32::System::IO::DeviceIoControl;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum EmiError {
        /// No EMI device interface is present
        NotFound,
        /// Devices exist but could not be opened or read (usually needs admin)
        Unreadable,
    }

    struct Device {
        handle: HANDLE,
        /// Number of channels (1 for EMI V1)
        channels: usize,
        /// Offset of this device's channels in the combined channel list
        base: usize,
    }

    pub struct EmiSampler {
        devices: Vec<Device>,
        selected: Vec<usize>,
        before: Option<Vec<Reading>>,
    }

    // SAFETY: handles are only used through `&mut self`.
    unsafe impl Send for EmiSampler {}
    unsafe impl Sync for EmiSampler {}

    impl Drop for EmiSampler {
        fn drop(&mut self) {
            for d in &self.devices {
                // SAFETY: handle was opened by CreateFileW and is closed once.
                unsafe { CloseHandle(d.handle) };
            }
        }
    }

    unsafe fn ioctl(h: HANDLE, code: u32, out: &mut [u8]) -> bool {
        let mut returned = 0u32;
        DeviceIoControl(
            h,
            code,
            std::ptr::null(),
            0,
            out.as_mut_ptr().cast(),
            out.len() as u32,
            &mut returned,
            std::ptr::null_mut(),
        ) != 0
    }

    /// Device paths of all present EMI interfaces.
    fn device_paths() -> Vec<Vec<u16>> {
        let mut paths = Vec::new();
        // SAFETY: standard SetupAPI enumeration; buffers are sized from the API's answer.
        unsafe {
            let set = SetupDiGetClassDevsW(
                &GUID_DEVICE_ENERGY_METER,
                std::ptr::null(),
                std::ptr::null_mut(),
                DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
            );
            if set == -1 {
                return paths;
            }
            let mut i = 0;
            loop {
                let mut data: SP_DEVICE_INTERFACE_DATA = std::mem::zeroed();
                data.cbSize = std::mem::size_of::<SP_DEVICE_INTERFACE_DATA>() as u32;
                if SetupDiEnumDeviceInterfaces(
                    set,
                    std::ptr::null(),
                    &GUID_DEVICE_ENERGY_METER,
                    i,
                    &mut data,
                ) == 0
                {
                    break;
                }
                i += 1;
                let mut required = 0u32;
                SetupDiGetDeviceInterfaceDetailW(
                    set,
                    &data,
                    std::ptr::null_mut(),
                    0,
                    &mut required,
                    std::ptr::null_mut(),
                );
                if required == 0 {
                    continue;
                }
                // u64 backing store keeps the buffer 8-byte aligned.
                let mut buf = vec![0u64; (required as usize).div_ceil(8)];
                let detail = buf.as_mut_ptr().cast::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>();
                (*detail).cbSize = std::mem::size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
                if SetupDiGetDeviceInterfaceDetailW(
                    set,
                    &data,
                    detail,
                    required,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                ) == 0
                {
                    continue;
                }
                let start = std::ptr::addr_of!((*detail).DevicePath).cast::<u16>();
                let max = (required as usize).saturating_sub(4) / 2;
                let mut path = Vec::new();
                for k in 0..max {
                    // read_unaligned: the struct is packed on x86
                    let c = start.add(k).read_unaligned();
                    if c == 0 {
                        break;
                    }
                    path.push(c);
                }
                path.push(0);
                paths.push(path);
            }
            SetupDiDestroyDeviceInfoList(set);
        }
        paths
    }

    impl EmiSampler {
        pub fn try_new() -> Result<Self, EmiError> {
            let paths = device_paths();
            if paths.is_empty() {
                return Err(EmiError::NotFound);
            }
            let mut denied_or_failed = false;
            let mut devices = Vec::new();
            let mut names: Vec<String> = Vec::new();
            for path in paths {
                // SAFETY: `path` is NUL-terminated UTF-16.
                let h = unsafe {
                    CreateFileW(
                        path.as_ptr(),
                        GENERIC_READ,
                        FILE_SHARE_READ | FILE_SHARE_WRITE,
                        std::ptr::null(),
                        OPEN_EXISTING,
                        0,
                        std::ptr::null_mut(),
                    )
                };
                if h == INVALID_HANDLE_VALUE {
                    denied_or_failed = true;
                    continue;
                }
                match Self::describe(h) {
                    Some(dev_names) => {
                        devices.push(Device {
                            handle: h,
                            channels: dev_names.len(),
                            base: names.len(),
                        });
                        names.extend(dev_names);
                    }
                    None => {
                        denied_or_failed = true;
                        // SAFETY: opened above, not stored.
                        unsafe { CloseHandle(h) };
                    }
                }
            }
            if devices.is_empty() {
                return Err(if denied_or_failed {
                    EmiError::Unreadable
                } else {
                    EmiError::NotFound
                });
            }
            let selected = select_channels(&names);
            Ok(Self {
                devices,
                selected,
                before: None,
            })
        }

        /// Channel names of one device, or None if it speaks an unknown version/unit.
        fn describe(h: HANDLE) -> Option<Vec<String>> {
            // SAFETY: buffers are sized per the EMI structures.
            unsafe {
                let mut ver = [0u8; 2];
                if !ioctl(h, IOCTL_EMI_GET_VERSION, &mut ver) {
                    return None;
                }
                let version = u16::from_le_bytes(ver);
                let mut size = [0u8; 4];
                if !ioctl(h, IOCTL_EMI_GET_METADATA_SIZE, &mut size) {
                    return None;
                }
                let mut meta = vec![0u8; u32::from_le_bytes(size) as usize];
                if !ioctl(h, IOCTL_EMI_GET_METADATA, &mut meta) {
                    return None;
                }
                match version {
                    // V1: one channel, MeasurementUnit (u32) must be picowatt-hours (0)
                    1 => (meta.get(0..4)? == [0, 0, 0, 0]).then(|| vec![String::new()]),
                    2 => parse_v2_channel_names(&meta),
                    _ => None,
                }
            }
        }

        fn read_all(&self) -> Option<Vec<Reading>> {
            let mut all = Vec::new();
            for d in &self.devices {
                let mut buf = vec![0u8; d.channels * 16];
                // SAFETY: buffer holds `channels` EMI_CHANNEL_MEASUREMENT_DATA entries.
                if !unsafe { ioctl(d.handle, IOCTL_EMI_GET_MEASUREMENT, &mut buf) } {
                    return None;
                }
                debug_assert_eq!(all.len(), d.base);
                all.extend(parse_measurement(&buf, d.channels)?);
            }
            Some(all)
        }
    }

    impl EnergySampler for EmiSampler {
        fn start(&mut self) {
            self.before = self.read_all();
        }

        fn stop(&mut self) -> Option<f64> {
            let before = self.before.take()?;
            let after = self.read_all()?;
            delta_joules(&before, &after, &self.selected)
        }

        fn is_available(&self) -> bool {
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(e: u64) -> Reading {
        Reading {
            energy_pwh: e,
            time_100ns: 0,
        }
    }

    fn utf16(s: &str) -> Vec<u8> {
        s.encode_utf16()
            .chain([0])
            .flat_map(|u| u.to_le_bytes())
            .collect()
    }

    fn v2_meta(names: &[&str]) -> Vec<u8> {
        let mut b = vec![0u8; 64];
        b.extend(1u16.to_le_bytes()); // revision
        b.extend((names.len() as u16).to_le_bytes());
        for n in names {
            let w = utf16(n);
            b.extend(0u32.to_le_bytes());
            b.extend((w.len() as u16).to_le_bytes());
            b.extend(w);
        }
        b
    }

    #[test]
    fn unit_conversion() {
        // 1 Wh = 3600 J = 1e12 pWh
        assert!((picowatt_hours_to_joules(1_000_000_000_000) - 3600.0).abs() < 1e-6);
    }

    #[test]
    fn parses_v2_channel_names() {
        let meta = v2_meta(&["RAPL_Package0_PKG", "RAPL_Package0_DRAM"]);
        assert_eq!(
            parse_v2_channel_names(&meta).unwrap(),
            vec!["RAPL_Package0_PKG", "RAPL_Package0_DRAM"]
        );
    }

    #[test]
    fn rejects_truncated_or_unknown_unit_metadata() {
        let meta = v2_meta(&["A", "B"]);
        assert!(parse_v2_channel_names(&meta[..meta.len() - 3]).is_none());
        let mut bad = v2_meta(&["A"]);
        bad[68] = 7; // unit != picowatt-hours
        assert!(parse_v2_channel_names(&bad).is_none());
        assert!(parse_v2_channel_names(&[]).is_none());
    }

    #[test]
    fn prefers_package_channels() {
        let n = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            select_channels(&n(&["RAPL_Package0_PKG", "RAPL_Package0_PP0", "DRAM"])),
            vec![0]
        );
        assert_eq!(select_channels(&n(&["a", "b"])), vec![0, 1]);
    }

    #[test]
    fn parses_measurements() {
        let mut b = Vec::new();
        for (e, t) in [(5u64, 7u64), (9, 11)] {
            b.extend(e.to_le_bytes());
            b.extend(t.to_le_bytes());
        }
        let v = parse_measurement(&b, 2).unwrap();
        assert_eq!(v[1].energy_pwh, 9);
        assert_eq!(v[1].time_100ns, 11);
        assert!(parse_measurement(&b, 3).is_none());
        assert!(parse_measurement(&b, 0).is_none());
    }

    #[test]
    fn delta_sums_selected_and_handles_wraparound() {
        let before = [r(100), r(u64::MAX - 1)];
        let after = [r(100 + 1_000_000_000_000), r(2)];
        let j = delta_joules(&before, &after, &[0]).unwrap();
        assert!((j - 3600.0).abs() < 1e-6);
        // wrapped counter: 2 - (MAX-1) = 4 pWh
        let w = delta_joules(&before, &after, &[1]).unwrap();
        assert!((w - 4.0 * 3.6e-9).abs() < 1e-15);
        assert!(delta_joules(&before, &after[..1], &[0]).is_none());
        assert!(delta_joules(&before, &after, &[]).is_none());
        assert!(delta_joules(&before, &after, &[5]).is_none());
    }
}

//! GPU energy and utilization (`--gpu`, #380): NVIDIA NVML and AMD SMI.
//!
//! Both vendor libraries are loaded at run time with `dlopen`, so the binary has no link-time
//! dependency and a machine without a GPU or driver only gets a warning. The energy counters
//! are cumulative (NVML `nvmlDeviceGetTotalEnergyConsumption`, millijoules since driver load;
//! AMD SMI `amdsmi_get_energy_count`, accumulator times resolution in microjoules), so a run's
//! energy is the difference between a read at `start()` and one at `stop()`.
//!
//! Layout: [`GpuBackend`] is the device-level interface. [`NvmlBackend`] and
//! [`AmdSmiBackend`] implement it over a table of C function pointers ([`NvmlApi`],
//! [`AmdSmiApi`]) so tests can substitute fake library functions. [`GpuSampler`] wraps the
//! host `EnergySampler` and adds `gpu:<index>` domains and utilization (spec `gpu-energy.md`).
//!
//! Not validated on hardware: the FFI signatures follow the NVML and AMD SMI headers but were
//! only exercised against mocked functions (no GPU on the development machine).

use std::ffi::{c_int, c_uint, c_void, CString};
use std::sync::atomic::{AtomicU64, Ordering};

use super::EnergySampler;

/// Why no GPU backend is usable; maps to `perfratio.gpu.unavailable_reason`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuError {
    /// Neither the NVML nor the AMD SMI shared library could be loaded
    LibraryNotFound,
    /// A library loaded but initialisation failed (driver mismatch, permissions)
    InitFailed,
    /// A library initialised but reports no device
    NoDevices,
    /// This OS has no loader for the vendor libraries
    UnsupportedPlatform,
}

impl GpuError {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LibraryNotFound => "gpu_library_not_found",
            Self::InitFailed => "gpu_init_failed",
            Self::NoDevices => "gpu_no_devices",
            Self::UnsupportedPlatform => "unsupported_platform",
        }
    }

    pub fn message(self) -> &'static str {
        match self {
            Self::LibraryNotFound => {
                "neither libnvidia-ml (NVIDIA NVML) nor libamd_smi (AMD SMI) could be loaded"
            }
            Self::InitFailed => "the GPU management library failed to initialise",
            Self::NoDevices => "the GPU management library reports no device",
            Self::UnsupportedPlatform => "GPU measurement is only implemented on Linux and macOS",
        }
    }

    /// The most informative reason wins: a library that loaded beats one that did not.
    fn rank(self) -> u8 {
        match self {
            Self::NoDevices => 3,
            Self::InitFailed => 2,
            Self::LibraryNotFound => 1,
            Self::UnsupportedPlatform => 0,
        }
    }
}

/// One vendor library with its devices. Indices are local to the backend.
pub trait GpuBackend: Send + Sync {
    /// Value of `perfratio.gpu.backends[]` (`nvml`, `amd_smi`)
    fn name(&self) -> &'static str;
    fn device_count(&self) -> usize;
    /// Cumulative energy of the device in joules, `None` if the device has no counter
    fn energy_joules(&self, device: usize) -> Option<f64>;
    /// Utilization of the graphics engine in percent (0 to 100)
    fn utilization_percent(&self, device: usize) -> Option<f64>;
}

// ---------------------------------------------------------------------------------------
// Dynamic loading
// ---------------------------------------------------------------------------------------

/// A shared library kept loaded for the life of the process (never `dlclose`d, so function
/// pointers taken from it stay valid).
struct DynLib(usize);

impl DynLib {
    #[cfg(unix)]
    fn open_first(names: &[&str]) -> Option<Self> {
        for name in names {
            let Ok(c) = CString::new(*name) else { continue };
            // SAFETY: `c` is a NUL-terminated string that outlives the call.
            let handle = unsafe { libc::dlopen(c.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
            if !handle.is_null() {
                return Some(Self(handle as usize));
            }
        }
        None
    }

    #[cfg(not(unix))]
    fn open_first(_names: &[&str]) -> Option<Self> {
        None
    }

    /// # Safety
    /// `T` must be the `extern "C"` function pointer type of the symbol.
    #[cfg(unix)]
    unsafe fn sym<T: Copy>(&self, name: &str) -> Option<T> {
        assert_eq!(std::mem::size_of::<T>(), std::mem::size_of::<usize>());
        let c = CString::new(name).ok()?;
        let p = libc::dlsym(self.0 as *mut c_void, c.as_ptr());
        // SAFETY: sizes checked above; the caller guarantees the signature.
        (!p.is_null()).then(|| std::mem::transmute_copy::<*mut c_void, T>(&p))
    }

    #[cfg(not(unix))]
    unsafe fn sym<T: Copy>(&self, _name: &str) -> Option<T> {
        None
    }
}

// ---------------------------------------------------------------------------------------
// NVIDIA NVML
// ---------------------------------------------------------------------------------------

const NVML_LIBS: &[&str] = &["libnvidia-ml.so.1", "libnvidia-ml.so"];
const NVML_SUCCESS: c_int = 0;

/// `nvmlUtilization_t`
#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct NvmlUtilization {
    pub gpu: c_uint,
    pub memory: c_uint,
}

type NvmlDevice = *mut c_void;

/// The NVML entry points used (all return `nvmlReturn_t`, 0 = success).
#[derive(Clone, Copy)]
pub struct NvmlApi {
    pub init: unsafe extern "C" fn() -> c_int,
    pub shutdown: unsafe extern "C" fn() -> c_int,
    pub device_count: unsafe extern "C" fn(*mut c_uint) -> c_int,
    pub device_by_index: unsafe extern "C" fn(c_uint, *mut NvmlDevice) -> c_int,
    pub total_energy_mj: unsafe extern "C" fn(NvmlDevice, *mut u64) -> c_int,
    pub utilization: unsafe extern "C" fn(NvmlDevice, *mut NvmlUtilization) -> c_int,
}

impl NvmlApi {
    fn resolve(lib: &DynLib) -> Option<Self> {
        // SAFETY: the names and signatures are those of nvml.h.
        unsafe {
            Some(Self {
                init: lib.sym("nvmlInit_v2")?,
                shutdown: lib.sym("nvmlShutdown")?,
                device_count: lib.sym("nvmlDeviceGetCount_v2")?,
                device_by_index: lib.sym("nvmlDeviceGetHandleByIndex_v2")?,
                total_energy_mj: lib.sym("nvmlDeviceGetTotalEnergyConsumption")?,
                utilization: lib.sym("nvmlDeviceGetUtilizationRates")?,
            })
        }
    }
}

pub struct NvmlBackend {
    api: NvmlApi,
    /// Device handles as integers so the struct is `Send + Sync`
    devices: Vec<usize>,
}

impl NvmlBackend {
    pub fn load() -> Result<Self, GpuError> {
        if cfg!(not(unix)) {
            return Err(GpuError::UnsupportedPlatform);
        }
        let lib = DynLib::open_first(NVML_LIBS).ok_or(GpuError::LibraryNotFound)?;
        let api = NvmlApi::resolve(&lib).ok_or(GpuError::InitFailed)?;
        // `DynLib` has no `Drop`: the library stays loaded and the pointers stay valid.
        Self::from_api(api)
    }

    /// Initialises NVML through `api` and enumerates the devices.
    pub fn from_api(api: NvmlApi) -> Result<Self, GpuError> {
        // SAFETY: calls into NVML (or a test double) with valid out-pointers.
        unsafe {
            if (api.init)() != NVML_SUCCESS {
                return Err(GpuError::InitFailed);
            }
            let mut count: c_uint = 0;
            if (api.device_count)(&mut count) != NVML_SUCCESS {
                (api.shutdown)();
                return Err(GpuError::InitFailed);
            }
            let mut devices = Vec::new();
            for index in 0..count {
                let mut handle: NvmlDevice = std::ptr::null_mut();
                if (api.device_by_index)(index, &mut handle) == NVML_SUCCESS {
                    devices.push(handle as usize);
                }
            }
            if devices.is_empty() {
                (api.shutdown)();
                return Err(GpuError::NoDevices);
            }
            Ok(Self { api, devices })
        }
    }
}

impl Drop for NvmlBackend {
    fn drop(&mut self) {
        // SAFETY: balances the `init` of `from_api`.
        unsafe {
            (self.api.shutdown)();
        }
    }
}

impl GpuBackend for NvmlBackend {
    fn name(&self) -> &'static str {
        "nvml"
    }

    fn device_count(&self) -> usize {
        self.devices.len()
    }

    fn energy_joules(&self, device: usize) -> Option<f64> {
        let handle = *self.devices.get(device)? as NvmlDevice;
        let mut mj: u64 = 0;
        // SAFETY: valid handle from `device_by_index`, valid out-pointer.
        let rc = unsafe { (self.api.total_energy_mj)(handle, &mut mj) };
        // NVML_ERROR_NOT_SUPPORTED (3) before Volta, among others
        (rc == NVML_SUCCESS).then(|| mj as f64 / 1e3)
    }

    fn utilization_percent(&self, device: usize) -> Option<f64> {
        let handle = *self.devices.get(device)? as NvmlDevice;
        let mut u = NvmlUtilization::default();
        // SAFETY: as above.
        let rc = unsafe { (self.api.utilization)(handle, &mut u) };
        (rc == NVML_SUCCESS).then(|| f64::from(u.gpu))
    }
}

// ---------------------------------------------------------------------------------------
// AMD SMI
// ---------------------------------------------------------------------------------------

const AMDSMI_LIBS: &[&str] = &["libamd_smi.so", "/opt/rocm/lib/libamd_smi.so"];
/// `AMDSMI_STATUS_SUCCESS`
const AMDSMI_SUCCESS: c_int = 0;
/// `AMDSMI_INIT_AMD_GPUS`
const AMDSMI_INIT_AMD_GPUS: u64 = 1 << 1;

/// `amdsmi_engine_usage_t` (`reserved` pads it to the size in amdsmi.h)
#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct AmdEngineUsage {
    pub gfx_activity: u32,
    pub umc_activity: u32,
    pub mm_activity: u32,
    pub reserved: [u32; 13],
}

type AmdHandle = *mut c_void;

/// The AMD SMI entry points used (all return `amdsmi_status_t`, 0 = success).
#[derive(Clone, Copy)]
pub struct AmdSmiApi {
    pub init: unsafe extern "C" fn(u64) -> c_int,
    pub shut_down: unsafe extern "C" fn() -> c_int,
    pub socket_handles: unsafe extern "C" fn(*mut u32, *mut AmdHandle) -> c_int,
    pub processor_handles: unsafe extern "C" fn(AmdHandle, *mut u32, *mut AmdHandle) -> c_int,
    /// (processor, energy accumulator, counter resolution in uJ, timestamp)
    pub energy_count: unsafe extern "C" fn(AmdHandle, *mut u64, *mut f32, *mut u64) -> c_int,
    pub gpu_activity: unsafe extern "C" fn(AmdHandle, *mut AmdEngineUsage) -> c_int,
}

impl AmdSmiApi {
    fn resolve(lib: &DynLib) -> Option<Self> {
        // SAFETY: the names and signatures are those of amdsmi.h.
        unsafe {
            Some(Self {
                init: lib.sym("amdsmi_init")?,
                shut_down: lib.sym("amdsmi_shut_down")?,
                socket_handles: lib.sym("amdsmi_get_socket_handles")?,
                processor_handles: lib.sym("amdsmi_get_processor_handles")?,
                energy_count: lib.sym("amdsmi_get_energy_count")?,
                gpu_activity: lib.sym("amdsmi_get_gpu_activity")?,
            })
        }
    }
}

pub struct AmdSmiBackend {
    api: AmdSmiApi,
    devices: Vec<usize>,
}

impl AmdSmiBackend {
    pub fn load() -> Result<Self, GpuError> {
        if cfg!(not(unix)) {
            return Err(GpuError::UnsupportedPlatform);
        }
        let lib = DynLib::open_first(AMDSMI_LIBS).ok_or(GpuError::LibraryNotFound)?;
        let api = AmdSmiApi::resolve(&lib).ok_or(GpuError::InitFailed)?;
        Self::from_api(api)
    }

    /// Initialises AMD SMI through `api` and enumerates the GPU processors of every socket.
    pub fn from_api(api: AmdSmiApi) -> Result<Self, GpuError> {
        // SAFETY: calls into AMD SMI (or a test double); buffers are sized from the counts
        // the library itself reported.
        unsafe {
            if (api.init)(AMDSMI_INIT_AMD_GPUS) != AMDSMI_SUCCESS {
                return Err(GpuError::InitFailed);
            }
            let devices = Self::enumerate(&api);
            match devices {
                Some(d) if !d.is_empty() => Ok(Self { api, devices: d }),
                other => {
                    (api.shut_down)();
                    Err(if other.is_none() {
                        GpuError::InitFailed
                    } else {
                        GpuError::NoDevices
                    })
                }
            }
        }
    }

    unsafe fn enumerate(api: &AmdSmiApi) -> Option<Vec<usize>> {
        let mut sockets: u32 = 0;
        if (api.socket_handles)(&mut sockets, std::ptr::null_mut()) != AMDSMI_SUCCESS {
            return None;
        }
        let mut socket_buf: Vec<AmdHandle> = vec![std::ptr::null_mut(); sockets as usize];
        if (api.socket_handles)(&mut sockets, socket_buf.as_mut_ptr()) != AMDSMI_SUCCESS {
            return None;
        }
        let mut devices = Vec::new();
        for &socket in socket_buf.iter().take(sockets as usize) {
            let mut n: u32 = 0;
            if (api.processor_handles)(socket, &mut n, std::ptr::null_mut()) != AMDSMI_SUCCESS {
                return None;
            }
            let mut buf: Vec<AmdHandle> = vec![std::ptr::null_mut(); n as usize];
            if (api.processor_handles)(socket, &mut n, buf.as_mut_ptr()) != AMDSMI_SUCCESS {
                return None;
            }
            devices.extend(buf.iter().take(n as usize).map(|&h| h as usize));
        }
        Some(devices)
    }
}

impl Drop for AmdSmiBackend {
    fn drop(&mut self) {
        // SAFETY: balances the `init` of `from_api`.
        unsafe {
            (self.api.shut_down)();
        }
    }
}

impl GpuBackend for AmdSmiBackend {
    fn name(&self) -> &'static str {
        "amd_smi"
    }

    fn device_count(&self) -> usize {
        self.devices.len()
    }

    fn energy_joules(&self, device: usize) -> Option<f64> {
        let handle = *self.devices.get(device)? as AmdHandle;
        let (mut accumulator, mut resolution_uj, mut timestamp) = (0u64, 0f32, 0u64);
        // SAFETY: valid processor handle, valid out-pointers.
        let rc = unsafe {
            (self.api.energy_count)(handle, &mut accumulator, &mut resolution_uj, &mut timestamp)
        };
        (rc == AMDSMI_SUCCESS && resolution_uj > 0.0)
            .then(|| accumulator as f64 * f64::from(resolution_uj) / 1e6)
    }

    fn utilization_percent(&self, device: usize) -> Option<f64> {
        let handle = *self.devices.get(device)? as AmdHandle;
        let mut usage = AmdEngineUsage::default();
        // SAFETY: as above.
        let rc = unsafe { (self.api.gpu_activity)(handle, &mut usage) };
        // The library reports a value above 100 or u32::MAX when the engine is unsupported
        (rc == AMDSMI_SUCCESS && usage.gfx_activity <= 100).then(|| f64::from(usage.gfx_activity))
    }
}

// ---------------------------------------------------------------------------------------
// Simulated backend (tests and the mock executor)
// ---------------------------------------------------------------------------------------

/// Deterministic backend: every energy read advances each device counter by `step_j`, so a
/// start/stop pair measures exactly `step_j`.
pub struct SimulatedGpu {
    name: &'static str,
    counters_mj: Vec<AtomicU64>,
    step_mj: u64,
    utilization: Vec<Option<f64>>,
    energy_supported: Vec<bool>,
}

impl SimulatedGpu {
    pub fn new(name: &'static str, devices: usize, step_j: f64, utilization: f64) -> Self {
        Self {
            name,
            counters_mj: (0..devices).map(|_| AtomicU64::new(0)).collect(),
            step_mj: (step_j * 1e3).round() as u64,
            utilization: vec![Some(utilization); devices],
            energy_supported: vec![true; devices],
        }
    }

    /// Marks a device as having no energy counter (pre-Volta NVIDIA)
    pub fn without_energy(mut self, device: usize) -> Self {
        self.energy_supported[device] = false;
        self
    }
}

impl GpuBackend for SimulatedGpu {
    fn name(&self) -> &'static str {
        self.name
    }

    fn device_count(&self) -> usize {
        self.counters_mj.len()
    }

    fn energy_joules(&self, device: usize) -> Option<f64> {
        if !*self.energy_supported.get(device)? {
            return None;
        }
        let c = self.counters_mj.get(device)?;
        let mj = c.fetch_add(self.step_mj, Ordering::Relaxed) + self.step_mj;
        Some(mj as f64 / 1e3)
    }

    fn utilization_percent(&self, device: usize) -> Option<f64> {
        *self.utilization.get(device)?
    }
}

// ---------------------------------------------------------------------------------------
// Selection and sampler
// ---------------------------------------------------------------------------------------

type Loader = fn() -> Result<Box<dyn GpuBackend>, GpuError>;

/// Loads every usable vendor backend. Both can be present (mixed machine); the error is the
/// most informative reason if none loads.
pub fn open_backends() -> Result<Vec<Box<dyn GpuBackend>>, GpuError> {
    let loaders: [Loader; 2] = [
        || NvmlBackend::load().map(|b| Box::new(b) as Box<dyn GpuBackend>),
        || AmdSmiBackend::load().map(|b| Box::new(b) as Box<dyn GpuBackend>),
    ];
    let mut backends = Vec::new();
    let mut worst: Option<GpuError> = None;
    for load in loaders {
        match load() {
            Ok(b) => backends.push(b),
            Err(e) => {
                if worst.is_none_or(|w| e.rank() > w.rank()) {
                    worst = Some(e);
                }
            }
        }
    }
    if backends.is_empty() {
        Err(worst.unwrap_or(GpuError::LibraryNotFound))
    } else {
        Ok(backends)
    }
}

/// What `perfratio.gpu` in the export reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuInfo {
    pub backends: Vec<&'static str>,
    pub devices: usize,
    pub unavailable_reason: Option<&'static str>,
}

pub fn probe() -> GpuInfo {
    match open_backends() {
        Ok(b) => GpuInfo {
            backends: b.iter().map(|b| b.name()).collect(),
            devices: b.iter().map(|b| b.device_count()).sum(),
            unavailable_reason: None,
        },
        Err(e) => GpuInfo {
            backends: vec![],
            devices: 0,
            unavailable_reason: Some(e.as_str()),
        },
    }
}

/// Wraps `inner` so that each window also reads the GPU counters. When no GPU backend can be
/// opened, warns once and returns `inner` unchanged.
pub fn with_gpu(inner: Box<dyn EnergySampler>) -> Box<dyn EnergySampler> {
    match open_backends() {
        Ok(backends) => Box::new(GpuSampler::new(inner, backends)),
        Err(e) => {
            static ONCE: std::sync::Once = std::sync::Once::new();
            ONCE.call_once(|| {
                eprintln!(
                    "Warning: --gpu: {} ({}); continuing without GPU measurements.",
                    e.message(),
                    e.as_str()
                );
            });
            inner
        }
    }
}

struct GpuDevice {
    /// `gpu:<index>`, the index counts across backends in load order
    name: String,
    backend: usize,
    local: usize,
}

/// Host sampler plus the GPU devices. `stop()` still returns the host energy only: the GPU
/// joules are reported per device in `domain_energy` and do not enter the total (RAPL
/// package energy does not include them, so the two are independent).
pub struct GpuSampler {
    inner: Box<dyn EnergySampler>,
    backends: Vec<Box<dyn GpuBackend>>,
    devices: Vec<GpuDevice>,
    start_j: Vec<Option<f64>>,
    energy: Vec<(String, f64)>,
    utilization: Vec<(String, f64)>,
}

impl GpuSampler {
    pub fn new(inner: Box<dyn EnergySampler>, backends: Vec<Box<dyn GpuBackend>>) -> Self {
        let mut devices = Vec::new();
        for (backend, b) in backends.iter().enumerate() {
            for local in 0..b.device_count() {
                devices.push(GpuDevice {
                    name: format!("gpu:{}", devices.len()),
                    backend,
                    local,
                });
            }
        }
        Self {
            inner,
            backends,
            devices,
            start_j: vec![],
            energy: vec![],
            utilization: vec![],
        }
    }

    fn read_energy(&self, d: &GpuDevice) -> Option<f64> {
        self.backends[d.backend].energy_joules(d.local)
    }
}

impl EnergySampler for GpuSampler {
    fn start(&mut self) {
        self.start_j = self.devices.iter().map(|d| self.read_energy(d)).collect();
        self.inner.start();
    }

    fn stop(&mut self) -> Option<f64> {
        let host = self.inner.stop();
        self.energy.clear();
        self.utilization.clear();
        for (d, start) in self.devices.iter().zip(&self.start_j) {
            if let (Some(start), Some(end)) = (start, self.read_energy(d)) {
                // counters never decrease; a reset (driver reload) clamps to 0
                self.energy.push((d.name.clone(), (end - start).max(0.0)));
            }
            if let Some(u) = self.backends[d.backend].utilization_percent(d.local) {
                self.utilization.push((d.name.clone(), u));
            }
        }
        host
    }

    fn is_available(&self) -> bool {
        self.inner.is_available()
    }

    fn enable_domains(&mut self) {
        self.inner.enable_domains();
    }

    fn domain_energy(&self) -> Option<Vec<(String, f64)>> {
        let mut all = self.inner.domain_energy().unwrap_or_default();
        all.extend(self.energy.iter().cloned());
        (!all.is_empty()).then_some(all)
    }

    fn gpu_utilization(&self) -> Option<Vec<(String, f64)>> {
        (!self.utilization.is_empty()).then(|| self.utilization.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::energy::MockEnergySampler;
    use std::sync::Mutex;

    // ----- fake NVML -------------------------------------------------------------------
    // Process-global state is unavoidable for `extern "C"` fns; tests that use it hold LOCK.
    static LOCK: Mutex<()> = Mutex::new(());
    static NVML_CALLS: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
    static NVML_ENERGY_MJ: AtomicU64 = AtomicU64::new(5_000);
    static NVML_DEVICES: AtomicU64 = AtomicU64::new(2);
    static NVML_INIT_RC: AtomicU64 = AtomicU64::new(0);

    unsafe extern "C" fn n_init() -> c_int {
        NVML_CALLS.lock().unwrap().push("init");
        NVML_INIT_RC.load(Ordering::Relaxed) as c_int
    }
    unsafe extern "C" fn n_shutdown() -> c_int {
        NVML_CALLS.lock().unwrap().push("shutdown");
        0
    }
    unsafe extern "C" fn n_count(out: *mut c_uint) -> c_int {
        *out = NVML_DEVICES.load(Ordering::Relaxed) as c_uint;
        0
    }
    unsafe extern "C" fn n_by_index(i: c_uint, out: *mut NvmlDevice) -> c_int {
        *out = (i as usize + 1) as NvmlDevice; // fake non-null handle
        0
    }
    unsafe extern "C" fn n_energy(dev: NvmlDevice, out: *mut u64) -> c_int {
        if dev as usize == 2 {
            return 3; // NVML_ERROR_NOT_SUPPORTED on device 1
        }
        *out = NVML_ENERGY_MJ.fetch_add(1_500, Ordering::Relaxed);
        0
    }
    unsafe extern "C" fn n_util(dev: NvmlDevice, out: *mut NvmlUtilization) -> c_int {
        *out = NvmlUtilization {
            gpu: 40 + dev as c_uint,
            memory: 10,
        };
        0
    }

    fn fake_nvml() -> NvmlApi {
        NvmlApi {
            init: n_init,
            shutdown: n_shutdown,
            device_count: n_count,
            device_by_index: n_by_index,
            total_energy_mj: n_energy,
            utilization: n_util,
        }
    }

    #[test]
    fn nvml_reads_energy_and_utilization_through_the_c_api() {
        let _g = LOCK.lock().unwrap();
        NVML_CALLS.lock().unwrap().clear();
        NVML_DEVICES.store(2, Ordering::Relaxed);
        NVML_INIT_RC.store(0, Ordering::Relaxed);
        NVML_ENERGY_MJ.store(5_000, Ordering::Relaxed);
        let b = NvmlBackend::from_api(fake_nvml()).unwrap();
        assert_eq!((b.name(), b.device_count()), ("nvml", 2));
        assert_eq!(b.energy_joules(0), Some(5.0)); // mJ to J
        assert_eq!(b.energy_joules(1), None); // not supported
        assert_eq!(b.utilization_percent(0), Some(41.0));
        assert_eq!(b.utilization_percent(5), None); // no such device
        drop(b);
        assert_eq!(*NVML_CALLS.lock().unwrap(), ["init", "shutdown"]);
    }

    #[test]
    fn nvml_init_failure_and_no_devices_are_distinguished() {
        let _g = LOCK.lock().unwrap();
        NVML_CALLS.lock().unwrap().clear();
        NVML_INIT_RC.store(9, Ordering::Relaxed); // NVML_ERROR_DRIVER_NOT_LOADED
        assert_eq!(
            NvmlBackend::from_api(fake_nvml()).err(),
            Some(GpuError::InitFailed)
        );
        NVML_INIT_RC.store(0, Ordering::Relaxed);
        NVML_DEVICES.store(0, Ordering::Relaxed);
        assert_eq!(
            NvmlBackend::from_api(fake_nvml()).err(),
            Some(GpuError::NoDevices)
        );
        NVML_DEVICES.store(2, Ordering::Relaxed);
    }

    // ----- fake AMD SMI ----------------------------------------------------------------
    unsafe extern "C" fn a_init(flags: u64) -> c_int {
        assert_eq!(flags, AMDSMI_INIT_AMD_GPUS);
        0
    }
    unsafe extern "C" fn a_shutdown() -> c_int {
        0
    }
    unsafe extern "C" fn a_sockets(n: *mut u32, out: *mut AmdHandle) -> c_int {
        if !out.is_null() {
            assert!(*n >= 1, "buffer sized from the first call");
            *out = 0x10 as AmdHandle;
        }
        *n = 1;
        0
    }
    unsafe extern "C" fn a_procs(_s: AmdHandle, n: *mut u32, out: *mut AmdHandle) -> c_int {
        if !out.is_null() {
            assert!(*n >= 2);
            *out = 0x21 as AmdHandle;
            *out.add(1) = 0x22 as AmdHandle;
        }
        *n = 2;
        0
    }
    unsafe extern "C" fn a_energy(
        h: AmdHandle,
        acc: *mut u64,
        res: *mut f32,
        ts: *mut u64,
    ) -> c_int {
        if h as usize == 0x22 {
            return 2; // AMDSMI_STATUS_NOT_SUPPORTED
        }
        *acc = 2_000_000;
        *res = 15.3; // uJ per tick
        *ts = 1;
        0
    }
    unsafe extern "C" fn a_activity(h: AmdHandle, out: *mut AmdEngineUsage) -> c_int {
        let gfx = if h as usize == 0x21 { 87 } else { u32::MAX };
        *out = AmdEngineUsage {
            gfx_activity: gfx,
            ..Default::default()
        };
        0
    }

    fn fake_amd() -> AmdSmiApi {
        AmdSmiApi {
            init: a_init,
            shut_down: a_shutdown,
            socket_handles: a_sockets,
            processor_handles: a_procs,
            energy_count: a_energy,
            gpu_activity: a_activity,
        }
    }

    #[test]
    fn amd_smi_enumerates_sockets_and_converts_the_accumulator() {
        let b = AmdSmiBackend::from_api(fake_amd()).unwrap();
        assert_eq!((b.name(), b.device_count()), ("amd_smi", 2));
        let j = b.energy_joules(0).unwrap();
        assert!((j - 2_000_000.0 * 15.3 / 1e6).abs() < 1e-3, "{j}");
        assert_eq!(b.energy_joules(1), None);
        assert_eq!(b.utilization_percent(0), Some(87.0));
        assert_eq!(b.utilization_percent(1), None, "u32::MAX means unsupported");
    }

    // ----- sampler -----------------------------------------------------------------------
    #[test]
    fn sampler_reports_gpu_domains_and_leaves_the_host_total_alone() {
        let backends: Vec<Box<dyn GpuBackend>> = vec![
            Box::new(SimulatedGpu::new("nvml", 2, 3.0, 55.0)),
            Box::new(SimulatedGpu::new("amd_smi", 1, 0.5, 10.0)),
        ];
        let mut s = GpuSampler::new(Box::new(MockEnergySampler::new()), backends);
        s.enable_domains();
        s.start();
        assert_eq!(s.stop(), Some(1.25), "host energy unchanged");
        let d = s.domain_energy().unwrap();
        let get = |n: &str| d.iter().find(|(k, _)| k == n).map(|(_, v)| *v);
        assert_eq!(get("gpu:0"), Some(3.0));
        assert_eq!(get("gpu:1"), Some(3.0));
        assert_eq!(get("gpu:2"), Some(0.5), "index counts across backends");
        assert!(get("package").is_some(), "host domains kept");
        let u = s.gpu_utilization().unwrap();
        assert_eq!(u[2], ("gpu:2".to_string(), 10.0));
    }

    #[test]
    fn devices_without_an_energy_counter_only_report_utilization() {
        let backends: Vec<Box<dyn GpuBackend>> = vec![Box::new(
            SimulatedGpu::new("nvml", 2, 1.0, 20.0).without_energy(1),
        )];
        let mut s = GpuSampler::new(Box::new(crate::energy::DummySampler::new()), backends);
        s.start();
        assert_eq!(s.stop(), None, "dummy host sampler has no total");
        let d = s.domain_energy().unwrap();
        assert_eq!(d, vec![("gpu:0".to_string(), 1.0)]);
        assert_eq!(s.gpu_utilization().unwrap().len(), 2);
    }

    #[test]
    fn a_counter_reset_clamps_to_zero() {
        struct Resets(AtomicU64);
        impl GpuBackend for Resets {
            fn name(&self) -> &'static str {
                "nvml"
            }
            fn device_count(&self) -> usize {
                1
            }
            fn energy_joules(&self, _: usize) -> Option<f64> {
                // 100 J at start, 4 J at stop (driver reload)
                Some(if self.0.fetch_add(1, Ordering::Relaxed) == 0 {
                    100.0
                } else {
                    4.0
                })
            }
            fn utilization_percent(&self, _: usize) -> Option<f64> {
                None
            }
        }
        let mut s = GpuSampler::new(
            Box::new(MockEnergySampler::new()),
            vec![Box::new(Resets(AtomicU64::new(0)))],
        );
        s.start();
        s.stop();
        assert_eq!(s.domain_energy().unwrap(), vec![("gpu:0".to_string(), 0.0)]);
        assert_eq!(s.gpu_utilization(), None);
    }

    #[test]
    fn missing_libraries_degrade_to_the_host_sampler() {
        // CI runners and developer machines without a GPU driver: the loaders must fail
        // cleanly (this is skipped where a driver is installed).
        if let Err(e) = open_backends() {
            let info = probe();
            assert!(info.backends.is_empty());
            assert_eq!(info.unavailable_reason, Some(e.as_str()));
            let s = with_gpu(Box::new(MockEnergySampler::new()));
            assert!(s.is_available());
            assert_eq!(s.domain_energy(), None);
        }
    }

    #[test]
    fn error_ranking_prefers_the_loaded_library() {
        assert!(GpuError::NoDevices.rank() > GpuError::InitFailed.rank());
        assert!(GpuError::InitFailed.rank() > GpuError::LibraryNotFound.rank());
        assert!(GpuError::LibraryNotFound.rank() > GpuError::UnsupportedPlatform.rank());
    }
}

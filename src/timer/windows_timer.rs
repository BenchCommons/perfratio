#![warn(unsafe_op_in_unsafe_fn)]

use std::{mem, os::windows::io::AsRawHandle, process, ptr};

use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE},
    System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectBasicAccountingInformation,
        JobObjectBasicAndIoAccountingInformation, QueryInformationJobObject,
        JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_BASIC_AND_IO_ACCOUNTING_INFORMATION,
    },
};

#[cfg(feature = "windows_process_extensions_main_thread_handle")]
use std::os::windows::process::ChildExt;
#[cfg(feature = "windows_process_extensions_main_thread_handle")]
use windows_sys::Win32::System::Threading::ResumeThread;

#[cfg(not(feature = "windows_process_extensions_main_thread_handle"))]
use once_cell::sync::Lazy;
#[cfg(not(feature = "windows_process_extensions_main_thread_handle"))]
use windows_sys::{
    s, w,
    Win32::{
        Foundation::{NTSTATUS, STATUS_SUCCESS},
        System::LibraryLoader::{GetModuleHandleW, GetProcAddress},
    },
};

use crate::benchmark::timing_result::ResourceCounters;
use crate::util::units::Second;

const HUNDRED_NS_PER_MS: i64 = 10;

#[cfg(not(feature = "windows_process_extensions_main_thread_handle"))]
#[allow(non_upper_case_globals)]
static NtResumeProcess: Lazy<unsafe extern "system" fn(process_handle: HANDLE) -> NTSTATUS> =
    Lazy::new(|| {
        // SAFETY: Getting the module handle for ntdll.dll is safe
        let ntdll = unsafe { GetModuleHandleW(w!("ntdll.dll")) };
        assert!(!ntdll.is_null(), "GetModuleHandleW failed");

        // SAFETY: The ntdll handle is valid
        let nt_resume_process = unsafe { GetProcAddress(ntdll, s!("NtResumeProcess")) };

        // SAFETY: We transmute to the correct function signature
        unsafe { mem::transmute(nt_resume_process.unwrap()) }
    });

/// Restrict the (still suspended) `child` to the CPUs in `mask`.
pub fn set_affinity(child: &process::Child, mask: usize) -> std::io::Result<()> {
    use windows_sys::Win32::System::Threading::SetProcessAffinityMask;

    // SAFETY: the handle belongs to our own child process, which is still alive
    // (suspended) at this point.
    let ret = unsafe { SetProcessAffinityMask(child.as_raw_handle() as HANDLE, mask) };
    if ret == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Set the priority class of the (still suspended) `child`.
pub fn set_priority_class(child: &process::Child, class: u32) -> std::io::Result<()> {
    use windows_sys::Win32::System::Threading::SetPriorityClass;

    // SAFETY: the handle belongs to our own child process, which is still alive
    // (suspended) at this point.
    let ret = unsafe { SetPriorityClass(child.as_raw_handle() as HANDLE, class) };
    if ret == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// (page fault count, peak working set in bytes) of the process, via
/// `GetProcessMemoryInfo`.
fn process_memory(process: HANDLE) -> Option<(u64, u64)> {
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };

    // SAFETY: PROCESS_MEMORY_COUNTERS is a plain C struct of integers.
    let mut counters: PROCESS_MEMORY_COUNTERS = unsafe { mem::zeroed() };
    counters.cb = mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
    // SAFETY: `process` is a handle of our child with query rights; `counters`
    // is a valid out-pointer of the size given in `cb`.
    let ok = unsafe { GetProcessMemoryInfo(process, &mut counters, counters.cb) };
    (ok != 0).then_some((
        u64::from(counters.PageFaultCount),
        counters.PeakWorkingSetSize as u64,
    ))
}

/// (read operations, write operations) of the process, via `GetProcessIoCounters`.
fn process_io_operations(process: HANDLE) -> Option<(u64, u64)> {
    use windows_sys::Win32::System::Threading::{GetProcessIoCounters, IO_COUNTERS};

    // SAFETY: IO_COUNTERS is a plain C struct of integers.
    let mut io: IO_COUNTERS = unsafe { mem::zeroed() };
    // SAFETY: `process` is a handle of our child; `io` is a valid out-pointer.
    let ok = unsafe { GetProcessIoCounters(process, &mut io) };
    (ok != 0).then_some((io.ReadOperationCount, io.WriteOperationCount))
}

pub struct CPUTimer {
    job_object: HANDLE,
}

impl CPUTimer {
    /// Assign `child` to a new job object (for CPU accounting) and resume it.
    ///
    /// # Safety
    ///
    /// `child` must have been spawned with `CREATE_SUSPENDED` and must not have
    /// been resumed yet. Otherwise the CPU time it used before being assigned to
    /// the job object is not accounted, and resuming it again changes its
    /// suspend count unexpectedly.
    pub unsafe fn start_suspended_process(child: &process::Child) -> Self {
        let child_handle = child.as_raw_handle() as HANDLE;

        // SAFETY: Creating a new job object is safe
        let job_object = unsafe { CreateJobObjectW(ptr::null_mut(), ptr::null_mut()) };
        assert!(!job_object.is_null(), "CreateJobObjectW failed");

        // SAFETY: The job object handle is valid
        let ret = unsafe { AssignProcessToJobObject(job_object, child_handle) };
        assert!(ret != 0, "AssignProcessToJobObject failed");

        #[cfg(feature = "windows_process_extensions_main_thread_handle")]
        {
            // SAFETY: The main thread handle is valid
            let ret = unsafe { ResumeThread(child.main_thread_handle().as_raw_handle() as HANDLE) };
            assert!(ret != u32::MAX, "ResumeThread failed");
        }

        #[cfg(not(feature = "windows_process_extensions_main_thread_handle"))]
        {
            // Since we can't get the main thread handle on stable rust, we use
            // the undocumented but widely known `NtResumeProcess` function to
            // resume a process by it's handle.

            // SAFETY: The process handle is valid
            let ret = unsafe { NtResumeProcess(child_handle) };
            assert!(ret == STATUS_SUCCESS, "NtResumeProcess failed");
        }

        Self { job_object }
    }

    /// CPU times, peak memory and resource counters of the job (the process and
    /// everything it started). `child` must have exited. Its handle stays valid
    /// until the `Child` is dropped, so per-process queries still work.
    pub fn stop(&self, child: &process::Child) -> (Second, Second, u64, ResourceCounters) {
        let child_handle = child.as_raw_handle() as HANDLE;
        let (page_faults, peak_working_set) = process_memory(child_handle).unzip();
        let peak_memory = peak_working_set.unwrap_or(0);

        let mut job_object_info =
            mem::MaybeUninit::<JOBOBJECT_BASIC_AND_IO_ACCOUNTING_INFORMATION>::uninit();

        // SAFETY: A valid job object got created in `start_suspended_process`
        let res = unsafe {
            QueryInformationJobObject(
                self.job_object,
                JobObjectBasicAndIoAccountingInformation,
                job_object_info.as_mut_ptr().cast(),
                mem::size_of::<JOBOBJECT_BASIC_AND_IO_ACCOUNTING_INFORMATION>() as u32,
                ptr::null_mut(),
            )
        };

        if res != 0 {
            // SAFETY: The job object info got correctly initialized
            let info = unsafe { job_object_info.assume_init() };

            // The `TotalUserTime` is "The total amount of user-mode execution time for
            // all active processes associated with the job, as well as all terminated processes no
            // longer associated with the job, in 100-nanosecond ticks."
            let user: i64 = info.BasicInfo.TotalUserTime / HUNDRED_NS_PER_MS;

            // The `TotalKernelTime` is "The total amount of kernel-mode execution time
            // for all active processes associated with the job, as well as all terminated
            // processes no longer associated with the job, in 100-nanosecond ticks."
            let kernel: i64 = info.BasicInfo.TotalKernelTime / HUNDRED_NS_PER_MS;

            // Page faults and I/O operations cover the whole job, like the times.
            let counters = ResourceCounters::from_windows(
                u64::from(info.BasicInfo.TotalPageFaultCount),
                info.IoInfo.ReadOperationCount,
                info.IoInfo.WriteOperationCount,
            );
            (
                user as f64 * 1e-6,
                kernel as f64 * 1e-6,
                peak_memory,
                counters,
            )
        } else {
            // Job accounting failed: fall back to the main process only.
            let (read_ops, write_ops) = process_io_operations(child_handle).unzip();
            let counters = ResourceCounters {
                minor_faults: page_faults,
                block_input_ops: read_ops,
                block_output_ops: write_ops,
                ..Default::default()
            };
            (0.0, 0.0, peak_memory, counters)
        }
    }

    /// Number of processes still running in the job: after the main process
    /// exited, these are descendants that outlived it.
    pub fn active_processes(&self) -> Option<u32> {
        let mut info = mem::MaybeUninit::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>::uninit();
        // SAFETY: the job object is valid; `info` is large enough for the class.
        let ok = unsafe {
            QueryInformationJobObject(
                self.job_object,
                JobObjectBasicAccountingInformation,
                info.as_mut_ptr().cast(),
                mem::size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                ptr::null_mut(),
            )
        };
        // SAFETY: initialized by the successful call above.
        (ok != 0).then(|| unsafe { info.assume_init() }.ActiveProcesses)
    }

    pub fn raw_job_handle(&self) -> HANDLE {
        self.job_object
    }
}

impl Drop for CPUTimer {
    fn drop(&mut self) {
        // SAFETY: A valid job object got created in `start_suspended_process`
        unsafe { CloseHandle(self.job_object) };
    }
}

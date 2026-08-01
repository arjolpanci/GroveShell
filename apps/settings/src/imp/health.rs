//! Per-process CPU%/RAM sampling and overall health determination for the
//! Home page (Task 9). No new IPC protocol: liveness is "is the PID still
//! present," CPU/RAM come from direct `GetProcessTimes`/
//! `GetProcessMemoryInfo` calls, and overall health additionally requires
//! a successful `host.ping` round trip.

use std::time::Duration;

/// `GetProcessTimes` reports kernel/user time in 100-nanosecond units.
/// Given two samples taken `wall_elapsed` apart, this is the standard
/// "(kernel delta + user delta) / wall delta" CPU% calculation, clamped to
/// `[0.0, 100.0 * number_of_cores]`-agnostic single-process percentage
/// (matching Task Manager's "single core = 100%" convention, not
/// normalized across cores, since there is no per-core breakdown needed
/// for a simple health display).
pub fn cpu_percent_from_times(
    kernel_before: u64,
    user_before: u64,
    kernel_after: u64,
    user_after: u64,
    wall_elapsed: Duration,
) -> f32 {
    if wall_elapsed.is_zero() {
        return 0.0;
    }
    let cpu_ticks = (kernel_after.saturating_sub(kernel_before))
        + (user_after.saturating_sub(user_before));
    let cpu_seconds = cpu_ticks as f64 / 10_000_000.0; // 100ns units -> seconds
    let wall_seconds = wall_elapsed.as_secs_f64();
    ((cpu_seconds / wall_seconds) * 100.0).max(0.0) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_cpu_time_over_one_second_is_zero_percent() {
        let pct = cpu_percent_from_times(0, 0, 0, 0, Duration::from_secs(1));
        assert_eq!(pct, 0.0);
    }

    #[test]
    fn half_a_second_of_cpu_time_over_one_wall_second_is_fifty_percent() {
        // 0.5s = 5_000_000 (100ns units), split between kernel and user.
        let pct = cpu_percent_from_times(0, 0, 2_500_000, 2_500_000, Duration::from_secs(1));
        assert!((pct - 50.0).abs() < 0.01, "expected ~50.0, got {pct}");
    }

    #[test]
    fn full_cpu_saturation_over_one_wall_second_is_one_hundred_percent() {
        let pct = cpu_percent_from_times(0, 0, 10_000_000, 0, Duration::from_secs(1));
        assert!((pct - 100.0).abs() < 0.01, "expected ~100.0, got {pct}");
    }

    #[test]
    fn zero_wall_elapsed_is_zero_percent_not_a_divide_by_zero() {
        let pct = cpu_percent_from_times(0, 0, 5_000_000, 0, Duration::ZERO);
        assert_eq!(pct, 0.0);
    }
}

pub struct ProcessSample {
    pub pid: u32,
    pub cpu_percent: f32,
    pub working_set_bytes: u64,
}

/// Takes two `GetProcessTimes` readings 200ms apart to compute a CPU%
/// snapshot, plus one `GetProcessMemoryInfo` reading for working-set
/// memory. Returns `None` if the process can't be opened (already exited,
/// or — unlikely for GroveShell's own unelevated children — permission
/// denied).
pub fn sample_process(pid: u32) -> Option<ProcessSample> {
    use windows::Win32::Foundation::{CloseHandle, FILETIME};
    use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
    use windows::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ,
    };

    fn filetime_to_u64(ft: FILETIME) -> u64 {
        ((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64
    }

    // RAII guard so `handle` is closed on every return path, including the
    // early `?` returns below if the target process exits mid-sample.
    struct HandleGuard(windows::Win32::Foundation::HANDLE);

    impl Drop for HandleGuard {
        fn drop(&mut self) {
            // SAFETY: `self.0` was opened by `OpenProcess` below and is
            // owned exclusively by this guard.
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    // SAFETY: `pid` is caller-supplied; `OpenProcess` documented-fails
    // (returns `Err`) for an invalid or inaccessible pid rather than
    // aliasing anything.
    let handle = HandleGuard(
        unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ, false, pid) }
            .ok()?,
    );

    let read_times = |h: windows::Win32::Foundation::HANDLE| -> Option<(u64, u64)> {
        let (mut creation, mut exit, mut kernel, mut user) =
            (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
        // SAFETY: `h` is the handle opened above, valid for this call;
        // every out-param is a local outliving the call.
        unsafe { GetProcessTimes(h, &mut creation, &mut exit, &mut kernel, &mut user) }.ok()?;
        Some((filetime_to_u64(kernel), filetime_to_u64(user)))
    };

    let (kernel_before, user_before) = read_times(handle.0)?;
    std::thread::sleep(Duration::from_millis(200));
    let (kernel_after, user_after) = read_times(handle.0)?;

    let mut counters = PROCESS_MEMORY_COUNTERS {
        cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        ..Default::default()
    };
    // SAFETY: `handle` is still valid; `counters` is a local outliving
    // the call.
    let working_set_bytes = unsafe {
        GetProcessMemoryInfo(handle.0, &mut counters, counters.cb)
    }
    .map(|_| counters.WorkingSetSize as u64)
    .unwrap_or(0);

    // `handle`'s `Drop` impl closes the underlying HANDLE when it goes out
    // of scope at the end of this function (or on any earlier `?` return).

    Some(ProcessSample {
        pid,
        cpu_percent: cpu_percent_from_times(kernel_before, user_before, kernel_after, user_after, Duration::from_millis(200)),
        working_set_bytes,
    })
}

/// A `host.ping` round trip within `timeout`. `groveshell_ipc::pipe`'s
/// `connect`/read/write calls are all synchronous and blocking (see
/// `crates/ipc/src/pipe.rs`), so the timeout here is a wall-clock check
/// around the whole exchange rather than a socket-level timeout — good
/// enough for a health indicator that's re-checked every couple of
/// seconds anyway.
pub fn host_ping_ok(timeout: Duration) -> bool {
    let started = std::time::Instant::now();
    let Ok(mut conn) = groveshell_ipc::pipe::connect("groveshell-host") else {
        return false;
    };
    let request = groveshell_ipc::Envelope::new(
        "groveshell-settings",
        groveshell_ipc::message_type::PING,
        serde_json::json!({}),
    );
    if groveshell_ipc::framing::write_envelope(&mut conn, &request).is_err() {
        return false;
    }
    let Ok(response) = groveshell_ipc::framing::read_envelope(&mut conn) else {
        return false;
    };
    response.message_type == groveshell_ipc::message_type::PONG && started.elapsed() <= timeout
}

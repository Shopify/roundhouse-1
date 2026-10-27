//! Opt-in wall time and peak memory measurements for large apps.

use std::sync::LazyLock;
use std::time::Instant;

static ENABLED: LazyLock<bool> = LazyLock::new(|| {
    std::env::var("ROUNDHOUSE_TIMINGS").is_ok_and(|value| value == "1" || value == "true")
});

#[cfg(target_os = "macos")]
fn peak_rss_mb() -> Option<u64> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } != 0 {
        return None;
    }
    Some(unsafe { usage.assume_init() }.ru_maxrss as u64 / (1024 * 1024))
}

#[cfg(target_os = "linux")]
fn peak_rss_mb() -> Option<u64> {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find(|line| line.starts_with("VmHWM:"))
                .and_then(|line| line.split_whitespace().nth(1)?.parse::<u64>().ok())
        })
        .map(|kb| kb / 1024)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn peak_rss_mb() -> Option<u64> {
    None
}

/// Resident anonymous memory now: the heap and stacks, without the
/// file-backed pages (mapped files, the binary) that VmHWM also counts.
#[cfg(target_os = "linux")]
fn rss_anon_mb() -> Option<u64> {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find(|line| line.starts_with("RssAnon:"))
                .and_then(|line| line.split_whitespace().nth(1)?.parse::<u64>().ok())
        })
        .map(|kb| kb / 1024)
}

#[cfg(not(target_os = "linux"))]
fn rss_anon_mb() -> Option<u64> {
    None
}

/// The measurements after a phase's time: peak RSS, then (where the
/// platform reports it) current anonymous RSS, then, with the
/// `memory-census` feature, live heap bytes and their high-water mark.
fn memory_summary() -> String {
    let mut summary = match peak_rss_mb() {
        Some(mb) => format!("peak rss {mb} MB"),
        None => "peak rss unavailable".to_string(),
    };
    if let Some(mb) = rss_anon_mb() {
        summary.push_str(&format!(", rss anon {mb} MB"));
    }
    #[cfg(feature = "memory-census")]
    summary.push_str(&format!(
        ", live heap {} MB, live peak {} MB",
        crate::memory_census::live_bytes() / 1_000_000,
        crate::memory_census::peak_bytes() / 1_000_000,
    ));
    summary
}

/// Run a phase and print measurements only when requested.
///
/// Resets the lap boundary afterward so a following `lap` does not
/// attribute this phase's duration to the next named pass.
pub fn phase<T>(name: impl std::fmt::Display, f: impl FnOnce() -> T) -> T {
    let _guard = begin(name);
    let result = f();
    lap_start();
    result
}

/// Start a timed span that prints when dropped. `None` when timings are off.
pub fn begin(name: impl std::fmt::Display) -> Option<Guard> {
    if !*ENABLED {
        return None;
    }
    Some(Guard {
        name: name.to_string(),
        start: Instant::now(),
    })
}

/// Print one `roundhouse-timing:` line that is not a span (a count, a
/// summary). Nothing when timings are off.
pub fn note(msg: impl std::fmt::Display) {
    if *ENABLED {
        eprintln!("roundhouse-timing: {msg}");
    }
}

thread_local! {
    static LAP: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) };
}

/// Start a run of laps: the next `lap` measures from here.
pub fn lap_start() {
    if *ENABLED {
        LAP.with(|lap| lap.set(Some(Instant::now())));
    }
}

/// Print the time since the previous `lap`/`lap_start` under `name`, so a
/// sequence of untimed passes names the one that is slow or never ends.
pub fn lap(name: impl std::fmt::Display) {
    if !*ENABLED {
        return;
    }
    let now = Instant::now();
    if let Some(start) = LAP.with(|lap| lap.replace(Some(now))) {
        drop(Guard { name: name.to_string(), start });
    }
}

/// Prints one `roundhouse-timing:` line on drop.
pub struct Guard {
    name: String,
    start: Instant,
}

impl Drop for Guard {
    fn drop(&mut self) {
        eprintln!(
            "roundhouse-timing: {}: {:.2}s ({})",
            self.name,
            self.start.elapsed().as_secs_f64(),
            memory_summary(),
        );
    }
}

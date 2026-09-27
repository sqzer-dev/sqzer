//! The memory budget that bounds `--jobs`, ADR-0008. Replaces the
//! decoded-pixel budget of ADR-0003 "Parallelism and memory".
//!
//! A file count alone does not bound memory, and a count of decoded
//! pixels does not either: the SSIMULACRA2 search holds about 130 bytes
//! of working planes per pixel, so seven 24-megapixel photos searched at
//! once need over 20 GB. Before a worker decodes, it reserves an estimate
//! of the file's peak memory, from the header dimensions and the work the
//! run does, and waits while the reservation would push the running total
//! over the budget. The budget is three quarters of the memory available
//! when the run starts, and never more than the ADR-0003 pixel rule
//! allowed. A file whose dimensions cannot be read reserves the estimate
//! for a whole `--max-pixels` image, so it runs alone.

#[cfg(target_os = "linux")]
use std::path::Path;
use std::sync::{Condvar, Mutex};

use sqzer::core::params::Resize;
use sysinfo::System;

/// Bytes per decoded pixel: the samples plus the decoder's scratch.
/// Measured at 6 on a 24.5-megapixel HEIC (ADR-0008).
const DECODE_BYTES_PER_PIXEL: u64 = 8;

/// What each file goes through after decoding, which sets its cost per
/// output pixel on top of the decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Work {
    /// `--dry-run`: decode and probe, no encode.
    Plan,
    /// One encode at an explicit quality, lossless, or `--fast`.
    Encode,
    /// The SSIMULACRA2 target search: up to six encodes, each decoded
    /// again and scored against a precomputed reference.
    Search,
}

impl Work {
    /// Bytes per output pixel beyond the decode. `Encode` covers the
    /// hungriest encoder measured (AVIF, 21 in total), `Search` the
    /// search at 135 in total, both with headroom (ADR-0008).
    const fn bytes_per_pixel(self) -> u64 {
        match self {
            Self::Plan => 0,
            Self::Encode => 24,
            Self::Search => 152,
        }
    }

    /// Estimated peak bytes for a file of `decoded` pixels that is
    /// encoded at `output` pixels, after any resize.
    pub const fn estimate(self, decoded: u64, output: u64) -> u64 {
        decoded
            .saturating_mul(DECODE_BYTES_PER_PIXEL)
            .saturating_add(output.saturating_mul(self.bytes_per_pixel()))
    }
}

/// Pixels the encoder gets from a `w` x `h` header after `resize`. The
/// header is read before EXIF orientation, which may swap the sides, and
/// a one-sided bound then fits a different box: the larger of the two.
pub fn output_pixels(resize: Resize, w: u32, h: u32) -> u64 {
    let fit = |w, h| {
        let (w, h) = resize.fit(w, h).unwrap_or((w, h));
        u64::from(w) * u64::from(h)
    };
    fit(w, h).max(fit(h, w))
}

/// Memory available to this process now: the system's, or the lowest
/// cgroup headroom above the process if that is lower. `None` where the
/// platform does not say.
pub fn available_memory() -> Option<u64> {
    if !sysinfo::IS_SUPPORTED_SYSTEM {
        return None;
    }
    let mut sys = System::new();
    sys.refresh_memory();
    let mut bytes = sys.available_memory();
    if let Some(c) = sys.cgroup_limits() {
        bytes = bytes.min(c.free_memory);
    }
    #[cfg(target_os = "linux")]
    if let Some(own) = std::fs::read_to_string("/proc/self/cgroup")
        .ok()
        .and_then(|s| cgroup_headroom(Path::new("/sys/fs/cgroup"), &s))
    {
        bytes = bytes.min(own);
    }
    (bytes > 0).then_some(bytes)
}

/// The lowest `memory.max` minus `memory.current` over the process's
/// cgroup v2 and its ancestors, from the contents of `/proc/self/cgroup`.
/// `sysinfo` reads only the root of the hierarchy, which is the process's
/// own cgroup inside a container but not in a limited cgroup on the host,
/// `systemd-run -p MemoryMax=` for one (ADR-0008).
#[cfg(target_os = "linux")]
fn cgroup_headroom(root: &Path, proc_cgroup: &str) -> Option<u64> {
    fn read(dir: &Path, file: &str) -> Option<u64> {
        std::fs::read_to_string(dir.join(file))
            .ok()?
            .trim()
            .parse()
            .ok()
    }
    let own = proc_cgroup.lines().find_map(|l| l.strip_prefix("0::"))?;
    let mut dir = root.join(own.trim().trim_start_matches('/'));
    let mut lowest: Option<u64> = None;
    loop {
        // `memory.max` reads `max` where there is no limit, which does not
        // parse and so does not count.
        if let (Some(max), Some(current)) = (read(&dir, "memory.max"), read(&dir, "memory.current"))
        {
            let free = max.saturating_sub(current);
            lowest = Some(lowest.map_or(free, |l| l.min(free)));
        }
        if dir == root || !dir.pop() {
            return lowest;
        }
    }
}

/// Shared budget, one per run.
#[derive(Debug)]
pub struct MemoryBudget {
    limit: u64,
    used: Mutex<u64>,
    freed: Condvar,
}

/// A reservation. Dropping it returns the bytes to the budget.
#[derive(Debug)]
pub struct Reservation<'a> {
    budget: &'a MemoryBudget,
    bytes: u64,
}

impl MemoryBudget {
    /// The ADR-0008 rule: three quarters of `available` bytes, capped by
    /// the ADR-0003 pixel rule costed as `work`, so a lowered
    /// `--max-pixels` still admits no more files than it did. The pixel
    /// rule is `max_pixels * jobs / 4` pixels, never less than one image
    /// at the limit, and stands alone where the platform does not report
    /// memory.
    pub fn for_run(available: Option<u64>, max_pixels: u64, jobs: usize, work: Work) -> Self {
        let jobs = u64::try_from(jobs).unwrap_or(u64::MAX);
        let pixels = max_pixels
            .saturating_mul(jobs)
            .checked_div(4)
            .unwrap_or(max_pixels)
            .max(max_pixels);
        let by_pixels = work.estimate(pixels, pixels);
        Self::new(available.map_or(by_pixels, |bytes| (bytes / 4 * 3).min(by_pixels)))
    }

    /// A budget of exactly `limit` bytes.
    pub fn new(limit: u64) -> Self {
        Self {
            limit,
            used: Mutex::new(0),
            freed: Condvar::new(),
        }
    }

    /// Bytes the budget holds.
    pub fn limit(&self) -> u64 {
        self.limit
    }

    /// Bytes reserved right now.
    #[cfg(test)]
    pub fn used(&self) -> u64 {
        *self
            .used
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Wait until `bytes` fit, then reserve them. A reservation larger
    /// than the whole budget is granted as soon as nothing else is
    /// running, so a single oversized input still proceeds (and is then
    /// refused by the decoder's own `max_pixels` check if it is over
    /// that, not here).
    pub fn reserve(&self, bytes: u64) -> Reservation<'_> {
        let mut used = self
            .used
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while *used > 0 && used.saturating_add(bytes) > self.limit {
            used = self
                .freed
                .wait(used)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        *used = used.saturating_add(bytes);
        Reservation {
            budget: self,
            bytes,
        }
    }
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        let mut used = self
            .budget
            .used
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *used = used.saturating_sub(self.bytes);
        drop(used);
        self.budget.freed.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    #[test]
    fn limit_is_three_quarters_of_available_memory() {
        let b = MemoryBudget::for_run(Some(8_000), 100, 8, Work::Search);
        assert_eq!(b.limit(), 6_000);
    }

    #[test]
    fn the_pixel_rule_caps_the_memory_rule() {
        // --max-pixels 30M and eight jobs admitted two 24-megapixel files
        // under ADR-0003, and admit no more on a machine with 64 GB free.
        let b = MemoryBudget::for_run(Some(64_000_000_000), 30_000_000, 8, Work::Search);
        let file = Work::Search.estimate(24_000_000, 24_000_000);
        assert_eq!(b.limit(), Work::Search.estimate(60_000_000, 60_000_000));
        assert!(2 * file <= b.limit() && 3 * file > b.limit());
    }

    #[test]
    fn output_pixels_cover_either_orientation() {
        let one_sided = Resize {
            max_width: Some(1000),
            max_height: None,
        };
        // A 2000x1000 header rotated to 1000x2000 is encoded whole.
        assert_eq!(output_pixels(one_sided, 2000, 1000), 2_000_000);
        assert_eq!(output_pixels(one_sided, 1000, 2000), 2_000_000);
        assert_eq!(output_pixels(Resize::default(), 30, 20), 600);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn cgroup_headroom_is_the_lowest_over_the_ancestors() {
        let root = std::env::temp_dir().join(format!("sqzer-cgroup-{}", std::process::id()));
        let leaf = root.join("user.slice/run-1.service");
        std::fs::create_dir_all(&leaf).unwrap();
        let write = |dir: &Path, max: &str, current: &str| {
            std::fs::write(dir.join("memory.max"), max).unwrap();
            std::fs::write(dir.join("memory.current"), current).unwrap();
        };
        write(&root.join("user.slice"), "max\n", "900\n");
        write(&leaf, "4000\n", "1000\n");
        assert_eq!(
            cgroup_headroom(&root, "0::/user.slice/run-1.service\n"),
            Some(3000)
        );
        // A tighter ancestor wins.
        write(&root.join("user.slice"), "2000\n", "900\n");
        assert_eq!(
            cgroup_headroom(&root, "0::/user.slice/run-1.service\n"),
            Some(1100)
        );
        // No limit anywhere, and no v2 line.
        write(&root.join("user.slice"), "max\n", "900\n");
        write(&leaf, "max\n", "1000\n");
        assert_eq!(
            cgroup_headroom(&root, "0::/user.slice/run-1.service\n"),
            None
        );
        assert_eq!(cgroup_headroom(&root, "12:memory:/x\n"), None);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn without_a_memory_reading_the_pixel_rule_stands_in() {
        let per_pixel = Work::Encode.estimate(1, 1);
        let limit = |max, jobs| MemoryBudget::for_run(None, max, jobs, Work::Encode).limit();
        assert_eq!(limit(100, 8), 200 * per_pixel);
        assert_eq!(limit(100, 1), 100 * per_pixel);
        assert_eq!(limit(100, 4), 100 * per_pixel);
        assert_eq!(limit(u64::MAX, 8), u64::MAX);
    }

    #[test]
    fn estimate_orders_the_work_and_follows_the_resize() {
        let p = 24_470_208; // 5712x4284, the HEIC that took a 7.7 GB VM down
        let (plan, encode, search) = (
            Work::Plan.estimate(p, p),
            Work::Encode.estimate(p, p),
            Work::Search.estimate(p, p),
        );
        assert!(plan < encode && encode < search);
        // Above the 3.3 GB measured for the search, and two of them do not
        // fit in three quarters of 6.9 GB available.
        assert!(search > 3_300_000_000);
        assert!(2 * search > 6_900_000_000 / 4 * 3);
        // A resize to a quarter of the pixels searches a quarter of them.
        assert!(Work::Search.estimate(p, p / 4) < search / 3);
    }

    #[test]
    fn available_memory_is_reported_on_supported_systems() {
        if sysinfo::IS_SUPPORTED_SYSTEM {
            assert!(available_memory().is_some_and(|b| b > 0));
        }
    }

    #[test]
    fn reservations_add_up_and_release_on_drop() {
        let b = MemoryBudget::new(100);
        let a = b.reserve(60);
        assert_eq!(b.used(), 60);
        let c = b.reserve(30);
        assert_eq!(b.used(), 90);
        drop(a);
        assert_eq!(b.used(), 30);
        drop(c);
        assert_eq!(b.used(), 0);
    }

    #[test]
    fn an_oversized_reservation_proceeds_when_alone() {
        let b = MemoryBudget::new(10);
        let r = b.reserve(1000);
        assert_eq!(b.used(), 1000);
        drop(r);
    }

    #[test]
    fn a_second_large_reservation_waits_for_the_first() {
        let b = MemoryBudget::new(100);
        let peak = AtomicU64::new(0);
        std::thread::scope(|s| {
            for _ in 0..4 {
                s.spawn(|| {
                    let r = b.reserve(60);
                    let now = b.used();
                    peak.fetch_max(now, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(20));
                    drop(r);
                });
            }
        });
        // Two 60s never fit in 100, so at most one was ever reserved.
        assert_eq!(peak.load(Ordering::SeqCst), 60);
        assert_eq!(b.used(), 0);
    }
}

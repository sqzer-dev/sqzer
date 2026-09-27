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
//! when the run starts. A file whose dimensions cannot be read reserves
//! the estimate for a whole `--max-pixels` image, so it runs alone.

use std::sync::{Condvar, Mutex};

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

/// Memory available to this process now: the system's, or the cgroup's
/// if that is lower. `None` where the platform does not say.
pub fn available_memory() -> Option<u64> {
    if !sysinfo::IS_SUPPORTED_SYSTEM {
        return None;
    }
    let mut sys = System::new();
    sys.refresh_memory();
    let host = sys.available_memory();
    let bytes = sys
        .cgroup_limits()
        .map_or(host, |c| c.free_memory.min(host));
    (bytes > 0).then_some(bytes)
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
    /// The ADR-0008 rule: three quarters of `available` bytes. Where the
    /// platform does not report memory, the old pixel rule stands in:
    /// `max_pixels * jobs / 4` pixels, never less than one image at the
    /// limit, costed as `work`.
    pub fn for_run(available: Option<u64>, max_pixels: u64, jobs: usize, work: Work) -> Self {
        if let Some(bytes) = available {
            return Self::new(bytes / 4 * 3);
        }
        let jobs = u64::try_from(jobs).unwrap_or(u64::MAX);
        let pixels = max_pixels
            .saturating_mul(jobs)
            .checked_div(4)
            .unwrap_or(max_pixels)
            .max(max_pixels);
        Self::new(work.estimate(pixels, pixels))
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

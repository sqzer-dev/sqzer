//! The decoded-pixel budget that bounds `--jobs`, ADR-0003 "Parallelism
//! and memory".
//!
//! A file count alone does not bound memory: eight 200-megapixel inputs
//! in flight is several gigabytes of samples before any encoder runs
//! (rimage #343, #340). Before a worker decodes, it reserves the file's
//! header pixel count here and waits while the reservation would push the
//! running total over the budget. A file whose dimensions cannot be read
//! reserves the whole `--max-pixels`, so it runs alone.

use std::sync::{Condvar, Mutex};

/// Shared budget, one per run.
#[derive(Debug)]
pub struct PixelBudget {
    limit: u64,
    used: Mutex<u64>,
    freed: Condvar,
}

/// A reservation. Dropping it returns the pixels to the budget.
#[derive(Debug)]
pub struct Reservation<'a> {
    budget: &'a PixelBudget,
    pixels: u64,
}

impl PixelBudget {
    /// The ADR-0003 rule: `max_pixels * jobs / 4`, and never less than one
    /// image at the limit, so a single input always fits.
    pub fn for_run(max_pixels: u64, jobs: usize) -> Self {
        let jobs = u64::try_from(jobs).unwrap_or(u64::MAX);
        let limit = max_pixels
            .saturating_mul(jobs)
            .checked_div(4)
            .unwrap_or(max_pixels)
            .max(max_pixels);
        Self::new(limit)
    }

    /// A budget of exactly `limit` pixels.
    pub fn new(limit: u64) -> Self {
        Self {
            limit,
            used: Mutex::new(0),
            freed: Condvar::new(),
        }
    }

    /// Pixels the budget holds.
    #[cfg(test)]
    pub fn limit(&self) -> u64 {
        self.limit
    }

    /// Pixels reserved right now.
    #[cfg(test)]
    pub fn used(&self) -> u64 {
        *self
            .used
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Wait until `pixels` fit, then reserve them. A reservation larger
    /// than the whole budget is granted as soon as nothing else is
    /// running, so a single oversized input still proceeds (and is then
    /// refused by the decoder's own `max_pixels` check, not here).
    pub fn reserve(&self, pixels: u64) -> Reservation<'_> {
        let mut used = self
            .used
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while *used > 0 && used.saturating_add(pixels) > self.limit {
            used = self
                .freed
                .wait(used)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        *used = used.saturating_add(pixels);
        Reservation {
            budget: self,
            pixels,
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
        *used = used.saturating_sub(self.pixels);
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
    fn limit_follows_the_adr_formula_with_a_floor() {
        assert_eq!(PixelBudget::for_run(100, 8).limit(), 200);
        assert_eq!(PixelBudget::for_run(100, 1).limit(), 100);
        assert_eq!(PixelBudget::for_run(100, 4).limit(), 100);
        assert_eq!(PixelBudget::for_run(u64::MAX, 8).limit(), u64::MAX);
    }

    #[test]
    fn reservations_add_up_and_release_on_drop() {
        let b = PixelBudget::new(100);
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
        let b = PixelBudget::new(10);
        let r = b.reserve(1000);
        assert_eq!(b.used(), 1000);
        drop(r);
    }

    #[test]
    fn a_second_large_reservation_waits_for_the_first() {
        let b = PixelBudget::new(100);
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

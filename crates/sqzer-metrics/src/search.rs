//! The target search: bisect encoder quality until the score lands within a
//! tolerance of the target.
//!
//! Quality is the abstract `0..=100` scale of
//! [`sqzer_core::params::EncodeParams`], searched on whole numbers because
//! that is the resolution every backend's own knob has. The search assumes
//! score rises with quality, which is true up to encoder noise, and the
//! final pick is made over everything it tried rather than trusting the
//! bracket, so a non-monotonic run still returns the best candidate seen.
//!
//! Budget rule: bisection from the full range needs about seven encodes to
//! reach whole-number resolution, so the default cap of six leaves the
//! answer within two quality steps of the true minimum. When every trial so
//! far falls short and one encode is left, that encode goes to the ceiling
//! instead of the midpoint: the midpoint would be within a step or two of
//! the ceiling anyway, and trying the ceiling settles whether the target is
//! reachable at all.
//!
//! Seeding: a caller with a good guess sets [`Search::seed`] and usually
//! [`Search::seed_step`]. The first trial is the seed. While every trial so
//! far sits on the same side of the target, the next one steps away from
//! the last by the step, doubling each time, until the target is
//! bracketed; bisection then finishes inside a bracket a few steps wide
//! instead of the whole range. The calibrated tables in [`crate::seeds`]
//! supply both numbers.

use sqzer_core::codec::Encoder;
use sqzer_core::image::Image;
use sqzer_core::params::{DecodeOpts, EncodeParams, Target};
use sqzer_core::{Error, Registry, Result};

/// One quality the search tried and what it scored.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Trial {
    /// Abstract quality, a whole number.
    pub quality: f32,
    /// Metric score at that quality.
    pub score: f32,
}

/// Result of one target search.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchReport {
    /// Quality the search settled on, `0..=100`.
    pub quality: f32,
    /// Score at that quality.
    pub score: f32,
    /// Encodes performed.
    pub iterations: u8,
    /// The chosen score is at or above `target - tolerance`.
    pub reached: bool,
    /// The ceiling quality was tried and still fell short of the target.
    /// Implies `!reached`.
    pub capped: bool,
    /// Every trial, in the order made.
    pub trials: Vec<Trial>,
}

/// What a search hands back: the chosen candidate and how it got there.
#[derive(Debug, Clone, PartialEq)]
pub struct Found<T> {
    /// The output of the chosen trial.
    pub output: T,
    /// The search report.
    pub report: SearchReport,
}

/// Search settings.
#[derive(Debug, Clone, PartialEq)]
pub struct Search {
    /// Score to aim for, on the metric's scale.
    pub target: f32,
    /// A score within this distance of `target` ends the search. A score
    /// at or above `target - tolerance` counts as reaching the target.
    pub tolerance: f32,
    /// Encode budget. The search never performs more encodes than this.
    pub max_encodes: u8,
    /// Lowest quality to consider.
    pub floor: f32,
    /// Highest quality to consider.
    pub ceiling: f32,
    /// First quality to try. `None` starts at the midpoint of the range.
    pub seed: Option<f32>,
    /// Distance of the second trial from the seed when the seed misses.
    /// Doubles on every further miss on the same side. `None` bisects
    /// toward the range end instead, which makes a seed cost nothing over
    /// an unseeded search but also gain little unless it lands within the
    /// tolerance. With a step, a seed close to the answer saves encodes
    /// and a seed far from it spends a few on catching up.
    pub seed_step: Option<f32>,
}

impl Default for Search {
    /// The `web` preset: target 70, tolerance 1, six encodes.
    fn default() -> Self {
        Self {
            target: 70.0,
            tolerance: 1.0,
            max_encodes: 6,
            floor: 1.0,
            ceiling: 100.0,
            seed: None,
            seed_step: None,
        }
    }
}

/// The candidate the search is currently holding.
struct Candidate<T> {
    quality: f32,
    score: f32,
    reaches: bool,
    output: T,
}

impl Search {
    /// Default settings aimed at `target`.
    #[must_use]
    pub fn new(target: f32) -> Self {
        Self {
            target,
            ..Self::default()
        }
    }

    /// Check the settings make sense.
    ///
    /// # Errors
    /// [`Error::InvalidParams`] for a non-finite target, tolerance or seed,
    /// a negative tolerance, a seed step that is not positive, a zero
    /// encode budget, or a range that is not inside `0..=100` with the
    /// floor at or below the ceiling.
    pub fn validate(&self) -> Result<()> {
        let bad = |what: &str| Err(Error::InvalidParams(format!("search: {what}")));
        if !self.target.is_finite() {
            return bad("target must be a finite number");
        }
        if !self.tolerance.is_finite() || self.tolerance < 0.0 {
            return bad("tolerance must be a finite, non-negative number");
        }
        if self.max_encodes == 0 {
            return bad("the encode budget must be at least 1");
        }
        let range_ok = self.floor.is_finite()
            && self.ceiling.is_finite()
            && (0.0..=100.0).contains(&self.floor)
            && (0.0..=100.0).contains(&self.ceiling)
            && self.floor <= self.ceiling;
        if !range_ok {
            return bad("quality range must lie within 0..=100 with floor <= ceiling");
        }
        if self.seed.is_some_and(|s| !s.is_finite()) {
            return bad("seed must be a finite number");
        }
        if self.seed_step.is_some_and(|s| !s.is_finite() || s <= 0.0) {
            return bad("seed step must be a finite, positive number");
        }
        Ok(())
    }

    /// Run the search over `trial`, which encodes at the given quality and
    /// returns the encoded output together with its score.
    ///
    /// # Errors
    /// [`Error::InvalidParams`] from [`Search::validate`], or the first
    /// error `trial` returns.
    pub fn run<T>(&self, mut trial: impl FnMut(f32) -> Result<(T, f32)>) -> Result<Found<T>> {
        self.validate()?;
        let floor = self.floor.round();
        let ceiling = self.ceiling.round();
        // `lo` is the highest quality known to fall short, `hi` the lowest
        // known to reach; until a trial says otherwise they are the range
        // ends.
        let mut lo = floor;
        let mut hi = ceiling;
        let mut short_seen = false;
        let mut reach_seen = false;
        // Expansion step while the target is not bracketed; `None` once
        // it is, or when the caller asked for plain bisection.
        let mut step = self.seed.and(self.seed_step);
        let mut best: Option<Candidate<T>> = None;
        let mut trials = Vec::with_capacity(usize::from(self.max_encodes));

        let mut next = self
            .seed
            .map_or_else(|| midpoint(lo, hi), |s| s.round().clamp(floor, ceiling));
        for done in 0..self.max_encodes {
            if done > 0 {
                let last = self.max_encodes - done == 1;
                next = if last && !reach_seen {
                    hi
                } else if let Some(d) = step {
                    // Every trial so far is on one side of the target:
                    // step away from the last one, further each time.
                    step = Some(d * 2.0);
                    if reach_seen {
                        (next - d).round().max(lo)
                    } else {
                        (next + d).round().min(hi)
                    }
                } else {
                    midpoint(lo, hi)
                };
            }
            let (output, score) = trial(next)?;
            trials.push(Trial {
                quality: next,
                score,
            });
            let reaches = score >= self.target - self.tolerance;
            let better = match &best {
                None => true,
                Some(b) => match (reaches, b.reaches) {
                    // Among candidates that reach, the cheapest.
                    (true, true) => next < b.quality,
                    (true, false) => true,
                    (false, true) => false,
                    // Among candidates that do not, the closest; on a tie
                    // the later one, which bisection placed higher.
                    (false, false) => score >= b.score,
                },
            };
            if better {
                best = Some(Candidate {
                    quality: next,
                    score,
                    reaches,
                    output,
                });
            }
            if reaches {
                hi = next;
                reach_seen = true;
            } else {
                lo = next;
                short_seen = true;
            }
            if short_seen && reach_seen {
                step = None;
            }
            if (score - self.target).abs() <= self.tolerance || hi - lo <= 1.0 {
                break;
            }
        }

        // `validate` guarantees at least one trial ran.
        let best = best.ok_or_else(|| Error::InvalidParams("search: no trial ran".into()))?;
        // Whole numbers compare exactly.
        #[allow(clippy::float_cmp)]
        let capped = !best.reaches && trials.iter().any(|t| t.quality == ceiling);
        // At most `max_encodes` trials ran, so the count fits.
        #[allow(clippy::cast_possible_truncation)]
        let iterations = trials.len() as u8;
        Ok(Found {
            output: best.output,
            report: SearchReport {
                quality: best.quality,
                score: best.score,
                iterations,
                reached: best.reaches,
                capped,
                trials,
            },
        })
    }

    /// Search `encoder` for `image`: each trial encodes with `params` at the
    /// trial quality, decodes the result through `registry` and hands the
    /// decoded picture to `score`. The target in `params` is ignored; the
    /// search supplies its own.
    ///
    /// The candidate decode is capped at the pixel count of `image`, since
    /// a candidate of any other size cannot be scored anyway.
    ///
    /// # Errors
    /// Whatever the encoder, decoder or `score` return, and
    /// [`Error::Unsupported`] if `registry` cannot decode the encoder's own
    /// output, in which case nothing can be scored.
    pub fn encode(
        &self,
        encoder: &dyn Encoder,
        image: &Image,
        params: &EncodeParams,
        registry: &Registry,
        mut score: impl FnMut(&Image) -> Result<f32>,
    ) -> Result<Found<Vec<u8>>> {
        let opts = DecodeOpts {
            max_pixels: image.pixels(),
            ..DecodeOpts::default()
        };
        let mut params = params.clone();
        self.run(|quality| {
            params.target = Target::Quality(quality);
            let bytes = encoder.encode(image, &params)?;
            let decoded = match registry.decode(&bytes, &opts) {
                Ok(d) => d.image,
                Err(Error::UnknownFormat) => {
                    let format = encoder.caps().format;
                    return Err(Error::Unsupported {
                        format,
                        what: format!(
                            "a perceptual target: this build has no {format} decoder to \
                             score the output with"
                        ),
                    });
                }
                Err(e) => return Err(e),
            };
            let s = score(&decoded)?;
            Ok((bytes, s))
        })
    }
}

fn midpoint(lo: f32, hi: f32) -> f32 {
    lo.midpoint(hi).round()
}

#[cfg(test)]
// Qualities are whole numbers and the curves are exact: float equality is
// the intent.
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    /// Run the search over a pure score curve, returning the quality as
    /// the output so the two can be checked against each other.
    fn over(search: &Search, curve: impl Fn(f32) -> f32) -> Found<f32> {
        search.run(|q| Ok((q, curve(q)))).unwrap()
    }

    /// Lowest whole quality whose score reaches the target under the
    /// tolerance, by brute force.
    fn true_minimum(search: &Search, curve: impl Fn(f32) -> f32) -> Option<f32> {
        (1u8..=100)
            .map(f32::from)
            .find(|&q| curve(q) >= search.target - search.tolerance)
    }

    #[test]
    fn monotone_curve_lands_near_the_true_minimum() {
        let search = Search::new(70.0);
        for (name, curve) in [
            (
                "linear",
                Box::new(|q: f32| q * 0.9) as Box<dyn Fn(f32) -> f32>,
            ),
            ("steep", Box::new(|q: f32| 40.0 + q * 0.6)),
            (
                "saturating",
                Box::new(|q: f32| 100.0 - 60.0 * (-q / 30.0).exp()),
            ),
        ] {
            let found = over(&search, &curve);
            let min = true_minimum(&search, &curve).unwrap();
            let r = &found.report;
            assert!(r.reached, "{name}: {r:?}");
            assert!(!r.capped, "{name}");
            assert!(r.iterations <= 6, "{name}: {r:?}");
            assert_eq!(found.output, r.quality, "{name}: output follows quality");
            assert!(
                r.quality >= min && r.quality - min <= 2.0,
                "{name}: chose {} but the minimum is {min}",
                r.quality
            );
            assert_eq!(r.trials.len(), usize::from(r.iterations));
        }
    }

    #[test]
    fn unreachable_target_ends_at_the_ceiling() {
        let search = Search::new(70.0);
        let found = over(&search, |q| q * 0.5);
        let r = &found.report;
        assert!(!r.reached);
        assert!(r.capped);
        assert_eq!(r.quality, 100.0);
        assert_eq!(r.score, 50.0);
        assert!(r.iterations <= 6);
        assert!(r.trials.iter().any(|t| t.quality == 100.0));
    }

    #[test]
    fn budget_is_spent_on_the_ceiling_only_when_nothing_reached() {
        // Everything reaches: the ceiling must never be tried.
        let found = over(&Search::new(70.0), |_| 100.0);
        assert!(found.report.trials.iter().all(|t| t.quality < 100.0));
        // Nothing reaches until 99: the last encode goes to 100 and finds
        // it reaches, so the target is met after all.
        let found = over(&Search::new(70.0), |q| if q >= 99.0 { 80.0 } else { 10.0 });
        assert!(found.report.reached, "{:?}", found.report);
        assert!(found.report.quality >= 99.0);
    }

    #[test]
    fn flat_curve_walks_down_to_the_floor() {
        let found = over(&Search::new(70.0), |_| 100.0);
        let r = &found.report;
        assert!(r.reached);
        assert!(r.quality <= 3.0, "{r:?}");
        assert_eq!(r.iterations, 6);
        // Every trial after the first went lower.
        assert!(r.trials.windows(2).all(|w| w[1].quality < w[0].quality));
    }

    #[test]
    fn tolerance_stops_the_search_early() {
        let search = Search {
            seed: Some(70.0),
            ..Search::new(70.0)
        };
        let found = over(&search, |q| q);
        assert_eq!(found.report.iterations, 1);
        assert_eq!(found.report.quality, 70.0);
        assert!(found.report.reached);

        // Within tolerance from below counts too.
        let found = over(&search, |q| q - 0.5);
        assert_eq!(found.report.iterations, 1);
        assert!(found.report.reached);
    }

    #[test]
    fn seed_is_the_first_trial_and_is_clamped() {
        let search = Search {
            seed: Some(80.4),
            ..Search::new(70.0)
        };
        assert_eq!(over(&search, |q| q).report.trials[0].quality, 80.0);
        let search = Search {
            seed: Some(500.0),
            ..Search::new(70.0)
        };
        assert_eq!(over(&search, |q| q).report.trials[0].quality, 100.0);
    }

    #[test]
    fn seed_step_brackets_near_the_seed() {
        // Reaches 69 (target 70, tolerance 1) from quality 77 upward.
        let curve = |q: f32| q * 0.9;
        let plain = over(&Search::new(70.0), curve);
        assert_eq!(plain.report.iterations, 6);

        // A seed just below the answer: one step up brackets it.
        let from_below = Search {
            seed: Some(75.0),
            seed_step: Some(5.0),
            ..Search::new(70.0)
        };
        let found = over(&from_below, curve);
        let r = &found.report;
        assert!(r.reached, "{r:?}");
        assert_eq!(r.iterations, 3, "{r:?}");
        assert_eq!(r.quality, 78.0);
        let tried: Vec<f32> = r.trials.iter().map(|t| t.quality).collect();
        assert_eq!(tried, [75.0, 80.0, 78.0]);

        // A seed above the answer walks down with a doubling step.
        let from_above = Search {
            seed: Some(90.0),
            seed_step: Some(5.0),
            ..Search::new(70.0)
        };
        let found = over(&from_above, curve);
        let r = &found.report;
        assert!(r.reached, "{r:?}");
        assert!(r.iterations <= plain.report.iterations, "{r:?}");
        assert!(r.quality >= 77.0 && r.quality <= 79.0, "{r:?}");
        let tried: Vec<f32> = r.trials.iter().map(|t| t.quality).collect();
        assert_eq!(&tried[..3], [90.0, 85.0, 75.0]);
    }

    #[test]
    fn bad_seed_still_reaches_within_budget() {
        // The step doubles until the target is bracketed, so a seed far
        // from the answer spends its budget catching up and settles a few
        // steps above the minimum rather than failing.
        let search = Search {
            seed: Some(10.0),
            seed_step: Some(5.0),
            ..Search::new(70.0)
        };
        let found = over(&search, |q| q * 0.9);
        let r = &found.report;
        assert!(r.reached, "{r:?}");
        assert!(r.iterations <= 6);
        assert!(r.quality >= 77.0, "{r:?}");
        let tried: Vec<f32> = r.trials.iter().map(|t| t.quality).collect();
        assert_eq!(&tried[..5], [10.0, 15.0, 25.0, 45.0, 85.0]);
    }

    #[test]
    fn seed_step_stops_at_the_range_ends() {
        // Nothing reaches: the step runs into the ceiling and the search
        // reports the cap.
        let search = Search {
            seed: Some(98.0),
            seed_step: Some(5.0),
            ..Search::new(70.0)
        };
        let found = over(&search, |q| q * 0.5);
        assert!(found.report.capped);
        assert_eq!(found.report.iterations, 2);

        // Everything reaches: the step runs into the floor.
        let search = Search {
            seed: Some(3.0),
            seed_step: Some(5.0),
            ..Search::new(70.0)
        };
        let found = over(&search, |_| 100.0);
        assert_eq!(found.report.quality, 1.0);
        assert_eq!(found.report.iterations, 2);

        // A step without a seed is ignored.
        let search = Search {
            seed_step: Some(5.0),
            ..Search::new(70.0)
        };
        let found = over(&search, |q| q * 0.9);
        assert_eq!(found.report.trials[0].quality, 51.0);
        assert_eq!(found.report.trials[1].quality, 76.0);
    }

    #[test]
    fn budget_is_a_hard_cap() {
        for budget in 1..=6 {
            let search = Search {
                max_encodes: budget,
                ..Search::new(70.0)
            };
            let found = over(&search, |q| q * 0.9);
            assert!(
                found.report.iterations <= budget,
                "{budget}: {:?}",
                found.report
            );
        }
        let found = over(
            &Search {
                max_encodes: 1,
                ..Search::new(70.0)
            },
            |q| q,
        );
        assert_eq!(found.report.iterations, 1);
        assert!(!found.report.reached);
        assert!(!found.report.capped, "the ceiling was never tried");
    }

    #[test]
    fn noisy_curve_still_returns_the_best_seen() {
        // Reaches only at some qualities, and never twice in a row.
        let found = over(&Search::new(70.0), |q| {
            if q.rem_euclid(2.0) == 0.0 { 75.0 } else { 20.0 }
        });
        let r = &found.report;
        assert!(r.output_matches(), "{r:?}");
        if r.reached {
            assert!(r.score >= 69.0);
        } else {
            assert_eq!(r.score, 20.0);
        }
        // NaN scores are treated as falling short and never panic.
        let found = over(&Search::new(70.0), |_| f32::NAN);
        assert!(!found.report.reached);
        assert!(found.report.capped);
    }

    impl SearchReport {
        fn output_matches(&self) -> bool {
            self.trials
                .iter()
                .any(|t| t.quality == self.quality && t.score.to_bits() == self.score.to_bits())
        }
    }

    #[test]
    fn narrow_range_is_honoured() {
        let search = Search {
            floor: 60.0,
            ceiling: 80.0,
            ..Search::new(70.0)
        };
        let found = over(&search, |q| q);
        assert!(
            found
                .report
                .trials
                .iter()
                .all(|t| (60.0..=80.0).contains(&t.quality))
        );
        let pinned = Search {
            floor: 42.0,
            ceiling: 42.0,
            ..Search::new(70.0)
        };
        let found = over(&pinned, |q| q);
        assert_eq!(found.report.iterations, 1);
        assert_eq!(found.report.quality, 42.0);
        assert!(found.report.capped);
    }

    #[test]
    fn settings_are_validated() {
        let bad = |s: Search| {
            assert!(matches!(
                s.run(|q| Ok((q, q))),
                Err(Error::InvalidParams(_))
            ));
        };
        bad(Search::new(f32::NAN));
        bad(Search {
            tolerance: -1.0,
            ..Search::default()
        });
        bad(Search {
            max_encodes: 0,
            ..Search::default()
        });
        bad(Search {
            floor: 50.0,
            ceiling: 40.0,
            ..Search::default()
        });
        bad(Search {
            ceiling: 101.0,
            ..Search::default()
        });
        bad(Search {
            seed: Some(f32::INFINITY),
            ..Search::default()
        });
        bad(Search {
            seed: Some(50.0),
            seed_step: Some(0.0),
            ..Search::default()
        });
        bad(Search {
            seed: Some(50.0),
            seed_step: Some(f32::NAN),
            ..Search::default()
        });
    }

    #[test]
    fn trial_errors_propagate() {
        let mut calls = 0;
        let err = Search::new(70.0)
            .run(|_: f32| -> Result<((), f32)> {
                calls += 1;
                Err(Error::Codec("boom".into()))
            })
            .unwrap_err();
        assert!(matches!(err, Error::Codec(_)));
        assert_eq!(calls, 1);
    }
}

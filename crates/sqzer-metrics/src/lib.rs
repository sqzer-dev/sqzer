//! SSIMULACRA2 scoring and the bisection that turns a perceptual target into
//! an encoder quality value.
//!
//! Two pieces:
//!
//! - [`Ssimulacra2`] implements [`sqzer_core::metric::Metric`] over
//!   `fast-ssim2`. [`Reference`] precomputes the reference side once for
//!   the repeated comparisons a search makes.
//! - [`Search`] bisects encoder quality until a score lands within a
//!   tolerance of the target, capped at a fixed number of encodes.
//!   [`Search::run`] takes a closure and knows nothing about codecs;
//!   [`Search::encode`] drives a [`sqzer_core::codec::Encoder`] through a
//!   [`sqzer_core::Registry`].
//! - [`seeds`] holds the calibrated tables that tell a search where to
//!   start for a given backend and target, generated offline by
//!   `tools/calibrate`.
//!
//! The search never fails because a target is out of reach. It returns the
//! best candidate it saw and says so in the [`SearchReport`]; deciding
//! whether that deserves a warning is the caller's job.

#![cfg_attr(docsrs, feature(doc_cfg))]

pub mod search;
pub mod seeds;
pub mod ssimulacra2;

pub use search::{Found, Search, SearchReport, Trial};
pub use seeds::{Seed, SeedPoint, SeedTable};
pub use ssimulacra2::{Reference, Ssimulacra2};

//! Core types for `sqzer`: the image model, codec traits, parameters and the
//! backend registry.
//!
//! This crate contains no codecs. Backends live in `sqzer-codecs` and are
//! registered into a [`Registry`] through the traits in [`codec`].

#![cfg_attr(docsrs, feature(doc_cfg))]

pub mod codec;
pub mod content;
pub mod error;
pub mod image;
pub mod metric;
pub mod params;
pub mod registry;

pub use error::{Error, Result};
pub use registry::{Decoded, Registry};

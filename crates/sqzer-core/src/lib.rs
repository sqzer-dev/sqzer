//! Core types for `sqzer`: the image model, codec traits and the pipeline.
//!
//! This crate contains no codecs. Backends live in `sqzer-codecs` and register
//! themselves through the traits defined here.

#![cfg_attr(docsrs, feature(doc_cfg))]

pub mod codec;
pub mod error;
pub mod image;
pub mod params;

pub use error::{Error, Result};

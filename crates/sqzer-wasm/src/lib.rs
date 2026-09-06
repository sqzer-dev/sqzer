//! WASM surface. `wasm-bindgen` exports are added once the facade has a
//! working `run`. This crate exists now so CI proves the portable tier
//! builds on `wasm32-unknown-unknown` from day one.

pub use sqzer::Sqzer;

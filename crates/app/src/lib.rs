//! Shared egui application and platform transport interface.
#[cfg(not(target_arch = "wasm32"))]
pub mod native;
pub mod ui;

mod filter_builder;
mod filters;

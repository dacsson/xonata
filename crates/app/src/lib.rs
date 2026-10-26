//! Shared egui application and platform transport interface.
#[cfg(not(target_arch = "wasm32"))]
pub mod native;
pub mod ui;

mod agent_area;
pub mod chat;
mod chat_activity;
mod comments;
mod filter_builder;
mod filters;
mod theme;

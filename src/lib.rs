// Re-export modules needed by integration tests.
// The binary entry point remains in main.rs.

mod entry;
pub use entry::run_cli;

pub mod agy_store;
pub mod agy_usage;
pub mod auth;
mod cache;
mod cli;
mod color;
mod commands;
mod config;
mod error;
mod launch;
mod running_agy;
mod logging;
mod output;
mod profile;
mod signals;
mod tui;
mod update;
pub mod usage;

pub mod agy_api;

//! Resident core of Universal Peripheral Hub.
//!
//! The library part exists so the pipe server and the RPC layer can be tested
//! without spawning a process or creating a tray icon.

pub mod rpc;
pub mod selftest;
pub mod server;
pub mod service;
pub mod single;
pub mod tray;
pub mod ui;
pub mod winutil;

pub use service::{Event, Service};

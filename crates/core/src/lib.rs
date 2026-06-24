//! Core domain logic for the `aip-cli` CLI.
//!
//! The crate is deliberately split into focused, independently testable modules
//! so behaviour can be driven by tests without ever shelling out to `make`,
//! `claude`, or `grok`. Side effects flow through the [`runner::CommandRunner`]
//! trait, which has both a real and a recording implementation.

pub mod agent_state;
pub mod config;
pub mod discovery;
pub mod doctor;
pub mod hook;
pub mod ingest;
pub mod manifest;
pub mod mode_apply;
pub mod modes;
pub mod runner;
pub mod setup;
pub mod store;

pub use agent_state::{read_state, AgentPlugins};
pub use discovery::{discover_plugins, Plugin};
pub use doctor::{build_report, render as render_doctor, DoctorReport};
pub use ingest::{ingest_folder, ingest_url, Ingested};
pub use manifest::PluginManifest;
pub use mode_apply::{apply_mode, Target};
pub use modes::{resolve, Mode, ModeError, ALL_PLUGINS};
pub use runner::{CommandRunner, RecordingRunner, SystemRunner};

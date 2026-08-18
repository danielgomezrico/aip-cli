//! Core domain logic for the `aip-cli` CLI.
//!
//! The crate is deliberately split into focused, independently testable modules
//! so behaviour can be driven by tests without ever shelling out to `make`,
//! `claude`, or `grok`. Side effects flow through the [`runner::CommandRunner`]
//! trait, which has both a real and a recording implementation.

pub mod agent_state;
pub mod arg_enum;
pub mod categories;
pub mod claude_plugins;
pub mod codex_plugins;
pub mod config;
pub mod discovery;
pub mod doctor;
pub mod grok_plugins;
pub mod hook;
pub mod ingest;
pub mod manifest;
pub mod mode_apply;
pub mod modes;
pub mod remove;
pub mod removed;
pub mod runner;
pub mod setup;
pub mod store;

pub use agent_state::{read_state, AgentPlugins};
pub use arg_enum::ArgEnum;
pub use discovery::{discover_plugins, Plugin};
pub use doctor::{build_report, render as render_doctor, DoctorReport};
pub use ingest::{ingest_folder, ingest_url, Ingested};
pub use manifest::PluginManifest;
pub use mode_apply::{apply_mode, Target};
pub use modes::{
    all_plugins, builtin_metadata, catalog, get_plugin_metadata, resolve, set_overlay, Domain,
    Mode, ModeError, PluginMetadata, Role,
};
pub use removed::{is_removed, is_setup_blocked, mark_removed, REMOVED_MARKER};
pub use runner::{CommandRunner, RecordingRunner, SystemRunner};

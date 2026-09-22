//! Velra core: pure task-state logic (intents, reverts, command outcomes,
//! continuation state machine, capsule rendering) plus the SQLite event log
//! and reducer. Nothing in this crate talks to Claude Code directly.

pub mod checkpoint;
pub mod commands;
pub mod constraint;
pub mod continuation;
pub mod db;
pub mod event;
pub mod eventlog;
pub mod git;
pub mod hash;
pub mod intent;
pub mod model;
pub mod paths;
pub mod redact;
pub mod reducer;
pub mod render;
pub mod restore;
pub mod revert;
pub mod shell;
pub mod snapshot;
pub mod spool;
pub mod staging;
pub mod text;
pub mod time;
pub mod transcript;
pub mod workspace;

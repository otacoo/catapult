//! Catapult agent harness.
//!
//! Sandbox-jailed tool execution, an orchestrator loop, and ephemeral
//! subagents over any OpenAI-compatible endpoint (llama.cpp first).
//! Design notes live in `plan.md`.

pub mod client;
pub mod jsonfix;
pub mod permissions;
pub mod sandbox;
pub mod tools;

pub use sandbox::{PathJail, PathScope};

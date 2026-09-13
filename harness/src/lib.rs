//! Catapult agent harness.
//!
//! Sandbox-jailed tool execution, an orchestrator loop, and ephemeral
//! subagents over any OpenAI-compatible endpoint (llama.cpp first).
//! Design notes live in `plan.md`.

pub mod agent;
pub mod client;
pub mod jsonfix;
pub mod mcp;
pub mod permissions;
pub mod sandbox;
pub mod skills;
pub mod tools;

pub use agent::{AgentEvent, AgentRun, ApprovalGate, ApprovalRequest, Approved};
pub use sandbox::{PathJail, PathScope};

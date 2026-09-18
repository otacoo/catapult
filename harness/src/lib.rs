//! Catapult agent harness.
//!
//! Sandbox-jailed tool execution, an orchestrator loop, and ephemeral
//! subagents over any OpenAI-compatible endpoint (llama.cpp first).

pub mod agent;
pub mod agents;
pub mod client;
pub mod compact;
pub mod git;
pub mod jsonfix;
pub mod mcp;
pub mod memory;
pub mod permissions;
pub mod sandbox;
pub mod skills;
pub mod tools;

pub use agent::{AgentEvent, AgentRun, ApprovalGate, ApprovalRequest, Approved};
pub use sandbox::{PathJail, PathScope};

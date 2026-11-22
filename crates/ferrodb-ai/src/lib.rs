//! # ferrodb-ai (plan, phase 9; version 1.1, optional)
//!
//! - [`assistant`]: natural-language query assistant (Anthropic, OpenAI-compatible
//!   and Ollama providers) with schema awareness;
//! - [`agents`]: permission layer for external agents (read-only by default,
//!   consent per call, audit log);
//! - [`mcp`]: the MCP server (`rmcp`) exposing data sources to agents through
//!   the permission layer, and the `ferrodb mcp` stdio bridge.
//!
//! Everything works on top of any [`ferrodb_api::Backend`]: [`AiBackend`]
//! wraps the core (or the mock) and adds `Backend::assistant` and
//! `Backend::agents`.

pub mod agents;
pub mod assistant;
mod backend;
pub mod mcp;
#[allow(dead_code)]
mod runtime;

pub use backend::{AiBackend, AiOptions};

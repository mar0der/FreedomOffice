//! `fsheets`: a headless live client for FreedomOffice documents.
//!
//! The same code path serves the scripted multi-client soak test and agents (CLI / MCP): sign
//! in, list items, open an item's live session over the WebSocket protocol of `cosync`, keep
//! the workbook (the app's own model and formula engine) in memory, send edits and presence,
//! answer the server's snapshot requests. `work` holds what agents do with a workbook (read,
//! write, calculate, export); the CLI and the MCP server (`mcp`) are two front ends of it.

pub mod a1;
pub mod api;
pub mod comments;
pub mod config;
pub mod live;
pub mod mcp;
pub mod rng;
pub mod sample;
pub mod soak;
pub mod work;

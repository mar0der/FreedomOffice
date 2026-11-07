//! `fsheets`: a headless live client for FreedomOffice documents.
//!
//! The same code path serves the scripted multi-client soak test and agents (CLI / MCP): sign
//! in, list items, open an item's live session over the WebSocket protocol of `cosync`, keep
//! the workbook's cells in memory, send edits and presence.

pub mod api;
pub mod book;
pub mod live;
pub mod rng;
pub mod sample;
pub mod soak;

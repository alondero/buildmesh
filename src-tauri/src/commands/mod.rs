//! Tauri command modules

pub mod agent;
#[cfg(test)]
pub mod agent_tests;
pub mod attention;
pub mod checkpoint;
pub mod diff;
pub mod file_tree;
pub mod file_watcher;
pub mod git;
pub mod mcp;
pub mod pr;
pub mod mesh;
pub mod agent_node;
pub mod terminal;
pub mod test;

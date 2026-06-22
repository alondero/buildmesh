//! Tauri command modules

pub mod agent;
pub mod clipboard;
#[cfg(test)]
pub mod agent_tests;
pub mod agent_node;
pub mod agent_node_discovery;
pub mod ai_context;
pub mod attention;
pub mod build_run;
pub mod coordinator;
pub mod devices;
pub mod diff;
pub mod file_tree;
pub mod file_watcher;
pub mod frontend_log;
pub mod git;
#[cfg(test)]
pub mod git_tests;
pub mod mesh;
pub mod mesh_properties;
pub mod preferences;
pub mod pr;
pub mod project_detect;
pub mod prune;
pub mod remote;
pub mod scratchpad;
pub mod test;
pub mod usage;

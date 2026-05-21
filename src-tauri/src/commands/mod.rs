//! Tauri command modules

pub mod agent;
#[cfg(test)]
pub mod agent_tests;
pub mod attention;
pub mod build_run;
pub mod checkpoint;
pub mod diff;
pub mod file_tree;
pub mod file_watcher;
pub mod frontend_log;
pub mod git;
#[cfg(test)]
pub mod git_tests;
pub mod mesh_config;
pub mod pr;
pub mod mesh;
pub mod project_detect;
pub mod agent_node;
pub mod remote;
pub mod session_discovery;
pub mod terminal;
pub mod test;

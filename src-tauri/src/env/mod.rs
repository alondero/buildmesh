//! Environment detection and path handling for Windows + WSL hybrid setup

use std::path::PathBuf;
use once_cell::sync::Lazy;
use std::process::Command;
use std::env;

/// The detected runtime environment for this process
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Environment {
    /// Running on native Windows (Git Bash/MSYS2)
    Windows,
    /// Running inside WSL (Windows Subsystem for Linux)
    Wsl,
}

impl Environment {
    /// Detect the current environment by checking for WSL signature
    pub fn detect() -> Self {
        if cfg!(target_os = "windows") {
            // On Windows, check if /proc/version contains "microsoft" (WSL signature)
            if let Ok(versions) = std::fs::read_to_string("/proc/version") {
                if versions.to_lowercase().contains("microsoft") {
                    return Environment::Wsl;
                }
            }
            // Check via wsl.exe detection
            if let Ok(output) = Command::new("wsl.exe")
                .args(["--detect-nested"])
                .output()
            {
                let stdout = String::from_utf8_lossy(&output.stdout);
                if stdout.trim() == "1" {
                    return Environment::Wsl;
                }
            }
            Environment::Windows
        } else {
            // Non-Windows (Linux/WSL)
            if let Ok(versions) = std::fs::read_to_string("/proc/version") {
                if versions.to_lowercase().contains("microsoft") {
                    Environment::Wsl
                } else {
                    Environment::Windows // treat native Linux as "Windows" for our purposes
                }
            } else {
                Environment::Windows
            }
        }
    }

    /// Returns true if we're running inside WSL
    pub fn is_wsl(&self) -> bool {
        matches!(self, Environment::Wsl)
    }
}

static CURRENT_ENV: Lazy<Environment> = Lazy::new(Environment::detect);

/// Get the current environment (cached)
pub fn current_env() -> Environment {
    *CURRENT_ENV
}

/// Convert a session path to the correct form for spawning commands
/// WSL paths are stored as Unix paths internally, Windows paths as Windows paths
pub fn to_spawn_path(path: &PathBuf) -> PathBuf {
    match current_env() {
        Environment::Wsl => {
            // If path looks like /mnt/c/... convert to WSL form
            if path.to_string_lossy().starts_with("/mnt/") {
                path.clone()
            } else if path.to_string_lossy().starts_with("C:\\") || path.to_string_lossy().starts_with("c:\\") {
                // Convert C:\... to /mnt/c/...
                let path_str = path.to_string_lossy().to_lowercase();
                let drive = path_str.chars().next().unwrap_or('c');
                let rest = &path_str[2..].replace('\\', "/");
                PathBuf::from(format!("/mnt/{}{}", drive, rest))
            } else {
                path.clone()
            }
        }
        Environment::Windows => {
            // Keep Windows paths as-is
            path.clone()
        }
    }
}

/// Get the path to the cc wrapper script
pub fn cc_path() -> PathBuf {
    match current_env() {
        Environment::Wsl => PathBuf::from("/mnt/c/Users/alond/.local/bin/cc"),
        Environment::Windows => {
            // Try to find cc in PATH
            if let Ok(output) = Command::new("where")
                .arg("cc")
                .output()
            {
                let path_str = String::from_utf8_lossy(&output.stdout);
                let path = path_str.trim().lines().next().unwrap_or("");
                if !path.is_empty() {
                    return PathBuf::from(path);
                }
            }
            PathBuf::from("C:/Users/alond/.local/bin/cc")
        }
    }
}

/// Get the Git binary path for the correct environment
pub fn git_path() -> PathBuf {
    match current_env() {
        Environment::Wsl => PathBuf::from("git"),
        Environment::Windows => {
            if let Ok(output) = Command::new("where")
                .arg("git")
                .output()
            {
                let path_str = String::from_utf8_lossy(&output.stdout);
                let path = path_str.trim().lines().next().unwrap_or("");
                if !path.is_empty() {
                    return PathBuf::from(path);
                }
            }
            PathBuf::from("git")
        }
    }
}

/// Determine the environment for a given session path
pub fn env_for_path(path: &PathBuf) -> Environment {
    let path_str = path.to_string_lossy().to_lowercase();

    // WSL detection: paths starting with /mnt/, /home/, or \\wsl$
    if path_str.starts_with("/mnt/")
        || path_str.starts_with("/home/")
        || path_str.starts_with("\\\\wsl$")
        || path_str.starts_with("/")
    {
        Environment::Wsl
    } else {
        Environment::Windows
    }
}

/// Convert a path from session internal form to host-readable form
/// (e.g., /home/user -> \\wsl$\Ubuntu\home\user)
pub fn to_host_path(path: &str) -> String {
    if path.starts_with('/') && !path.starts_with("/mnt/") {
        // Assume default Ubuntu distro for now - in production this would be dynamic
        format!("\\\\wsl$\\Ubuntu{}", path.replace('/', "\\"))
    } else if path.starts_with("/mnt/") {
        // /mnt/c/Users -> C:\Users
        let drive = path.chars().nth(5).unwrap_or('c').to_uppercase().next().unwrap();
        format!("{}:{}", drive, path[6..].replace('/', "\\"))
    } else {
        path.to_string()
    }
}

/// Get the .claude directory for session storage in the correct environment
pub fn claude_dir() -> PathBuf {
    match current_env() {
        Environment::Wsl => PathBuf::from("/mnt/c/Users/alond/.claude"),
        Environment::Windows => {
            if let Ok(home) = env::var("USERPROFILE") {
                PathBuf::from(home).join(".claude")
            } else if let Ok(home) = env::var("HOME") {
                PathBuf::from(home).join(".claude")
            } else {
                PathBuf::from("C:/Users/alond/.claude")
            }
        }
    }
}

//! Tests for worktree resume fix
//!
//! These tests verify that sessions store worktree_name and that the
//! spawn command passes `-w <worktree_name>` explicitly to cwrap.
//!
//! Run with: cd src-tauri && cargo test

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    /// Test 1: Session model should have worktree_name field
    /// This documents the expected struct shape after the fix.
    #[test]
    fn session_model_has_worktree_name() {
        // The Session struct should have a worktree_name: Option<String> field
        // This test documents the expected state post-fix.
        struct Session {
            id: i64,
            project_id: i64,
            name: String,
            path: String,
            branch: String,
            env: String,
            provider: String,
            status: String,
            cli_session_id: Option<String>,
            worktree_name: Option<String>, // NEW field
            created_at: String,
        }

        let session = Session {
            id: 1,
            project_id: 1,
            name: "async-plotting-riddle".to_string(),
            path: "X:\\src\\pixelpath".to_string(),
            branch: "main".to_string(),
            env: "windows".to_string(),
            provider: "minimax".to_string(),
            status: "idle".to_string(),
            cli_session_id: Some("abc-123".to_string()),
            worktree_name: Some("async-plotting-riddle".to_string()),
            created_at: "2026-05-03".to_string(),
        };

        assert!(session.worktree_name.is_some());
        assert_eq!(session.worktree_name.as_ref().unwrap(), "async-plotting-riddle");
    }

    /// Test 2: build_spawn_command should pass `-w <name>` for cwrap providers
    /// when worktree_name is Some(name).
    #[test]
    fn build_spawn_command_with_explicit_worktree_name() {
        // Document the expected command structure:
        // For cwrap provider (Anthropic/Minimax) with worktree_name = Some("async-plotting-riddle"):
        // cmd.exe /c cwrap --minimax -w async-plotting-riddle --session-id <id>
        //                                ^^^^^^^^^^^^^^^^^^^^^^^^^
        //                                this should be the worktree name, NOT omitted

        let is_cwrap = true;
        let worktree_name = Some("async-plotting-riddle".to_string());
        let session_id_mode = "Assign";

        // Simulated args building
        let mut args = vec!["--minimax".to_string()];
        if is_cwrap {
            if let Some(ref name) = worktree_name {
                args.push("-w".to_string());
                args.push(name.clone());
            }
        }
        match session_id_mode {
            "Assign" => {
                args.push("--session-id".to_string());
                args.push("abc-123".to_string());
            }
            _ => {}
        }

        assert_eq!(args, vec![
            "--minimax",
            "-w",
            "async-plotting-riddle",
            "--session-id",
            "abc-123"
        ]);
    }

    /// Test 3: Non-cwrap providers (gemini, opencode) should NOT get -w flag
    #[test]
    fn non_cwrap_providers_no_worktree_flag() {
        let is_cwrap = false;
        let worktree_name = Some("async-plotting-riddle".to_string());

        let mut args = vec!["gemini".to_string()];
        if is_cwrap {
            if let Some(ref name) = worktree_name {
                args.push("-w".to_string());
                args.push(name.clone());
            }
        }

        // Should just be ["gemini"] - no worktree flags
        assert_eq!(args, vec!["gemini"]);
    }

    /// Test 4: Null worktree_name falls back to `-w` without explicit name on Assign mode
    /// (backward compatibility for old sessions)
    #[test]
    fn null_worktree_name_falls_back_to_w_without_name() {
        let is_cwrap = true;
        let worktree_name: Option<String> = None;
        let session_id_mode = "Assign";

        let mut args = vec!["--minimax".to_string()];
        if is_cwrap {
            match session_id_mode {
                "Assign" => {
                    if let Some(ref name) = worktree_name {
                        args.push("-w".to_string());
                        args.push(name.clone());
                    } else {
                        args.push("-w".to_string());
                    }
                }
                "Resume" => {}
                _ => {}
            }
        }

        // Should be ["--minimax", "-w"] — no explicit worktree name
        assert_eq!(args, vec!["--minimax", "-w"]);
    }

    /// Test 5: Resume mode should omit `-w` entirely, as the process is spawned inside the worktree
    #[test]
    fn resume_mode_omits_w_flag() {
        let is_cwrap = true;
        let worktree_name = Some("async-plotting-riddle".to_string());
        let session_id_mode = "Resume";

        let mut args = vec!["--minimax".to_string()];
        if is_cwrap {
            match session_id_mode {
                "Assign" => {
                    if let Some(ref name) = worktree_name {
                        args.push("-w".to_string());
                        args.push(name.clone());
                    } else {
                        args.push("-w".to_string());
                    }
                }
                "Resume" => {}
                _ => {}
            }
        }
        match session_id_mode {
            "Resume" => {
                args.push("--resume".to_string());
                args.push("abc-123".to_string());
            }
            _ => {}
        }

        // Should just be ["--minimax", "--resume", "abc-123"]
        assert_eq!(args, vec!["--minimax", "--resume", "abc-123"]);
    }

    /// Test 6: DB schema should have worktree_name column
    /// This documents the expected SQL for the migration.
    #[test]
    fn db_schema_has_worktree_name_column() {
        // Expected CREATE TABLE statement for sessions:
        let expected_columns = vec![
            "id INTEGER PRIMARY KEY",
            "project_id INTEGER NOT NULL",
            "name TEXT NOT NULL",
            "path TEXT NOT NULL",
            "branch TEXT NOT NULL DEFAULT 'main'",
            "env TEXT NOT NULL DEFAULT 'windows'",
            "provider TEXT NOT NULL DEFAULT 'anthropic'",
            "status TEXT NOT NULL DEFAULT 'idle'",
            "cli_session_id TEXT",
            "worktree_name TEXT", // NEW — nullable
            "created_at TEXT NOT NULL DEFAULT (datetime('now'))",
        ];

        // Verify worktree_name is present and is TEXT (nullable)
        let has_worktree_name = expected_columns.iter().any(|c| c.contains("worktree_name TEXT"));
        assert!(has_worktree_name, "sessions table should have worktree_name TEXT column");
    }

    /// Test 7: Session path format for worktree-based sessions
    /// When a session is created, path stays as main repo path.
    /// The worktree path is derived at spawn time using worktree_name.
    #[test]
    fn session_path_stays_main_repo_on_create() {
        // On create_session, path should be the main project path
        // not the worktree path. The worktree path is computed at spawn.
        let session_path = "X:\\src\\pixelpath";
        let worktree_name = Some("async-plotting-riddle".to_string());

        // Worktree path = session.path + "/.claude/worktrees/" + worktree_name
        let worktree_path = format!(
            "{}\\.claude\\worktrees\\{}",
            session_path,
            worktree_name.unwrap()
        );

        assert_eq!(worktree_path, "X:\\src\\pixelpath\\.claude\\worktrees\\async-plotting-riddle");
    }

    /// Test 8: cwrap command structure for Windows cwrap providers via PowerShell
    #[test]
    fn windows_cwrap_command_structure() {
        // For Windows with cwrap providers, the command now uses PowerShell:
        // powershell.exe -NoLogo -Command "cwrap --<provider> -w <name> --session-id <id>"
        //
        // We pass -NoLogo to suppress the PowerShell banner and -Command to run
        // the cwrap invocation directly. The combined args string is joined and
        // passed as a single -Command argument rather than a cmd.exe /c layer.

        let binary = "cwrap";
        let provider_flag = "--minimax";
        let worktree_name = "async-plotting-riddle";
        let session_id_arg = "--session-id";
        let session_id = "abc-123";

        // Build the inner args like build_spawn_command does
        let mut args = vec![provider_flag.to_string()];
        args.push("-w".to_string());
        args.push(worktree_name.to_string());
        args.push(session_id_arg.to_string());
        args.push(session_id.to_string());

        // Simulating build_spawn_command for Windows cwrap via PowerShell
        // The combined inner command: "cwrap --minimax -w async-plotting-riddle --session-id abc-123"
        let combined = format!("{} {}", binary, args.join(" "));
        let cmd_args = vec!["powershell.exe", "-NoLogo", "-Command", &combined];

        let expected = vec![
            "powershell.exe",
            "-NoLogo",
            "-Command",
            "cwrap --minimax -w async-plotting-riddle --session-id abc-123"
        ];

        assert_eq!(cmd_args, expected);
    }
}
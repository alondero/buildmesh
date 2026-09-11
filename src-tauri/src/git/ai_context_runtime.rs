//! Runtime repair for Git symlink placeholders used by Muse in WSL.
//!
//! Windows Git can check out a tracked symlink as a regular file when
//! `core.symlinks=false`. Muse needs to traverse those entries as links before
//! its workspace host starts. This module repairs only the two exact links
//! Buildmesh's AI-context portability command records, and only when every
//! identity check proves that the checkout is an untouched placeholder.

use std::path::{Path, PathBuf};

const AGENTS_PATH: &str = "AGENTS.md";
const AGENTS_TARGET: &[u8] = b"CLAUDE.md";
const SKILLS_PATH: &str = ".agents/skills";
const SKILLS_TARGET: &[u8] = b"../.claude/skills";
const TRANSACTION_PREFIX: &str = ".buildmesh-muse-context-";

/// Restore the exact AI-context symlinks needed by a Muse WSL launch.
///
/// On non-Windows hosts WSL is not a Windows interoperability launch, so this
/// is intentionally a no-op. The Windows implementation uses native NT links
/// and a recoverable sibling transaction; it never changes the Git index.
pub fn prepare_muse_context(host_path: &str) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        windows::prepare(host_path)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = host_path;
        Ok(())
    }
}

#[cfg(target_os = "windows")]
mod windows {
    use super::*;
    use std::ffi::OsStr;
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::fs::{symlink_dir, symlink_file, OpenOptionsExt};
    use uuid::Uuid;

    #[derive(Clone, Copy)]
    struct Spec {
        path: &'static str,
        target: &'static [u8],
        target_is_dir: bool,
    }

    const SPECS: [Spec; 2] = [
        Spec {
            path: AGENTS_PATH,
            target: AGENTS_TARGET,
            target_is_dir: false,
        },
        Spec {
            path: SKILLS_PATH,
            target: SKILLS_TARGET,
            target_is_dir: true,
        },
    ];

    struct Candidate {
        spec: Spec,
        path: PathBuf,
        temp: PathBuf,
        backup: PathBuf,
    }

    struct Transaction {
        path: PathBuf,
        file: Option<fs::File>,
    }

    impl Drop for Transaction {
        fn drop(&mut self) {
            let _ = self.file.take();
            let _ = fs::remove_file(&self.path);
        }
    }

    pub(super) fn prepare(host_path: &str) -> Result<(), String> {
        let repo = match git2::Repository::discover(host_path) {
            Ok(repo) => repo,
            Err(_) => return Ok(()),
        };
        let Some(root) = repo.workdir().map(Path::to_path_buf) else {
            return Ok(());
        };
        let root = root
            .canonicalize()
            .map_err(|e| format!("Muse context: cannot resolve worktree root: {e}"))?;
        let admin = repo.path().to_path_buf();
        let Some(transaction) = acquire_transaction(&admin)? else {
            return Err("Muse context repair is already running for this worktree".into());
        };
        let _transaction = transaction;
        let index = repo
            .index()
            .map_err(|e| format!("Muse context: cannot read Git index: {e}"))?;
        recover_transactions(&repo, &index, &root, &admin)?;

        let operation = Uuid::new_v4().simple().to_string();
        let mut candidates = Vec::new();
        for spec in SPECS {
            if let Some(candidate) = inspect(&repo, &index, &root, spec, &operation)? {
                candidates.push(candidate);
            }
        }
        if candidates.is_empty() {
            return Ok(());
        }

        let manifest = admin.join(format!("{TRANSACTION_PREFIX}{operation}.txn"));
        write_manifest(&manifest, &root, &candidates)?;
        let mut installed = Vec::new();
        let mut backups_clean = true;
        let result = (|| {
            for candidate in &candidates {
                create_candidate(candidate)?;
            }
            // Re-read the index after creating candidates. Candidate creation
            // is side-effect free for the tracked paths, and this catches a
            // checkout that changed while the links were being staged.
            let fresh_index = repo
                .index()
                .map_err(|e| format!("Muse context: cannot refresh Git index: {e}"))?;
            for candidate in &candidates {
                if !still_eligible(&repo, &fresh_index, candidate)? {
                    return Err(format!(
                        "Muse context changed while preparing {}",
                        candidate.spec.path
                    ));
                }
            }
            for candidate in &candidates {
                move_no_replace(&candidate.path, &candidate.backup)?;
                if !placeholder_matches(&candidate.backup, candidate.spec) {
                    let _ = move_no_replace(&candidate.backup, &candidate.path);
                    return Err(format!(
                        "Muse context changed while moving {}",
                        candidate.spec.path
                    ));
                }
                if let Err(error) = move_no_replace(&candidate.temp, &candidate.path) {
                    let _ = move_no_replace(&candidate.backup, &candidate.path);
                    return Err(error);
                }
                installed.push(candidate);
            }
            for candidate in &candidates {
                validate_installed(candidate)?;
            }
            for candidate in &candidates {
                // The links are now validated and are the committed operation.
                // A backup that cannot be removed is harmless: leave it for
                // the next recovery pass rather than rolling back after
                // another backup has already been deleted.
                if let Err(error) = fs::remove_file(&candidate.backup) {
                    backups_clean = false;
                    tracing::warn!(
                        path = %candidate.backup.display(),
                        "Muse context: leaving transaction backup for recovery: {error}"
                    );
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            rollback(&installed);
            cleanup_candidates(&candidates);
            return Err(error);
        }
        cleanup_candidates(&candidates);
        if backups_clean {
            if let Err(error) = fs::remove_file(&manifest) {
                tracing::warn!(
                    "Muse context: transaction completed but manifest cleanup failed: {error}"
                );
            }
        } else {
            tracing::warn!(
                "Muse context: transaction completed with backups retained for recovery"
            );
        }
        Ok(())
    }

    fn acquire_transaction(admin: &Path) -> Result<Option<Transaction>, String> {
        let path = admin.join(format!("{TRANSACTION_PREFIX}lock"));
        let create_lock = || {
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .share_mode(0)
                .open(&path)
        };
        match create_lock() {
            Ok(mut file) => {
                let _ = writeln!(file, "{}", std::process::id());
                file.sync_all()
                    .map_err(|e| format!("Muse context: cannot flush lock: {e}"))?;
                Ok(Some(Transaction {
                    path,
                    file: Some(file),
                }))
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                // A live transaction holds the file with share mode zero. If
                // we can open it exclusively, it is a crash residue and can
                // be removed safely; no age heuristic can mistake a
                // suspended live process for a stale owner.
                match OpenOptions::new().read(true).share_mode(0).open(&path) {
                    Ok(file) => {
                        drop(file);
                        let _ = fs::remove_file(&path);
                        match create_lock() {
                            Ok(mut file) => {
                                let _ = writeln!(file, "{}", std::process::id());
                                file.sync_all()
                                    .map_err(|e| format!("Muse context: cannot flush lock: {e}"))?;
                                Ok(Some(Transaction {
                                    path,
                                    file: Some(file),
                                }))
                            }
                            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                                Ok(None)
                            }
                            Err(error) => Err(format!(
                                "Muse context: cannot acquire transaction lock: {error}"
                            )),
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => Ok(None),
                    Err(error) => Err(format!(
                        "Muse context: cannot inspect transaction lock: {error}"
                    )),
                }
            }
            Err(error) => Err(format!(
                "Muse context: cannot acquire transaction lock: {error}"
            )),
        }
    }

    fn recover_transactions(
        repo: &git2::Repository,
        index: &git2::Index,
        root: &Path,
        admin: &Path,
    ) -> Result<(), String> {
        let entries = fs::read_dir(admin)
            .map_err(|e| format!("Muse context: cannot inspect transactions: {e}"))?;
        for entry in entries {
            let entry =
                entry.map_err(|e| format!("Muse context: cannot inspect transaction: {e}"))?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !name.starts_with(TRANSACTION_PREFIX) || !name.ends_with(".txn") {
                continue;
            }
            let operation = &name[TRANSACTION_PREFIX.len()..name.len() - 4];
            for spec in SPECS {
                let path = root.join(spec.path);
                let parent = path.parent().unwrap_or(root);
                let leaf = spec.path.rsplit('/').next().unwrap_or(spec.path);
                let candidate =
                    parent.join(format!("{TRANSACTION_PREFIX}{operation}-{leaf}.candidate"));
                let backup = parent.join(format!(
                    "{}{TRANSACTION_PREFIX}{}.backup",
                    spec.path.rsplit('/').next().unwrap_or(spec.path),
                    operation
                ));
                let candidate_exists = path_exists_any(&candidate);
                let backup_exists = path_exists_any(&backup);
                if !candidate_exists && !backup_exists {
                    continue;
                }
                if !index_matches(repo, index, spec)? {
                    return Err(format!(
                        "Muse context recovery found a changed Git index for {}; preserving transaction files",
                        spec.path
                    ));
                }
                if candidate_exists {
                    if link_matches(&candidate, spec) {
                        fs::remove_file(&candidate).map_err(|e| {
                            format!(
                                "Muse context recovery cannot remove candidate {}: {e}",
                                candidate.display()
                            )
                        })?;
                    } else {
                        return Err(format!(
                            "Muse context recovery found an occupied candidate: {}",
                            candidate.display()
                        ));
                    }
                }
                if !backup_exists {
                    continue;
                }
                if !path.exists() && !path.is_symlink() {
                    if !placeholder_matches(&backup, spec) {
                        return Err(format!(
                            "Muse context recovery found an invalid backup: {}",
                            backup.display()
                        ));
                    }
                    move_no_replace(&backup, &path)?;
                } else if link_matches(&path, spec) {
                    if !placeholder_matches(&backup, spec) {
                        return Err(format!(
                            "Muse context recovery found an invalid backup: {}",
                            backup.display()
                        ));
                    }
                    fs::remove_file(&backup).map_err(|e| {
                        format!("Muse context: cannot discard completed backup: {e}")
                    })?;
                } else {
                    return Err(format!(
                        "Muse context recovery found an occupied path: {}",
                        path.display()
                    ));
                }
            }
            let _ = fs::remove_file(entry.path());
        }
        Ok(())
    }

    fn inspect(
        repo: &git2::Repository,
        index: &git2::Index,
        root: &Path,
        spec: Spec,
        operation: &str,
    ) -> Result<Option<Candidate>, String> {
        let path = Path::new(spec.path);
        if !index_matches(repo, index, spec)? {
            return Ok(None);
        }

        let full = root.join(path);
        let metadata = match fs::symlink_metadata(&full) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(format!(
                    "Muse context: cannot inspect {}: {error}",
                    spec.path
                ));
            }
        };
        if metadata.file_type().is_symlink() || is_reparse_point(&full) {
            return Ok(None);
        }
        if !metadata.file_type().is_file() {
            return Err(format!(
                "Muse context: preserving non-file placeholder at {}",
                spec.path
            ));
        }
        if fs::read(&full).map_err(|e| e.to_string())? != spec.target {
            return Err(format!(
                "Muse context: preserving modified placeholder at {}",
                spec.path
            ));
        }

        let parent = full
            .parent()
            .ok_or_else(|| format!("Muse context: no parent for {}", spec.path))?
            .to_path_buf();
        let parent_meta = fs::symlink_metadata(&parent).map_err(|e| e.to_string())?;
        if !parent_meta.file_type().is_dir()
            || parent_meta.file_type().is_symlink()
            || is_reparse_point(&parent)
        {
            return Err(format!(
                "Muse context: preserving symlinked or invalid parent for {}",
                spec.path
            ));
        }
        let target = parent.join(std::str::from_utf8(spec.target).unwrap());
        let target_meta = fs::symlink_metadata(&target)
            .map_err(|e| format!("Muse context: target for {} is unavailable: {e}", spec.path))?;
        if is_reparse_point(&target)
            || target_meta.file_type().is_symlink()
            || target_meta.file_type().is_dir() != spec.target_is_dir
            || (!spec.target_is_dir && !target_meta.file_type().is_file())
        {
            return Err(format!(
                "Muse context: target for {} has an unsupported type",
                spec.path
            ));
        }
        let canonical_root = root.canonicalize().map_err(|e| e.to_string())?;
        let canonical_target = target.canonicalize().map_err(|e| e.to_string())?;
        if !canonical_target.starts_with(&canonical_root) {
            return Err(format!(
                "Muse context: target for {} escapes the worktree",
                spec.path
            ));
        }
        let leaf = full.file_name().unwrap().to_string_lossy();
        let temp = parent.join(format!("{TRANSACTION_PREFIX}{operation}-{leaf}.candidate"));
        let backup = parent.join(format!("{leaf}{TRANSACTION_PREFIX}{operation}.backup"));
        Ok(Some(Candidate {
            spec,
            path: full,
            temp,
            backup,
        }))
    }

    fn still_eligible(
        repo: &git2::Repository,
        index: &git2::Index,
        candidate: &Candidate,
    ) -> Result<bool, String> {
        if !index_matches(repo, index, candidate.spec)? {
            return Ok(false);
        }
        let Ok(metadata) = fs::symlink_metadata(&candidate.path) else {
            return Ok(false);
        };
        Ok(metadata.file_type().is_file()
            && !metadata.file_type().is_symlink()
            && !is_reparse_point(&candidate.path)
            && fs::read(&candidate.path).ok().as_deref() == Some(candidate.spec.target))
    }

    fn index_matches(
        repo: &git2::Repository,
        index: &git2::Index,
        spec: Spec,
    ) -> Result<bool, String> {
        let entries: Vec<_> = index
            .iter()
            .filter(|entry| entry.path == spec.path.as_bytes())
            .collect();
        if entries.len() != 1 {
            return Ok(false);
        }
        let entry = &entries[0];
        let stage = (entry.flags >> 12) & 3;
        if stage != 0 || entry.mode != 0o120000 {
            return Ok(false);
        }
        let blob = repo
            .find_blob(entry.id)
            .map_err(|e| format!("Muse context: cannot read {} from Git: {e}", spec.path))?;
        Ok(blob.content() == spec.target)
    }

    fn path_exists_any(path: &Path) -> bool {
        fs::symlink_metadata(path).is_ok()
    }

    fn placeholder_matches(path: &Path, spec: Spec) -> bool {
        let Ok(metadata) = fs::symlink_metadata(path) else {
            return false;
        };
        metadata.file_type().is_file()
            && !metadata.file_type().is_symlink()
            && !is_reparse_point(path)
            && fs::read(path).ok().as_deref() == Some(spec.target)
    }

    fn link_matches(path: &Path, spec: Spec) -> bool {
        let Ok(metadata) = fs::symlink_metadata(path) else {
            return false;
        };
        if !metadata.file_type().is_symlink() {
            return false;
        }
        fs::read_link(path)
            .ok()
            .map(|target| target.to_string_lossy().replace('\\', "/"))
            .is_some_and(|target| target == std::str::from_utf8(spec.target).unwrap())
    }

    fn create_candidate(candidate: &Candidate) -> Result<(), String> {
        if candidate.temp.exists() || candidate.backup.exists() {
            return Err(format!(
                "Muse context transaction name collision at {}",
                candidate.temp.display()
            ));
        }
        let target = std::str::from_utf8(candidate.spec.target)
            .unwrap()
            .replace('/', "\\");
        if candidate.spec.target_is_dir {
            symlink_dir(&target, &candidate.temp)
                .map_err(|e| format!("Muse context: cannot create directory link: {e}"))?;
        } else {
            symlink_file(&target, &candidate.temp)
                .map_err(|e| format!("Muse context: cannot create file link: {e}"))?;
        }
        Ok(())
    }

    fn validate_installed(candidate: &Candidate) -> Result<(), String> {
        let metadata = fs::symlink_metadata(&candidate.path).map_err(|e| e.to_string())?;
        if !metadata.file_type().is_symlink() {
            return Err(format!(
                "Muse context: installed {} is not a link",
                candidate.spec.path
            ));
        }
        let installed_target = fs::read_link(&candidate.path).map_err(|e| e.to_string())?;
        if installed_target.to_string_lossy().replace('\\', "/")
            != std::str::from_utf8(candidate.spec.target).unwrap()
        {
            return Err(format!(
                "Muse context: installed {} has the wrong target",
                candidate.spec.path
            ));
        }
        if candidate.spec.target_is_dir {
            if let Err(error) = fs::read_dir(&candidate.path) {
                return Err(format!(
                    "Muse context: cannot traverse {} -> {}: {error}",
                    candidate.spec.path,
                    candidate.path.display()
                ));
            }
        } else if fs::read(&candidate.path).is_err() {
            return Err(format!("Muse context: cannot read {}", candidate.spec.path));
        }
        Ok(())
    }

    fn rollback(installed: &[&Candidate]) {
        for candidate in installed.iter().rev() {
            // Do not unlink by pathname after a separate ownership check: an
            // editor or Git could replace the path between those operations.
            // Leave an expected link plus its manifest/backup for the next
            // recovery pass; preserve any unexpected object in place.
            if path_exists_any(&candidate.path) {
                continue;
            }
            if placeholder_matches(&candidate.backup, candidate.spec) {
                let _ = move_no_replace(&candidate.backup, &candidate.path);
            }
        }
    }

    fn cleanup_candidates(candidates: &[Candidate]) {
        for candidate in candidates {
            // Candidate names are unique, but preserve an unexpected object
            // if another process occupied one before cleanup.
            if link_matches(&candidate.temp, candidate.spec) {
                let _ = fs::remove_file(&candidate.temp);
            }
        }
    }

    fn write_manifest(path: &Path, root: &Path, candidates: &[Candidate]) -> Result<(), String> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|e| format!("Muse context: cannot create transaction manifest: {e}"))?;
        writeln!(file, "root={}", root.display()).map_err(|e| e.to_string())?;
        for candidate in candidates {
            writeln!(file, "path={}", candidate.path.display()).map_err(|e| e.to_string())?;
        }
        file.sync_all()
            .map_err(|e| format!("Muse context: cannot flush transaction manifest: {e}"))
    }

    fn move_no_replace(source: &Path, destination: &Path) -> Result<(), String> {
        let source_display = source.display().to_string();
        let destination_display = destination.display().to_string();
        let source = wide(source.as_os_str());
        let destination = wide(destination.as_os_str());
        let status = unsafe { move_file_ex(source.as_ptr(), destination.as_ptr(), 0) };
        if status == 0 {
            Err(format!(
                "cannot move {source_display} to {destination_display}: {}",
                std::io::Error::last_os_error()
            ))
        } else {
            Ok(())
        }
    }

    fn wide(value: &OsStr) -> Vec<u16> {
        value.encode_wide().chain(std::iter::once(0)).collect()
    }

    fn is_reparse_point(path: &Path) -> bool {
        let path = wide(path.as_os_str());
        let attributes = unsafe { get_file_attributes(path.as_ptr()) };
        attributes != u32::MAX && attributes & 0x400 != 0
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        #[link_name = "MoveFileExW"]
        fn move_file_ex(existing: *const u16, new: *const u16, flags: u32) -> i32;
        #[link_name = "GetFileAttributesW"]
        fn get_file_attributes(path: *const u16) -> u32;
    }
}

#[cfg(test)]
mod tests {
    #[cfg(not(target_os = "windows"))]
    #[test]
    fn non_windows_prepare_is_a_noop() {
        assert!(super::prepare_muse_context("/not-a-real-worktree").is_ok());
    }

    #[cfg(target_os = "windows")]
    mod windows_tests {
        use super::super::prepare_muse_context;
        use crate::agent::provider::{AgentProvider, Platform};
        use crate::models::EnvType;
        use std::path::Path;
        use std::process::{Command, Stdio};
        use tempfile::TempDir;

        fn git(root: &Path, args: &[&str], input: Option<&[u8]>) -> String {
            let mut command = Command::new("git");
            command
                .arg("-C")
                .arg(root)
                .args(args)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            if input.is_some() {
                command.stdin(Stdio::piped());
            }
            let mut child = command.spawn().unwrap();
            if let Some(input) = input {
                use std::io::Write;
                child.stdin.take().unwrap().write_all(input).unwrap();
            }
            let output = child.wait_with_output().unwrap();
            assert!(
                output.status.success(),
                "git {:?}: {}",
                args,
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        }

        fn fixture() -> TempDir {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path();
            std::fs::create_dir_all(root.join(".claude/skills/probe")).unwrap();
            std::fs::create_dir_all(root.join(".agents")).unwrap();
            std::fs::write(root.join("CLAUDE.md"), b"rules\n").unwrap();
            std::fs::write(root.join(".claude/skills/probe/SKILL.md"), b"skill\n").unwrap();
            std::fs::write(root.join("AGENTS.md"), super::super::AGENTS_TARGET).unwrap();
            std::fs::write(root.join(".agents/skills"), super::super::SKILLS_TARGET).unwrap();
            git(root, &["init", "-q"], None);
            git(root, &["config", "core.symlinks", "false"], None);
            git(root, &["add", "."], None);
            let agents_oid = git(
                root,
                &["hash-object", "-w", "--stdin"],
                Some(super::super::AGENTS_TARGET),
            );
            let skills_oid = git(
                root,
                &["hash-object", "-w", "--stdin"],
                Some(super::super::SKILLS_TARGET),
            );
            git(
                root,
                &[
                    "update-index",
                    "--add",
                    "--cacheinfo",
                    &format!("120000,{agents_oid},AGENTS.md"),
                ],
                None,
            );
            git(
                root,
                &[
                    "update-index",
                    "--add",
                    "--cacheinfo",
                    &format!("120000,{skills_oid},.agents/skills"),
                ],
                None,
            );
            git(
                root,
                &[
                    "-c",
                    "user.name=Buildmesh Test",
                    "-c",
                    "user.email=buildmesh-test@example.invalid",
                    "commit",
                    "-qm",
                    "fixture",
                ],
                None,
            );
            temp
        }

        #[test]
        fn repairs_exact_windows_checkout_and_is_idempotent() {
            let temp = fixture();
            prepare_muse_context(temp.path().to_str().unwrap()).unwrap();
            assert!(std::fs::symlink_metadata(temp.path().join("AGENTS.md"))
                .unwrap()
                .file_type()
                .is_symlink());
            assert!(
                std::fs::symlink_metadata(temp.path().join(".agents/skills"))
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
            assert!(git(temp.path(), &["status", "--porcelain"], None).is_empty());
            prepare_muse_context(temp.path().to_str().unwrap()).unwrap();
            assert!(git(temp.path(), &["status", "--porcelain"], None).is_empty());
        }

        #[test]
        fn preserves_authored_regular_context() {
            let temp = fixture();
            let oid = git(
                temp.path(),
                &["hash-object", "-w", "--stdin"],
                Some(super::super::AGENTS_TARGET),
            );
            git(
                temp.path(),
                &[
                    "update-index",
                    "--add",
                    "--cacheinfo",
                    &format!("100644,{oid},AGENTS.md"),
                ],
                None,
            );
            let before = std::fs::read(temp.path().join("AGENTS.md")).unwrap();
            prepare_muse_context(temp.path().to_str().unwrap()).unwrap();
            assert_eq!(
                std::fs::read(temp.path().join("AGENTS.md")).unwrap(),
                before
            );
            assert!(!std::fs::symlink_metadata(temp.path().join("AGENTS.md"))
                .unwrap()
                .file_type()
                .is_symlink());
        }

        #[test]
        fn refuses_modified_placeholder_without_partial_repair() {
            let temp = fixture();
            std::fs::write(temp.path().join("AGENTS.md"), b"user-authored pointer\n").unwrap();
            assert!(prepare_muse_context(temp.path().to_str().unwrap()).is_err());
            assert!(!std::fs::symlink_metadata(temp.path().join("AGENTS.md"))
                .unwrap()
                .file_type()
                .is_symlink());
            assert!(
                !std::fs::symlink_metadata(temp.path().join(".agents/skills"))
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
        }

        #[test]
        fn preserves_absent_alias_without_recreating_it() {
            let temp = fixture();
            std::fs::remove_file(temp.path().join("AGENTS.md")).unwrap();
            prepare_muse_context(temp.path().to_str().unwrap()).unwrap();
            assert!(!temp.path().join("AGENTS.md").exists());
            assert!(
                std::fs::symlink_metadata(temp.path().join(".agents/skills"))
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
        }

        #[test]
        fn recovery_preserves_backup_when_git_index_changed() {
            let temp = fixture();
            let root = temp.path();
            let backup = root.join(format!(
                "AGENTS.md{}deadbeef.backup",
                super::super::TRANSACTION_PREFIX
            ));
            std::fs::rename(root.join("AGENTS.md"), &backup).unwrap();
            std::fs::write(
                root.join(".git")
                    .join(format!("{}deadbeef.txn", super::super::TRANSACTION_PREFIX)),
                b"version=1\n",
            )
            .unwrap();
            let oid = git(
                root,
                &["hash-object", "-w", "--stdin"],
                Some(super::super::AGENTS_TARGET),
            );
            git(
                root,
                &[
                    "update-index",
                    "--add",
                    "--cacheinfo",
                    &format!("100644,{oid},AGENTS.md"),
                ],
                None,
            );

            assert!(prepare_muse_context(root.to_str().unwrap()).is_err());
            assert!(!root.join("AGENTS.md").exists());
            assert!(backup.exists());
        }

        #[test]
        #[ignore = "requires authenticated Muse installation in WSL"]
        fn live_muse_exec_uses_the_production_wsl_wrapper_after_repair() {
            let temp = fixture();
            prepare_muse_context(temp.path().to_str().unwrap()).unwrap();
            let guest_path_output = Command::new("wsl.exe")
                .args([
                    "-d",
                    "Ubuntu",
                    "--exec",
                    "wslpath",
                    "-u",
                    temp.path().to_str().unwrap(),
                ])
                .output()
                .unwrap();
            assert!(guest_path_output.status.success());
            let guest_path = String::from_utf8(guest_path_output.stdout)
                .unwrap()
                .trim()
                .to_string();
            let mut recipe =
                crate::agent::provider::adapters::MUSE.spawn_recipe(Platform::Linux, EnvType::Wsl);
            recipe.base_args = vec![
                "exec".into(),
                "--provider".into(),
                "echo".into(),
                "--trust-workspace".into(),
                "--disable-shell".into(),
                "--disable-write".into(),
                "--no-session-log".into(),
                "MUSE_PRODUCTION_PREFLIGHT_OK".into(),
            ];
            let command = crate::agent::spawn_environment::wrap(
                recipe,
                EnvType::Wsl,
                Some("Ubuntu"),
                None,
                &guest_path,
                0,
                false,
            );
            let argv = command.get_argv();
            let output = Command::new(&argv[0]).args(&argv[1..]).output().unwrap();
            assert!(
                output.status.success(),
                "Muse failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                String::from_utf8_lossy(&output.stdout).contains("MUSE_PRODUCTION_PREFLIGHT_OK")
            );
            assert!(std::fs::symlink_metadata(temp.path().join("AGENTS.md"))
                .unwrap()
                .file_type()
                .is_symlink());
            assert!(
                std::fs::symlink_metadata(temp.path().join(".agents/skills"))
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
            assert!(git(temp.path(), &["status", "--porcelain"], None).is_empty());
        }
    }
}

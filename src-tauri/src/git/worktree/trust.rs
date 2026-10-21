//! Exact Git ownership exceptions for linked checkouts of a trusted source.

use git2::{Config, ErrorCode, Repository};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const SAFE: &str = "safe.directory";
const OWNED: &str = "buildmesh.managedWorktreeSafeDirectory";
static CONFIG_WRITER: Mutex<()> = Mutex::new(());

pub(super) fn ensure(project_root: &str, checkout: &str) -> Result<(), String> {
    let checkout = PathBuf::from(crate::env::to_host_path(checkout));
    if !checkout.join(".git").is_file() {
        return Ok(());
    }
    match Repository::open(&checkout) {
        Ok(_) => return Ok(()),
        Err(error) if error.code() == ErrorCode::Owner => {}
        Err(error) => {
            return Err(format!(
                "Cannot open worktree {}: {error}",
                checkout.display()
            ))
        }
    }
    let source = super::super::project::open(project_root).map_err(|error| {
        format!(
            "Cannot inherit Git trust from source repository {project_root}: {error}. \
             Review its ownership and global Git safe.directory configuration, then retry"
        )
    })?;
    let _writer = CONFIG_WRITER.lock().map_err(|e| e.to_string())?;
    let mut config = global_config().map_err(|e| e.to_string())?;
    let key = authorize(&source, &checkout, &mut config)?;
    let opened = Repository::open(&checkout)
        .map_err(|e| e.to_string())
        .and_then(|repo| {
            let common = std::fs::canonicalize(repo.commondir()).map_err(|e| e.to_string())?;
            let expected = std::fs::canonicalize(source.commondir()).map_err(|e| e.to_string())?;
            if common != expected {
                return Err("Worktree repository changed during Git trust registration".to_string());
            }
            Ok(())
        });
    match opened {
        Ok(()) => Ok(()),
        Err(error) => {
            remove_owned(&mut config, &key).map_err(|cleanup| {
                format!("{error}; cannot revoke worktree Git trust: {cleanup}")
            })?;
            Err(error)
        }
    }
}

fn authorize(source: &Repository, checkout: &Path, config: &mut Config) -> Result<String, String> {
    let key = verified_worktree_key(source, checkout)?;
    register(config, &key).map_err(|error| {
        format!("Cannot register Git trust for worktree {key}: {error}; review global Git configuration and retry")
    })?;
    Ok(key)
}

fn global_config() -> Result<Config, git2::Error> {
    let config = Config::open_default()?;
    // Read every protected level so ineffective user exceptions in XDG are
    // preserved. With a Global backend present, default writes go to that
    // higher-priority level, regardless of whether XDG exists or appears later.
    config.open_level(git2::ConfigLevel::Global)?;
    Ok(config)
}

/// Capture before moving/removing a checkout so aliases resolve while it exists.
pub(super) fn key(path: &str) -> Result<String, String> {
    let path = PathBuf::from(crate::env::to_host_path(path));
    let canonical = std::fs::canonicalize(&path).or_else(|_| {
        let parent = path.parent().unwrap_or(Path::new("."));
        std::fs::canonicalize(parent)
            .map(|parent| parent.join(path.file_name().unwrap_or_default()))
    });
    let path = match canonical {
        Ok(path) => path,
        Err(_) if path.is_absolute() => path,
        Err(error) => return Err(error.to_string()),
    };
    if path.to_str().is_none() {
        return Err("Git trust requires a UTF-8 worktree path".to_string());
    }
    Ok(crate::env::canonical_host_path_string(&path).replace('\\', "/"))
}

pub(super) fn forget_key(key: &str) -> Result<(), String> {
    let _writer = CONFIG_WRITER.lock().map_err(|e| e.to_string())?;
    let mut config = global_config()
        .and_then(|config| config.open_level(git2::ConfigLevel::Global))
        .map_err(|e| e.to_string())?;
    remove_owned(&mut config, key).map_err(|e| e.to_string())
}

fn resolve_link(base: &Path, contents: &str) -> Result<PathBuf, String> {
    let raw = contents.trim();
    if raw.is_empty() {
        return Err("Empty worktree administrative link".to_string());
    }
    let path = PathBuf::from(crate::env::to_host_path(raw));
    let path = if path.is_absolute() {
        path
    } else {
        base.join(path)
    };
    std::fs::canonicalize(path).map_err(|e| e.to_string())
}

fn verified_worktree_key(source: &Repository, checkout: &Path) -> Result<String, String> {
    let validate = || -> Result<String, String> {
        let checkout = std::fs::canonicalize(checkout).map_err(|e| e.to_string())?;
        let gitfile = checkout.join(".git");
        let link = std::fs::read_to_string(&gitfile).map_err(|e| e.to_string())?;
        let target = link
            .trim()
            .strip_prefix("gitdir:")
            .ok_or("Invalid worktree .git link")?;
        let admin = resolve_link(&checkout, target)?;
        let common = std::fs::canonicalize(source.commondir()).map_err(|e| e.to_string())?;
        let registry =
            std::fs::canonicalize(common.join("worktrees")).map_err(|e| e.to_string())?;
        if admin.parent() != Some(registry.as_path()) {
            return Err(
                "Worktree admin directory is outside the source repository registry".to_string(),
            );
        }
        let shared = std::fs::read_to_string(admin.join("commondir")).map_err(|e| e.to_string())?;
        if resolve_link(&admin, &shared)? != common {
            return Err(
                "Worktree common directory does not match its source repository".to_string(),
            );
        }
        let backlink = std::fs::read_to_string(admin.join("gitdir")).map_err(|e| e.to_string())?;
        if resolve_link(&admin, &backlink)?
            != std::fs::canonicalize(&gitfile).map_err(|e| e.to_string())?
        {
            return Err(
                "Worktree administrative backpointer names a different checkout".to_string(),
            );
        }
        let name = admin
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("Invalid worktree registry name")?;
        let registered = source.find_worktree(name).map_err(|e| e.to_string())?;
        registered.validate().map_err(|e| e.to_string())?;
        if std::fs::canonicalize(registered.path()).map_err(|e| e.to_string())? != checkout {
            return Err("Source repository registered a different checkout path".to_string());
        }
        key(checkout.to_str().ok_or("Worktree path is not UTF-8")?)
    };
    validate()
        .map_err(|error| format!("Refusing to trust worktree {}: {error}", checkout.display()))
}

fn contains(config: &Config, name: &str, value: &str) -> Result<bool, git2::Error> {
    let mut entries = match config.multivar(name, None) {
        Ok(entries) => entries,
        Err(error) if error.code() == ErrorCode::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    while let Some(entry) = entries.next() {
        if entry?.value()? == value {
            return Ok(true);
        }
    }
    Ok(false)
}

fn exact(value: &str) -> String {
    format!("^{}$", regex::escape(value))
}

fn register(config: &mut Config, value: &str) -> Result<(), git2::Error> {
    if value.is_empty() || value == "*" || value.ends_with("/*") {
        return Err(git2::Error::from_str(
            "Worktree Git trust requires an exact path, not a wildcard or reset",
        ));
    }
    if contains(config, SAFE, value)? {
        return Err(git2::Error::from_str(
            "An existing safe.directory entry is ineffective; review overrides before retrying",
        ));
    }
    // Mark first: an interrupted write must not leave an untracked exception.
    config.set_multivar(OWNED, &exact(value), value)?;
    config.set_multivar(SAFE, &exact(value), value)
}

fn remove_owned(config: &mut Config, value: &str) -> Result<(), git2::Error> {
    if !contains(config, OWNED, value)? {
        return Ok(());
    }
    for name in [SAFE, OWNED] {
        match config.remove_multivar(name, &exact(value)) {
            Ok(()) => {}
            Err(error) if error.code() == ErrorCode::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;

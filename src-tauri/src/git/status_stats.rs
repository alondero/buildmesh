//! Exact counts for simple, unfiltered deltas without retaining patch lines.
//! Other deltas stay with libgit2, including its attributes and size policy.
use git2::{AttrCheckFlags, Delta, DiffDelta, FileMode, Repository};
use std::io::Read;

/// None means the caller must use libgit2's patch implementation.
pub(crate) fn simple_counts(repo: &Repository, delta: &DiffDelta<'_>) -> Option<(usize, usize)> {
    if !matches!(
        delta.status(),
        Delta::Added | Delta::Untracked | Delta::Deleted | Delta::Modified
    ) {
        return None;
    }
    // On small files the native patch is cheaper than an extra file read and
    // attribute checks. This is an optimization threshold, not a count limit:
    // returning None requests the complete native calculation.
    if delta.old_file().size().max(delta.new_file().size()) < 64 * 1024 {
        return None;
    }
    let path = delta
        .new_file()
        .path()
        .or_else(|| delta.old_file().path())?;
    // Attributes may change binary detection or clean-filter output. Defer
    // rather than duplicating libgit2's driver and filter selection rules.
    for name in [
        "diff",
        "filter",
        "text",
        "crlf",
        "eol",
        "ident",
        "working-tree-encoding",
    ] {
        if repo
            .get_attr(path, name, AttrCheckFlags::FILE_THEN_INDEX)
            .ok()?
            .is_some()
        {
            return None;
        }
    }
    let config = repo.config().ok()?;
    match config.get_string("core.autocrlf") {
        Ok(value) if value == "false" => {}
        Err(error) if error.code() == git2::ErrorCode::NotFound => {}
        _ => return None,
    }
    content_counts(repo, delta)
}

fn content_counts(repo: &Repository, delta: &DiffDelta<'_>) -> Option<(usize, usize)> {
    let old_file = delta.old_file();
    let new_file = delta.new_file();
    let path = new_file.path().or_else(|| old_file.path())?;
    // These limits select the native fallback, never an approximate count.
    // libgit2's default max_size is 512 MiB; non-regular modes also have
    // special loading rules (symlink contents, submodule summaries).
    if old_file.size().max(new_file.size()) > 0x2000_0000 {
        return None;
    }
    let regular = |mode| matches!(mode, FileMode::Blob | FileMode::BlobExecutable);
    let old = if matches!(delta.status(), Delta::Added | Delta::Untracked) {
        None
    } else {
        if !regular(old_file.mode()) {
            return None;
        }
        Some(repo.find_blob(old_file.id()).ok()?)
    };
    let old_bytes = old.as_ref().map_or(&[][..], |blob| blob.content());
    if old_bytes.len() > 0x2000_0000 {
        return None;
    }
    if delta.status() == Delta::Deleted {
        return Some(if binary(old_bytes) {
            (0, 0)
        } else {
            (0, lines(old_bytes))
        });
    }
    if !regular(new_file.mode()) {
        return None;
    }
    let absolute = repo.workdir()?.join(path);
    if !std::fs::symlink_metadata(&absolute)
        .ok()?
        .file_type()
        .is_file()
    {
        return None;
    }
    let mut file = std::fs::File::open(absolute).ok()?;
    let mut buffer = [0; 64 * 1024];
    let mut length = 0;
    let mut appended_lines = 0;
    let mut last = None;
    let mut is_binary = binary(old_bytes);
    loop {
        let read = file.read(&mut buffer).ok()?;
        if read == 0 {
            break;
        }
        let chunk = &buffer[..read];
        if length < 8000 {
            is_binary |= chunk[..read.min(8000 - length)].contains(&0);
        }
        // A binary verdict on either side suppresses both text counts.
        if is_binary {
            return Some((0, 0));
        }
        if length + read > 0x2000_0000 {
            return None;
        }
        let overlap = read.min(old_bytes.len().saturating_sub(length));
        if chunk[..overlap]
            != old_bytes[length.min(old_bytes.len())..length.min(old_bytes.len()) + overlap]
        {
            return None;
        }
        appended_lines += chunk[overlap..]
            .iter()
            .filter(|&&byte| byte == b'\n')
            .count();
        length += read;
        last = chunk.last().copied();
    }
    if is_binary {
        return Some((0, 0));
    }
    if length >= old_bytes.len() {
        if !old_bytes.is_empty() && !old_bytes.ends_with(b"\n") {
            return None;
        }
        Some((
            appended_lines + usize::from(length > old_bytes.len() && last != Some(b'\n')),
            0,
        ))
    } else {
        if length > 0 && last != Some(b'\n') {
            return None;
        }
        Some((0, lines(&old_bytes[length..])))
    }
}

fn binary(bytes: &[u8]) -> bool {
    bytes[..bytes.len().min(8000)].contains(&0)
}

fn lines(bytes: &[u8]) -> usize {
    bytes.iter().filter(|&&byte| byte == b'\n').count()
        + usize::from(!bytes.is_empty() && !bytes.ends_with(b"\n"))
}

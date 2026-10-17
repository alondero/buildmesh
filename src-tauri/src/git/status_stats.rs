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
    stream_counts(old_bytes, &mut file)
}

fn stream_counts(old_bytes: &[u8], file: &mut impl Read) -> Option<(usize, usize)> {
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

#[cfg(test)]
mod tests {
    use super::stream_counts;
    use std::io::{self, Cursor, Read};

    #[test]
    fn counts_complete_line_appends_and_truncations() {
        for (old, current, expected) in [
            (b"".as_slice(), b"".as_slice(), (0, 0)),
            (b"".as_slice(), b"one\nlast".as_slice(), (2, 0)),
            (b"one\n".as_slice(), b"one\n".as_slice(), (0, 0)),
            (b"one\n".as_slice(), b"one\ntwo\nlast".as_slice(), (2, 0)),
            (b"one\ntwo\nlast".as_slice(), b"one\n".as_slice(), (0, 2)),
            (b"one\nlast".as_slice(), b"".as_slice(), (0, 2)),
        ] {
            assert_eq!(
                stream_counts(old, &mut Cursor::new(current)),
                Some(expected)
            );
        }
    }

    #[test]
    fn interior_edits_and_partial_line_prefixes_request_native_fallback() {
        for (old, current) in [
            (b"one\nold\n".as_slice(), b"one\nnew\n".as_slice()),
            (b"one".as_slice(), b"one\ntwo\n".as_slice()),
            (b"one\ntwo\n".as_slice(), b"one\ntwo".as_slice()),
        ] {
            assert_eq!(stream_counts(old, &mut Cursor::new(current)), None);
        }
    }

    #[test]
    fn counts_across_buffer_boundaries_and_short_reads() {
        let mut before = vec![b'x'; 65534];
        before.push(b'\n');
        let mut after = before.clone();
        after.extend_from_slice(b"tail\nlast");
        for split in [1, 65535, 65536] {
            let mut reader = Cursor::new(&after[..split]).chain(Cursor::new(&after[split..]));
            assert_eq!(stream_counts(&before, &mut reader), Some((2, 0)));
        }
        assert_eq!(
            stream_counts(&after, &mut Cursor::new(&before)),
            Some((0, 2))
        );
    }

    #[test]
    fn binary_scan_ends_before_byte_8000_even_with_short_reads() {
        for (nul_at, additions, deletions) in [(7999, 0, 0), (8000, 1, 1)] {
            let mut bytes = vec![b'x'; 65536];
            bytes[nul_at] = 0;
            bytes.push(b'\n');
            for split in [1, 7999, 8000, 65535] {
                let mut reader = Cursor::new(&bytes[..split]).chain(Cursor::new(&bytes[split..]));
                assert_eq!(stream_counts(&[], &mut reader), Some((additions, 0)));
            }
            assert_eq!(
                stream_counts(&bytes, &mut io::empty()),
                Some((0, deletions))
            );
        }
    }

    #[test]
    fn read_error_requests_native_fallback_instead_of_partial_counts() {
        struct FailedRead;
        impl Read for FailedRead {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::other("injected read failure"))
            }
        }
        let mut reader = Cursor::new(b"one\n").chain(FailedRead);
        assert_eq!(stream_counts(&[], &mut reader), None);
    }
}

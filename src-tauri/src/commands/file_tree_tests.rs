use super::*;
use std::cell::RefCell;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

fn scratch() -> tempfile::TempDir {
    tempfile::tempdir().expect("temp dir")
}

fn write_file(path: &Path) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("parent dir");
    }
    fs::write(path, b"x").expect("write file");
}

fn child<'a>(node: &'a FileNode, name: &str) -> &'a FileNode {
    node.children
        .iter()
        .find(|entry| entry.name == name)
        .unwrap_or_else(|| panic!("missing child {name}"))
}

fn names(node: &FileNode) -> Vec<&str> {
    node.children
        .iter()
        .map(|entry| entry.name.as_str())
        .collect()
}

fn outline(node: &FileNode) -> String {
    let mut lines = Vec::new();
    fn walk(node: &FileNode, lines: &mut Vec<String>) {
        lines.push(format!(
            "{} {} {}",
            node.name,
            if node.is_dir { "dir" } else { "file" },
            node.path
        ));
        for child in &node.children {
            walk(child, lines);
        }
    }
    walk(node, &mut lines);
    lines.join("\n")
}

#[test]
fn listing_sorts_directories_first_then_name_and_skips_hidden() {
    let dir = scratch();
    write_file(&dir.path().join("b.txt"));
    write_file(&dir.path().join("a.txt"));
    write_file(&dir.path().join("m_dir").join("child.txt"));
    fs::create_dir(dir.path().join("a_dir")).unwrap();
    write_file(&dir.path().join(".hidden"));
    write_file(&dir.path().join(".git").join("config"));

    let node =
        list_directory_blocking(dir.path().to_string_lossy().into_owned(), Some(2)).expect("list");

    assert_eq!(names(&node), ["a_dir", "m_dir", "a.txt", "b.txt"]);
    assert!(child(&node, "a_dir").is_dir);
    assert!(child(&node, "a_dir").children.is_empty());
    assert_eq!(names(child(&node, "m_dir")), ["child.txt"]);
    assert!(!child(&node, "a.txt").is_dir);
    assert!(node
        .children
        .iter()
        .all(|entry| !entry.name.starts_with('.')));
}

#[test]
fn depth_limit_keeps_deeper_directories_closed() {
    let dir = scratch();
    write_file(&dir.path().join("a").join("b").join("c").join("leaf.txt"));
    write_file(&dir.path().join("file.txt"));

    let node =
        list_directory_blocking(dir.path().to_string_lossy().into_owned(), Some(3)).expect("list");

    let a = child(&node, "a");
    let b = child(a, "b");
    let c = child(b, "c");
    assert!(c.is_dir);
    assert!(
        c.children.is_empty(),
        "depth 3 lists c and does not open it"
    );
    assert_eq!(names(&node), ["a", "file.txt"]);

    let closed = list_directory_blocking(dir.path().to_string_lossy().into_owned(), Some(0))
        .expect("list root only");
    assert!(closed.is_dir);
    assert!(closed.children.is_empty());
}

#[test]
fn guest_prefix_replaces_the_host_prefix() {
    let dir = scratch();
    write_file(&dir.path().join("sub").join("a.txt"));
    let host_base = dir.path().to_string_lossy().into_owned();

    let node = read_dir_recursive(
        dir.path(),
        true,
        "/guest/repo",
        &host_base,
        0,
        2,
        &entry_is_directory,
    );

    assert_eq!(node.path, "/guest/repo");
    assert_eq!(child(&node, "sub").path, "/guest/repo/sub");
    assert_eq!(
        child(child(&node, "sub"), "a.txt").path,
        "/guest/repo/sub/a.txt"
    );
}

#[test]
fn unmatched_host_prefix_falls_back_to_the_host_path() {
    let dir = scratch();
    write_file(&dir.path().join("a.txt"));

    let node = read_dir_recursive(
        dir.path(),
        true,
        "/guest/repo",
        "NOT-A-PREFIX",
        0,
        1,
        &entry_is_directory,
    );

    let expected = dir
        .path()
        .join("a.txt")
        .to_string_lossy()
        .replace('\\', "/");
    assert_eq!(child(&node, "a.txt").path, expected);
}

#[test]
fn metadata_failure_sorts_and_renders_as_a_file() {
    let dir = scratch();
    fs::create_dir(dir.path().join("a_dir")).unwrap();
    write_file(&dir.path().join("z_dir").join("secret.txt"));
    write_file(&dir.path().join("m_file"));

    let classify = |path: &Path| {
        if path.file_name() == Some(OsStr::new("z_dir")) {
            // Stand-in for `Path::is_dir`'s false result on a metadata error.
            false
        } else {
            entry_is_directory(path)
        }
    };
    let host = dir.path().to_string_lossy().into_owned();
    let node = read_dir_recursive(dir.path(), true, &host, &host, 0, 2, &classify);

    assert_eq!(names(&node), ["a_dir", "m_file", "z_dir"]);
    let failed = child(&node, "z_dir");
    assert!(!failed.is_dir);
    assert!(
        failed.children.is_empty(),
        "a metadata failure does not open the directory"
    );
    assert!(child(&node, "a_dir").is_dir);
}

#[test]
fn directory_open_failure_keeps_the_directory_without_children() {
    let dir = scratch();
    write_file(&dir.path().join("locked.txt"));
    write_file(&dir.path().join("plain.txt"));

    // A file classified as a directory makes `read_dir` fail the same way an
    // unreadable directory does: the node stays a directory and has no children.
    let classify = |path: &Path| {
        path.file_name() == Some(OsStr::new("locked.txt")) || entry_is_directory(path)
    };
    let host = dir.path().to_string_lossy().into_owned();
    let node = read_dir_recursive(dir.path(), true, &host, &host, 0, 2, &classify);

    assert_eq!(names(&node), ["locked.txt", "plain.txt"]);
    let locked = child(&node, "locked.txt");
    assert!(locked.is_dir);
    assert!(locked.children.is_empty());
}

fn symlink_dir(target: &Path, link: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_dir(target, link)
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link)
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = (target, link);
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "symlinks are not supported on this platform",
        ))
    }
}

fn symlink_file(target: &Path, link: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_file(target, link)
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link)
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = (target, link);
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "symlinks are not supported on this platform",
        ))
    }
}

/// A directory link the host can create. Windows without symlink privilege
/// still has junctions, and Rust reports those as symlinks whose `file_type`
/// does not follow the target. `Path::is_dir` does.
fn link_directory(target: &Path, link: &Path) -> std::io::Result<()> {
    match symlink_dir(target, link) {
        Err(err) if err.raw_os_error() == Some(1314) => directory_junction(target, link),
        other => other,
    }
}

fn directory_junction(target: &Path, link: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use crate::process_util::command_no_window;

        let _guard = crate::env::ENV_LOCK.lock().expect("env lock");
        let link_arg = link.to_string_lossy().into_owned();
        let target_arg = target.to_string_lossy().into_owned();
        let status = command_no_window("cmd.exe")
            .args(["/c", "mklink", "/J", &link_arg, &target_arg])
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(std::io::Error::other(format!(
                "mklink /J failed with {status}"
            )))
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (target, link);
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "directory junctions are Windows-only",
        ))
    }
}

#[test]
fn symlink_to_a_directory_sorts_and_expands_as_a_directory() {
    let dir = scratch();
    let real_dir = dir.path().join("real_dir");
    write_file(&real_dir.join("nested.txt"));
    let real_file = dir.path().join("real_file");
    write_file(&real_file);
    fs::create_dir(dir.path().join("z_dir")).unwrap();
    write_file(&dir.path().join("a_file"));

    let link_dir = dir.path().join("m_link");
    link_directory(&real_dir, &link_dir).expect("directory link");

    let link_type = fs::symlink_metadata(&link_dir)
        .expect("link metadata")
        .file_type();
    assert!(
        link_type.is_symlink(),
        "the directory link must be a symlink or junction, not a real directory"
    );
    assert!(
        link_dir.is_dir(),
        "Path::is_dir follows a directory symlink or junction"
    );
    assert!(
        !link_type.is_dir(),
        "file_type does not follow a directory symlink or junction"
    );

    let mut file_names = vec!["a_file", "real_file"];
    let link_file = dir.path().join("b_link");
    let dangling = dir.path().join("dangling");
    match symlink_file(&real_file, &link_file) {
        Ok(()) => {
            symlink_file(&dir.path().join("missing-target"), &dangling).expect("dangling symlink");
            assert!(!dangling.is_dir(), "a dangling symlink is not a directory");
            file_names = vec!["a_file", "b_link", "dangling", "real_file"];
        }
        Err(err) if err.raw_os_error() == Some(1314) => {
            assert_eq!(err.raw_os_error(), Some(1314));
        }
        Err(err) => panic!("could not create a file symlink: {err}"),
    }

    let node =
        list_directory_blocking(dir.path().to_string_lossy().into_owned(), Some(2)).expect("list");

    // Followed: m_link is a directory, with real_dir and z_dir. file_type
    // would leave m_link in the file group.
    let mut expected = vec!["m_link", "real_dir", "z_dir"];
    expected.extend(file_names);
    assert_eq!(names(&node), expected);
    assert!(child(&node, "m_link").is_dir);
    assert_eq!(names(child(&node, "m_link")), ["nested.txt"]);
    if node.children.iter().any(|entry| entry.name == "b_link") {
        assert!(!child(&node, "b_link").is_dir);
        assert!(!child(&node, "dangling").is_dir);
        assert!(child(&node, "dangling").children.is_empty());
    }
}

#[test]
fn classifications_are_one_per_visible_entry() {
    let dir = scratch();
    for index in (0..8).rev() {
        write_file(&dir.path().join(format!("f{index}")));
    }
    for index in 0..4 {
        write_file(&dir.path().join("d").join(format!("c{index}")));
    }
    write_file(&dir.path().join(".hidden"));
    write_file(&dir.path().join(".hdir").join("nope"));

    let seen = RefCell::new(HashSet::<PathBuf>::new());
    let calls = RefCell::new(0usize);
    let classify = |path: &Path| {
        *calls.borrow_mut() += 1;
        assert!(
            seen.borrow_mut().insert(path.to_path_buf()),
            "classified twice: {}",
            path.display()
        );
        entry_is_directory(path)
    };

    let requested = dir.path().to_string_lossy().into_owned();
    let host = env::to_host_path(&requested);
    let node = read_dir_recursive(Path::new(&host), true, &requested, &host, 0, 3, &classify);

    // 8 files + directory d + d's 4 files. Hidden entries are not classified.
    let expected = 13usize;
    assert_eq!(
        *calls.borrow(),
        expected,
        "directory classification scaled with comparator calls"
    );
    assert!(seen.borrow().iter().all(|path| {
        path.file_name()
            .map(|name| !entry_name_is_hidden(name))
            .unwrap_or(false)
    }));
    assert_eq!(
        names(&node),
        ["d", "f0", "f1", "f2", "f3", "f4", "f5", "f6", "f7"]
    );
    assert_eq!(names(child(&node, "d")), ["c0", "c1", "c2", "c3"]);

    let via_command = list_directory_blocking(requested, Some(3)).expect("list");
    assert_eq!(outline(&node), outline(&via_command));
}

#[test]
fn list_directory_blocking_rejects_a_missing_path_and_a_file() {
    let missing = list_directory_blocking(
        "Z:\\definitely-not-a-real-buildmesh-path-12345".to_string(),
        Some(1),
    );
    assert!(missing.unwrap_err().contains("does not exist"));

    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let file = list_directory_blocking(manifest.to_string_lossy().into_owned(), Some(1));
    assert!(file.unwrap_err().contains("is not a directory"));
}

fn main() {
    // Without these Cargo only re-runs build.rs when this file itself
    // changes, so a `git pull` between builds leaves the embedded SHA
    // stale. packed-refs covers fresh clones; the current branch's ref
    // covers the common dev case; HEAD covers detached HEAD. The paths come
    // from git because `.git` is not under this crate (and is a file in a
    // worktree, whose HEAD lives in the per-worktree git dir). Only existing
    // paths are watched: Cargo re-runs a build script on every build while a
    // watched path is missing, which rebuilt the whole crate on every cargo
    // invocation. Only this branch's ref is watched, not all of `refs/`, so
    // commits in other worktrees do not rebuild this one.
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| std::path::PathBuf::from(s.trim()))
    };
    let mut watched = Vec::new();
    if let Some(git_dir) = git(&["rev-parse", "--git-dir"]) {
        watched.push(git_dir.join("HEAD"));
    }
    if let Some(common_dir) = git(&["rev-parse", "--git-common-dir"]) {
        watched.push(common_dir.join("packed-refs"));
        if let Some(branch_ref) = git(&["symbolic-ref", "-q", "HEAD"]) {
            watched.push(common_dir.join(branch_ref));
        }
    }
    for path in watched.iter().filter(|path| path.exists()) {
        println!("cargo:rerun-if-changed={}", path.display());
    }

    let git_sha = std::process::Command::new("git")
        .args(["describe", "--always", "--dirty"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=GIT_SHA={}", git_sha);

    // rust-embed for the mobile bundle resolves its `folder` path at
    // compile time. Make sure the directory exists even on a fresh
    // checkout where `npm run build` has not yet run, so cargo build
    // never fails on a missing path.
    let mobile_dist = std::path::Path::new("../dist/mobile");
    if !mobile_dist.exists() {
        let _ = std::fs::create_dir_all(mobile_dist);
    }

    // Windows test binary lacks the comctl32 v6 manifest that tauri-build
    // embeds into [[bin]] targets, so cargo test --lib exits with
    // STATUS_ENTRYPOINT_NOT_FOUND when the loader can't find
    // TaskDialogIndirect in System32's v5 comctl32. Delay-loading defers
    // the resolution; test code never calls these APIs so the DLL never loads.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rustc-link-arg=/DELAYLOAD:comctl32.dll");
        println!("cargo:rustc-link-lib=delayimp");

        println!("cargo:rerun-if-changed=../scripts/prepare-conpty.mjs");
        println!("cargo:rerun-if-changed=../scripts/conpty-LICENSE.txt");
        let out_dir = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
        let profile_dir = out_dir.ancestors().nth(3).expect("Cargo profile directory");
        let status = std::process::Command::new("node")
            .arg("../scripts/prepare-conpty.mjs")
            .arg(std::env::var("CARGO_CFG_TARGET_ARCH").unwrap())
            .arg(profile_dir)
            .status()
            .expect("Node.js is required to stage the Windows ConPTY runtime");
        assert!(
            status.success(),
            "failed to prepare the pinned Microsoft ConPTY runtime"
        );
        println!("cargo:rustc-link-search=native={}", profile_dir.display());
    }

    tauri_build::build()
}

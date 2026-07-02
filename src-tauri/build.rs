fn main() {
    // Without these Cargo only re-runs build.rs when this file itself
    // changes, so a `git pull` between builds leaves the embedded SHA
    // stale. .git/packed-refs covers fresh clones; .git/refs/heads/<b>
    // covers the common dev case; .git/HEAD covers detached HEAD.
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/packed-refs");
    println!("cargo:rerun-if-changed=.git/refs");

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
    }

    tauri_build::build()
}

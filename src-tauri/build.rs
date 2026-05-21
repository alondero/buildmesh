fn main() {
    let git_sha = std::process::Command::new("git")
        .args(["describe", "--always", "--dirty"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=GIT_SHA={}", git_sha);

    // Without this, `cargo test --lib` on Windows crashes at startup with
    // STATUS_ENTRYPOINT_NOT_FOUND (0xC0000139) because the test binary lacks
    // the comctl32 v6 manifest that the production exe gets from resource.lib.
    // Delay-loading comctl32 keeps the OS loader from resolving the missing
    // import until first use — and the lib code never actually calls it.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rustc-link-arg=/DELAYLOAD:comctl32.dll");
        println!("cargo:rustc-link-lib=delayimp");
    }

    tauri_build::build()
}

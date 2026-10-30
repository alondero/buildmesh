//! Shared support for subprocess-based hook delivery tests.

use std::process::Command;
use std::time::Duration;

// Cold PowerShell startup has exceeded 20 seconds under contention (#2049).
// Keep ample headroom for more heavily loaded concurrent verification runs.
pub(super) fn budget(request_timeout: Duration) -> Duration {
    request_timeout + Duration::from_secs(120)
}

pub(super) fn isolate_cache(command: &mut Command) -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    command.env(
        "PSModuleAnalysisCachePath",
        directory.path().join("module-cache"),
    );
    directory
}

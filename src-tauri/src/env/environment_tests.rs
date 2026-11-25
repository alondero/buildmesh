use super::*;
use crate::models::EnvType;

fn devin_db_path_for_host_with<
    F: Fn(&str) -> Option<std::ffi::OsString>,
    H: FnOnce() -> Option<PathBuf>,
    D: FnOnce() -> Option<String>,
>(
    env_type: EnvType,
    spawn_path: &str,
    get: F,
    guest_home: H,
    distro: D,
) -> Option<PathBuf> {
    let db_path = devin_db_path_with_seams(env_type, spawn_path, get, guest_home)?;
    Some(PathBuf::from(crate::env::to_host_path_with_distro(
        &db_path.to_string_lossy(),
        distro,
    )))
}

#[test]
fn devin_db_path_for_host_translates_wsl_guest_path() {
    let guest_home = || Some(std::path::PathBuf::from("/home/alond"));
    let distro = || Some("FakeDistro".to_string());
    let injected_env = |key: &str| match key {
        "USERPROFILE" => Some(r"C:\Users\dev".into()),
        "HOME" => Some("/home/dev".into()),
        "USERNAME" => Some("dev".into()),
        _ => None,
    };

    let guest = devin_db_path_for_host_with(EnvType::Wsl, "", injected_env, guest_home, distro)
        .expect("WSL guest home must resolve");
    let host_str = if cfg!(target_os = "windows") {
        r"\\wsl$\FakeDistro\home\alond\.local\share\devin\cli\sessions.db"
    } else {
        "/home/alond/.local/share/devin/cli/sessions.db"
    };
    assert_eq!(
        guest.to_string_lossy(),
        host_str,
        "WSL guest path must translate to the injected distro's UNC path"
    );

    let raw_windows = devin_db_path_with_seams(EnvType::Windows, "", injected_env, || {
        None::<std::path::PathBuf>
    })
    .expect("windows home must resolve");
    let host_windows = devin_db_path_for_host_with(
        EnvType::Windows,
        "",
        injected_env,
        || None::<std::path::PathBuf>,
        || None,
    )
    .expect("windows host-path must resolve");
    assert_eq!(raw_windows, host_windows, "Windows paths must be no-ops");
}

#[test]
fn devin_db_path_honours_the_binary_overrides() {
    let injected = |key: &str| match key {
        "DEVIN_SESSIONS_DIR" => Some(r"D:\devin-state".into()),
        _ => None,
    };
    let path =
        devin_db_path_with_seams(
            EnvType::Windows,
            "",
            injected,
            || None::<std::path::PathBuf>,
        )
        .expect("override must resolve");
    assert_eq!(
        path,
        std::path::PathBuf::from(r"D:\devin-state").join("sessions.db")
    );

    let injected = |key: &str| match key {
        "CHISEL_SESSION_DB" => Some(r"D:\devin-state\daemon.db".into()),
        _ => None,
    };
    let path =
        devin_db_path_with_seams(
            EnvType::Windows,
            "",
            injected,
            || None::<std::path::PathBuf>,
        )
        .expect("file override must resolve");
    assert_eq!(path, std::path::PathBuf::from(r"D:\devin-state\daemon.db"));
}

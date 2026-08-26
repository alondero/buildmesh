// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if buildmesh_lib::run_crash_watchdog_if_requested() {
        return;
    }
    buildmesh_lib::run()
}

//! Fast, in-memory credit return for the two independent terminal transports.

#[tauri::command]
pub fn acknowledge_pty_output(session_id: i64, channel_id: u32, sequence: u64, build_run: bool) {
    let sinks = if build_run {
        &crate::pty::sink::BUILD_RUN
    } else {
        &crate::pty::sink::AGENT
    };
    sinks.acknowledge(session_id, channel_id, sequence);
}

/// Deterministic raw frames for the native transport benchmark. No PTY, DB or
/// provider work participates in this measurement. Unavailable in release builds.
#[cfg(any(debug_assertions, feature = "dev-tools"))]
#[tauri::command]
pub async fn debug_pump_pty_output(
    session_id: i64,
    build_run: bool,
    frames: u32,
    frame_size: Option<usize>,
) -> Result<(), String> {
    if frame_size.is_some_and(|size| size == 0 || size > 32768) {
        return Err("frameSize must be between 1 and 32768".into());
    }
    super::run_blocking("debug_pump_pty_output", move || {
        let sinks = if build_run {
            &crate::pty::sink::BUILD_RUN
        } else {
            &crate::pty::sink::AGENT
        };
        let sink = sinks.ensure(session_id);
        for sequence in 0..frames {
            let size = frame_size.unwrap_or([512, 2048, 32768, 3, 1][sequence as usize % 5]);
            let data = (0..size)
                .map(|offset| (sequence as usize + offset) as u8)
                .collect();
            sink.send_owned(data);
        }
        Ok(())
    })
    .await
}

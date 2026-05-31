//! Backward-compat facade. The implementation lives in `crate::http`.
//!
//! New code should import from `crate::http::*` directly; existing call sites
//! continue to use `crate::http_server::*` without churn.

pub use crate::http::current_http_port;
pub use crate::http::fulfill_snapshot;
pub use crate::http::start_http_server;
pub use crate::http::ws::{
    clear_scrollback, ensure_pty_channel, send_pty_output,
};
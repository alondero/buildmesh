//! Muse Session Protocol integration (issues #1679 / #1681 / #1680).
//!
//! The native harness adapter stays in [`super::adapters::muse`]. This
//! module owns MSP-shaped concerns that are not PTY spawn recipes.
//! Telemetry (#1680) is the first landed slice: it ingests
//! `session/tokenUsage` and `session/contextUsage` notifications and
//! exposes a public node payload. Lifecycle (#1681) should call
//! [`telemetry::ingest_line`] for every `MspTransport::events()`
//! notification once that transport exists.

pub mod telemetry;

//! Headless CLI administration, maintenance, status monitoring, printing,
//! and scanning library for the Canon PIXMA G2010 multifunction printer.

pub mod args;
pub mod commands;
pub mod status;

use pixma_daemon::DaemonClient;
use std::path::Path;

/// Establishes communication with the printer via `--mock` / `PIXMA_MOCK=1` or fail-closed live hardware (`pixmad` / USB).
pub async fn connect_client(socket: Option<&Path>, mock: bool) -> anyhow::Result<DaemonClient> {
    if mock || std::env::var("PIXMA_MOCK").as_deref() == Ok("1") {
        Ok(DaemonClient::connect_mock(None)?)
    } else {
        Ok(DaemonClient::connect_live(socket, None).await?)
    }
}

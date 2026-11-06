//! Headless CLI administration, maintenance, status monitoring, printing,
//! and scanning library for the Canon PIXMA G2010 multifunction printer.

pub mod args;
pub mod commands;
pub mod status;

use pixma_core::maintenance::{MaintenanceRecord, MaintenanceSupervisor};
use pixma_daemon::DaemonClient;
use pixma_transport::controller::{AnyTransport, G2010Controller};
use pixma_transport::mock_device::MockG2010Transport;
use std::path::Path;

/// Establishes communication with the printer via `pixmad` socket or direct controller.
pub async fn connect_client(socket: Option<&Path>, mock: bool) -> anyhow::Result<DaemonClient> {
    if mock {
        let mock_transport = MockG2010Transport::new();
        let supervisor = MaintenanceSupervisor::in_memory(MaintenanceRecord::default());
        let controller =
            G2010Controller::new(AnyTransport::Mock(Box::new(mock_transport)), supervisor);
        Ok(DaemonClient::direct(controller))
    } else {
        let client = DaemonClient::connect_or_fallback(socket, None).await?;
        Ok(client)
    }
}

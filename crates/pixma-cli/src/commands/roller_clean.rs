//! Mechanical roller and platen bottom plate cleaning routines.

use pixma_core::maintenance::{MaintenanceAction, RollerCleanTarget};
use pixma_core::models::DeviceStatus;
use pixma_daemon::DaemonClient;

use crate::args::RollerTargetArg;

/// Executes feed roller or bottom plate cleaning.
pub async fn execute_roller_clean(
    client: &mut DaemonClient,
    target_arg: RollerTargetArg,
    json: bool,
) -> anyhow::Result<DeviceStatus> {
    let target: RollerCleanTarget = target_arg.into();
    let action = MaintenanceAction::MechanicalClean(target);

    if !json {
        match target {
            RollerCleanTarget::FeedRoller => {
                println!("Initiating paper feed roller cleaning...");
                println!("Remove all paper from the rear tray and press Black/Color if prompted.");
            }
            RollerCleanTarget::BottomPlate => {
                println!("Initiating bottom plate platen cleaning...");
                println!(
                    "Fold an A4 sheet horizontally in half, insert into rear tray, and start."
                );
            }
        }
    }

    let status = client.execute_maintenance(action, None, false).await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&status)?);
    } else {
        println!("Cleaning routine command dispatched successfully.");
    }

    Ok(status)
}

//! Print head alignment calibration routines.

use pixma_core::maintenance::{MaintenanceAction, TestPrintType};
use pixma_core::models::DeviceStatus;
use pixma_daemon::DaemonClient;

use crate::args::{parse_alignment_values_csv, AlignActionArg};

/// Executes the requested alignment action.
pub async fn execute_align(
    client: &mut DaemonClient,
    action: AlignActionArg,
    values_csv: Option<&str>,
    json: bool,
) -> anyhow::Result<DeviceStatus> {
    let action_to_dispatch = match action {
        AlignActionArg::PrintManual => {
            if !json {
                println!("Printing manual printhead alignment pattern (2 sheets)...");
            }
            MaintenanceAction::TestPrint(TestPrintType::ManualAlignmentPattern)
        }
        AlignActionArg::HalfAuto => {
            if !json {
                println!("Printing semi-automatic printhead alignment pattern (1 sheet)...");
            }
            MaintenanceAction::TestPrint(TestPrintType::HalfAutoAlignment)
        }
        AlignActionArg::Check => {
            if !json {
                println!("Printing alignment verification check pattern (1 sheet)...");
            }
            MaintenanceAction::TestPrint(TestPrintType::AlignmentCheck)
        }
        AlignActionArg::Apply => {
            let csv = values_csv.ok_or_else(|| {
                anyhow::anyhow!("`--values <CSV>` is required when `--action apply` is selected.")
            })?;
            let column_values = parse_alignment_values_csv(csv)
                .map_err(|e| anyhow::anyhow!("Invalid alignment values: {e}"))?;

            if !json {
                println!(
                    "Applying manual alignment column corrections: {:?}",
                    column_values
                );
            }
            MaintenanceAction::ApplyManualAlignment { column_values }
        }
    };

    let status = client
        .execute_maintenance(action_to_dispatch, None, false)
        .await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&status)?);
    } else {
        println!("Alignment command executed successfully.");
    }

    Ok(status)
}

//! Print head cleaning execution and safety interlock verification.

use pixma_core::errors::SafetyInterlockError;
use pixma_core::maintenance::{
    CleaningTarget, CleaningType, MaintenanceAction, INK_FLUSH_CONFIRMATION_TOKEN,
};
use pixma_core::models::DeviceStatus;
use pixma_daemon::DaemonClient;

use crate::args::{CleanKindArg, CleanTargetArg};

/// Displays the mandatory safety warning for ink flush.
pub fn print_ink_flush_warning() {
    eprintln!(
        "\
================================================================================
CRITICAL SAFETY WARNING: SYSTEM INK FLUSH (choke)
================================================================================
An Ink Flush performs a high-pressure purge of the CISS ink supply tubes:
  - Consumes approximately 45 ml of ink across all tanks (~1/3 of bottle capacity).
  - Advances the internal Waste Ink Absorber counter (D-Counter) by ~20.0%.
  - Requires a mandatory 24-hour hardware rest cooldown between runs.
  - HARD-BLOCKED if the Waste Ink Absorber counter is >= 80% to protect against
    fatal error 5B00/P07 and hazardous internal ink pad overflow.

To proceed, you MUST provide explicit confirmation:
  pixma-cli clean --kind flush --confirm {}
================================================================================",
        INK_FLUSH_CONFIRMATION_TOKEN
    );
}

/// Executes the requested cleaning operation.
pub async fn execute_clean(
    client: &mut DaemonClient,
    kind_arg: CleanKindArg,
    target_arg: CleanTargetArg,
    confirm: Option<&str>,
    override_cooldown: bool,
    json: bool,
) -> anyhow::Result<DeviceStatus> {
    let kind: CleaningType = kind_arg.into();
    let target: CleaningTarget = target_arg.into();

    if kind == CleaningType::InkFlush && confirm != Some(INK_FLUSH_CONFIRMATION_TOKEN) {
        print_ink_flush_warning();
        return Err(SafetyInterlockError::ConfirmationRequired {
            expected_token: INK_FLUSH_CONFIRMATION_TOKEN,
        }
        .into());
    }

    let action = MaintenanceAction::Clean { kind, target };

    let updated_status = client
        .execute_maintenance(action, confirm, override_cooldown)
        .await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&updated_status)?);
    } else {
        println!(
            "Successfully initiated {:?} cleaning for {:?} heads.",
            kind, target
        );
        println!(
            "Waste absorber saturation is now at {:.1}%.",
            updated_status.waste_absorber_percent
        );
    }

    Ok(updated_status)
}

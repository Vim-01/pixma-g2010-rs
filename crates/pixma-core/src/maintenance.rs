//! Maintenance operations, ink consumption estimation, and mandatory
//! cooldown supervisor protecting against waste ink overflow (`5B00` / `P07`).

use crate::errors::SafetyInterlockError;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Mandatory 24-hour cooldown between Ink System Flushes (`choke`).
pub const INK_FLUSH_COOLDOWN: Duration = Duration::from_secs(24 * 60 * 60);
/// Mandatory 10-minute cooldown between Deep Cleanings (`deep`).
pub const DEEP_CLEANING_COOLDOWN: Duration = Duration::from_secs(10 * 60);
/// Mandatory 60-second cooldown between Standard Cleanings (`regular`).
pub const STANDARD_CLEANING_COOLDOWN: Duration = Duration::from_secs(60);

/// Maximum safe D-counter percentage before blocking Ink Flush.
pub const MAX_SAFE_D_COUNTER_PERCENT: f32 = 80.0;
/// Explicit confirmation token required for Ink Flush.
pub const INK_FLUSH_CONFIRMATION_TOKEN: &str = "YES-DRAIN-INK";

/// IVEC `<ivec:Cleaning>` type (`regular`, `deep`, `choke`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CleaningType {
    /// Standard printhead cleaning (~0.3 ml ink, +0.2% D-counter).
    Regular,
    /// Deep printhead cleaning (~2.0 ml ink, +1.2% D-counter).
    Deep,
    /// System Cleaning / Ink Flush (~45 ml ink, +20.0% D-counter). Purges CISS tubes.
    InkFlush,
}

impl CleaningType {
    pub const fn ivec_xml_value(self) -> &'static str {
        match self {
            Self::Regular => "regular",
            Self::Deep => "deep",
            Self::InkFlush => "choke",
        }
    }

    pub const fn estimated_ink_ml(self) -> f32 {
        match self {
            Self::Regular => 0.3,
            Self::Deep => 2.0,
            Self::InkFlush => 45.0,
        }
    }

    pub const fn estimated_d_counter_delta(self) -> f32 {
        match self {
            Self::Regular => 0.2,
            Self::Deep => 1.2,
            Self::InkFlush => 20.0,
        }
    }
}

/// Ink group target for cleaning (`all`, `group1` Black, `group2` Color).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CleaningTarget {
    /// Both Black (PGBK) and Color (CMY) heads.
    All,
    /// Black (GI-790 PGBK / BH-4) only (`group1`).
    BlackOnly,
    /// Color (GI-790 C/M/Y / CH-41) only (`group2`).
    ColorOnly,
}

impl CleaningTarget {
    pub const fn ivec_xml_value(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::BlackOnly => "group1",
            Self::ColorOnly => "group2",
        }
    }
}

/// Mechanical cleaning target (`roller` or `platen`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RollerCleanTarget {
    /// Paper feed rollers (`roller`).
    FeedRoller,
    /// Bottom plate ribs (`platen`).
    BottomPlate,
}

impl RollerCleanTarget {
    pub const fn ivec_xml_value(self) -> &'static str {
        match self {
            Self::FeedRoller => "roller",
            Self::BottomPlate => "platen",
        }
    }
}

/// Diagnostic test print and printhead alignment routines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TestPrintType {
    /// Nozzle check diagnostic grid (`nozzle_check`, 1 sheet).
    NozzleCheck,
    /// Manual printhead alignment pattern (`manual_registration`, 2 sheets).
    ManualAlignmentPattern,
    /// Semi-automatic printhead alignment (`half_auto_registration`, 1 sheet).
    HalfAutoAlignment,
    /// Alignment verification pattern (`regi_check`, 1 sheet).
    AlignmentCheck,
}

impl TestPrintType {
    pub const fn ivec_xml_value(self) -> &'static str {
        match self {
            Self::NozzleCheck => "nozzle_check",
            Self::ManualAlignmentPattern => "manual_registration",
            Self::HalfAutoAlignment => "half_auto_registration",
            Self::AlignmentCheck => "regi_check",
        }
    }
}

/// High-level maintenance operation requested by CLI, GUI, or CUPS command filter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MaintenanceAction {
    Clean {
        kind: CleaningType,
        target: CleaningTarget,
    },
    MechanicalClean(RollerCleanTarget),
    TestPrint(TestPrintType),
    ApplyManualAlignment {
        /// Alignment adjustment values for columns A..N (-5..=+5).
        column_values: Vec<i8>,
    },
}

/// Persistent state record tracking maintenance history and waste absorber saturation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaintenanceRecord {
    pub last_standard_clean_epoch: u64,
    pub last_deep_clean_epoch: u64,
    pub last_ink_flush_epoch: u64,
    pub estimated_d_counter_percent: f32,
    pub total_ink_flushes: u32,
}

impl Default for MaintenanceRecord {
    fn default() -> Self {
        Self {
            last_standard_clean_epoch: 0,
            last_deep_clean_epoch: 0,
            last_ink_flush_epoch: 0,
            estimated_d_counter_percent: 0.0,
            total_ink_flushes: 0,
        }
    }
}

/// Enforces mandatory cooldowns and waste ink absorber safety interlocks.
#[derive(Debug, Clone)]
pub struct MaintenanceSupervisor {
    state_file: Option<PathBuf>,
    record: MaintenanceRecord,
}

impl MaintenanceSupervisor {
    pub fn in_memory(record: MaintenanceRecord) -> Self {
        Self {
            state_file: None,
            record,
        }
    }

    pub fn load_or_init<P: AsRef<Path>>(path: P) -> Result<Self, std::io::Error> {
        let path = path.as_ref().to_path_buf();
        let record = if path.exists() {
            let content = std::fs::read_to_string(&path)?;
            serde_json::from_str(&content).unwrap_or_default()
        } else {
            MaintenanceRecord::default()
        };
        Ok(Self {
            state_file: Some(path),
            record,
        })
    }

    pub const fn record(&self) -> &MaintenanceRecord {
        &self.record
    }

    pub fn update_d_counter_from_hardware(&mut self, reported_percent: f32) {
        if reported_percent > self.record.estimated_d_counter_percent {
            self.record.estimated_d_counter_percent = reported_percent;
        }
    }

    /// Validates whether a cleaning operation is safe to execute at `now_epoch`.
    pub fn authorize_cleaning(
        &self,
        kind: CleaningType,
        confirmation_token: Option<&str>,
        override_cooldown: bool,
        now_epoch: u64,
    ) -> Result<(), SafetyInterlockError> {
        match kind {
            CleaningType::Regular => {
                if self.record.last_standard_clean_epoch > 0 && !override_cooldown {
                    let elapsed = now_epoch.saturating_sub(self.record.last_standard_clean_epoch);
                    if elapsed < STANDARD_CLEANING_COOLDOWN.as_secs() {
                        return Err(SafetyInterlockError::CooldownActive {
                            operation: "Standard Cleaning",
                            remaining_seconds: STANDARD_CLEANING_COOLDOWN.as_secs() - elapsed,
                        });
                    }
                }
            }
            CleaningType::Deep => {
                if self.record.last_deep_clean_epoch > 0 && !override_cooldown {
                    let elapsed = now_epoch.saturating_sub(self.record.last_deep_clean_epoch);
                    if elapsed < DEEP_CLEANING_COOLDOWN.as_secs() {
                        return Err(SafetyInterlockError::CooldownActive {
                            operation: "Deep Cleaning",
                            remaining_seconds: DEEP_CLEANING_COOLDOWN.as_secs() - elapsed,
                        });
                    }
                }
            }
            CleaningType::InkFlush => {
                if self.record.estimated_d_counter_percent >= MAX_SAFE_D_COUNTER_PERCENT {
                    return Err(SafetyInterlockError::AbsorberNearCapacity {
                        current_percent: self.record.estimated_d_counter_percent,
                        max_safe_percent: MAX_SAFE_D_COUNTER_PERCENT,
                    });
                }
                if confirmation_token != Some(INK_FLUSH_CONFIRMATION_TOKEN) {
                    return Err(SafetyInterlockError::ConfirmationRequired {
                        expected_token: INK_FLUSH_CONFIRMATION_TOKEN,
                    });
                }
                if self.record.last_ink_flush_epoch > 0 && !override_cooldown {
                    let elapsed = now_epoch.saturating_sub(self.record.last_ink_flush_epoch);
                    if elapsed < INK_FLUSH_COOLDOWN.as_secs() {
                        return Err(SafetyInterlockError::CooldownActive {
                            operation: "Ink System Flush (choke)",
                            remaining_seconds: INK_FLUSH_COOLDOWN.as_secs() - elapsed,
                        });
                    }
                }
            }
        }
        Ok(())
    }

    /// Records the execution of a cleaning operation and persists the updated state.
    pub fn record_cleaning_executed(
        &mut self,
        kind: CleaningType,
        now_epoch: u64,
    ) -> Result<(), std::io::Error> {
        match kind {
            CleaningType::Regular => {
                self.record.last_standard_clean_epoch = now_epoch;
            }
            CleaningType::Deep => {
                self.record.last_deep_clean_epoch = now_epoch;
            }
            CleaningType::InkFlush => {
                self.record.last_ink_flush_epoch = now_epoch;
                self.record.total_ink_flushes += 1;
            }
        }
        self.record.estimated_d_counter_percent =
            (self.record.estimated_d_counter_percent + kind.estimated_d_counter_delta()).min(100.0);
        self.persist()
    }

    pub fn current_unix_epoch() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }

    fn persist(&self) -> Result<(), std::io::Error> {
        if let Some(ref path) = self.state_file {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let serialized =
                serde_json::to_string_pretty(&self.record).map_err(std::io::Error::other)?;
            std::fs::write(path, serialized)?;
        }
        Ok(())
    }
}

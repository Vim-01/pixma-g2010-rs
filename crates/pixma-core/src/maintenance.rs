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

/// Ink group target for cleaning (`all`, `group1`, `group2`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CleaningTarget {
    /// Both Black (PGBK) and Color (CMY) heads (`all`).
    All,
    /// Black (GI-790 PGBK / BH-4) only (`group1`).
    BlackOnly,
    /// Color (GI-790 C/M/Y / CH-41) only (`group2`).
    ColorOnly,
}

impl CleaningTarget {
    /// Canon IVEC `<ivec:inkgroup>` wire token (confirmed via official PPD `#CNIJ-IVEC-CAPABILITY-MAINTENANCE`).
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

/// Maintenance cleaning capabilities parsed from `GetCapability(servicetype="maintenance")`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MaintenanceCapabilities {
    /// Supported `(cleaning_type, allowed_ink_groups)` pairs reported by printer firmware
    /// (for example `("regular", ["all", "group1", "group2"])`).
    pub cleaning_groups: Vec<(String, Vec<String>)>,
    /// Raw `GetCapabilityResponse` XML returned by the firmware for diagnostics.
    pub raw_xml: String,
}

impl MaintenanceCapabilities {
    /// Returns `Some(true)` if the firmware explicitly lists `target_ivec` under `kind_ivec`,
    /// `Some(false)` if `kind_ivec` is listed with a non-empty allowed group list excluding
    /// `target_ivec`, and `None` when capability data is unavailable or empty.
    pub fn supports(&self, kind_ivec: &str, target_ivec: &str) -> Option<bool> {
        let kind_norm = kind_ivec.trim().to_ascii_lowercase();
        let target_norm = target_ivec.trim().to_ascii_lowercase();
        let (_, groups) = self
            .cleaning_groups
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(&kind_norm))?;
        if groups.is_empty() {
            return None;
        }
        Some(groups.iter().any(|g| g.eq_ignore_ascii_case(&target_norm)))
    }

    /// Formats the allowed `(type, [groups])` list into a concise summary string.
    pub fn format_summary(&self) -> Option<String> {
        if self.cleaning_groups.is_empty() {
            return None;
        }
        let summary = self
            .cleaning_groups
            .iter()
            .map(|(kind, ink_groups)| format!("{kind}=[{}]", ink_groups.join(", ")))
            .collect::<Vec<_>>()
            .join(", ");
        Some(summary)
    }
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

    /// Probes whether `dir` exists (creating it if absent) and supports creating/deleting files.
    pub fn probe_dir_writable(dir: &Path) -> Result<(), std::io::Error> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static PROBE_SEQ: AtomicU64 = AtomicU64::new(0);

        if dir.as_os_str().is_empty() {
            return Ok(());
        }
        if let Ok(meta) = std::fs::symlink_metadata(dir) {
            if meta.file_type().is_symlink() || !meta.file_type().is_dir() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("Unsafe directory path (symlink or non-directory): {}", dir.display()),
                ));
            }
        } else {
            let mut builder = std::fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o755);
            }
            builder.create(dir)?;
        }

        let probe_path = dir.join(format!(
            ".pixma_write_probe.{}.{}",
            std::process::id(),
            PROBE_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        opts.open(&probe_path)?;
        let _ = std::fs::remove_file(&probe_path);
        Ok(())
    }

    pub fn load_or_init<P: AsRef<Path>>(path: P) -> Result<Self, std::io::Error> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            Self::probe_dir_writable(parent)?;
        }
        if let Ok(meta) = std::fs::symlink_metadata(&path) {
            if meta.file_type().is_symlink() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("Refusing symlink state file: {}", path.display()),
                ));
            }
        }
        let record = match std::fs::read_to_string(&path) {
            Ok(content) => serde_json::from_str(&content).map_err(|e| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("Corrupted maintenance state JSON {}: {e}", path.display()),
                )
            })?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => MaintenanceRecord::default(),
            Err(e) => return Err(e),
        };
        Ok(Self {
            state_file: Some(path),
            record,
        })
    }

    /// Loads persisted maintenance state read-only into an in-memory supervisor
    /// without ever creating directories or modifying files on disk.
    pub fn load_readonly<P: AsRef<Path>>(path: P) -> Result<Self, std::io::Error> {
        let content = std::fs::read_to_string(path)?;
        let record: MaintenanceRecord = serde_json::from_str(&content)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        Ok(Self::in_memory(record))
    }

    pub const fn record(&self) -> &MaintenanceRecord {
        &self.record
    }

    pub fn update_d_counter_from_hardware(&mut self, reported_percent: f32) {
        if reported_percent.is_finite()
            && reported_percent >= 0.0
            && reported_percent > self.record.estimated_d_counter_percent
        {
            self.record.estimated_d_counter_percent = reported_percent.min(100.0);
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
                let d_counter = self.record.estimated_d_counter_percent;
                if !d_counter.is_finite() || d_counter >= MAX_SAFE_D_COUNTER_PERCENT {
                    return Err(SafetyInterlockError::AbsorberNearCapacity {
                        current_percent: d_counter,
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
        use std::io::Write;
        use std::sync::atomic::{AtomicU64, Ordering};

        static NEXT_TMP_SEQ: AtomicU64 = AtomicU64::new(0);

        if let Some(ref path) = self.state_file {
            if let Some(parent) = path.parent() {
                if !parent.as_os_str().is_empty() && !parent.exists() {
                    let mut builder = std::fs::DirBuilder::new();
                    builder.recursive(true);
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::DirBuilderExt;
                        builder.mode(0o755);
                    }
                    builder.create(parent)?;
                }
            }
            let serialized =
                serde_json::to_string_pretty(&self.record).map_err(std::io::Error::other)?;

            let file_name = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("state.json");
            let pid = std::process::id();
            let seq = NEXT_TMP_SEQ.fetch_add(1, Ordering::Relaxed);
            let tmp_name = format!("{file_name}.tmp.{pid}.{seq}");
            let tmp_path = match path.parent() {
                Some(parent) if !parent.as_os_str().is_empty() => parent.join(tmp_name),
                _ => PathBuf::from(tmp_name),
            };

            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o644);
            }

            let write_res = (|| -> Result<(), std::io::Error> {
                let mut file = opts.open(&tmp_path)?;
                file.write_all(serialized.as_bytes())?;
                file.sync_all()?;
                Ok(())
            })();
            if let Err(e) = write_res {
                let _ = std::fs::remove_file(&tmp_path);
                return Err(e);
            }

            if let Err(e) = std::fs::rename(&tmp_path, path) {
                let _ = std::fs::remove_file(&tmp_path);
                return Err(e);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleaning_interlocks_enforce_cooldowns_override_and_tokens() {
        let mut sup = MaintenanceSupervisor::in_memory(MaintenanceRecord::default());
        assert!(sup
            .authorize_cleaning(CleaningType::Regular, None, false, 1_000)
            .is_ok());
        sup.record_cleaning_executed(CleaningType::Regular, 1_000)
            .unwrap();

        assert!(matches!(
            sup.authorize_cleaning(CleaningType::Regular, None, false, 1_030),
            Err(SafetyInterlockError::CooldownActive {
                remaining_seconds: 30,
                ..
            })
        ));
        assert!(sup
            .authorize_cleaning(CleaningType::Regular, None, true, 1_030)
            .is_ok());
        assert!(sup
            .authorize_cleaning(CleaningType::Regular, None, false, 1_060)
            .is_ok());

        // Backwards wall clock uses saturating subtraction and keeps cooldown enforced safely
        assert!(matches!(
            sup.authorize_cleaning(CleaningType::Regular, None, false, 900),
            Err(SafetyInterlockError::CooldownActive {
                remaining_seconds: 60,
                ..
            })
        ));
    }

    #[test]
    fn ink_flush_blocks_high_and_non_finite_d_counter_even_with_override() {
        for bad_val in [80.0_f32, 95.0, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let rec = MaintenanceRecord {
                estimated_d_counter_percent: bad_val,
                ..Default::default()
            };
            let sup = MaintenanceSupervisor::in_memory(rec);
            assert!(matches!(
                sup.authorize_cleaning(
                    CleaningType::InkFlush,
                    Some(INK_FLUSH_CONFIRMATION_TOKEN),
                    true,
                    100_000,
                ),
                Err(SafetyInterlockError::AbsorberNearCapacity { .. })
            ));
        }
    }

    #[test]
    fn ink_flush_requires_confirmation_token_and_24h_cooldown() {
        let mut sup = MaintenanceSupervisor::in_memory(MaintenanceRecord::default());
        assert!(matches!(
            sup.authorize_cleaning(CleaningType::InkFlush, Some("NOPE"), false, 10_000),
            Err(SafetyInterlockError::ConfirmationRequired { .. })
        ));
        assert!(sup
            .authorize_cleaning(
                CleaningType::InkFlush,
                Some(INK_FLUSH_CONFIRMATION_TOKEN),
                false,
                10_000
            )
            .is_ok());
        sup.record_cleaning_executed(CleaningType::InkFlush, 10_000)
            .unwrap();
        assert_eq!(sup.record().total_ink_flushes, 1);

        assert!(matches!(
            sup.authorize_cleaning(
                CleaningType::InkFlush,
                Some(INK_FLUSH_CONFIRMATION_TOKEN),
                false,
                10_500
            ),
            Err(SafetyInterlockError::CooldownActive { .. })
        ));
    }

    #[test]
    fn atomic_persist_and_reload_roundtrip() {
        let dir = std::env::temp_dir().join(format!(
            "pixma-maint-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let state_path = dir.join("sub").join("state.json");

        let mut sup = MaintenanceSupervisor::load_or_init(&state_path).unwrap();
        sup.update_d_counter_from_hardware(f32::NAN);
        sup.update_d_counter_from_hardware(12.5);
        sup.record_cleaning_executed(CleaningType::Deep, 5_000)
            .unwrap();

        let reloaded = MaintenanceSupervisor::load_or_init(&state_path).unwrap();
        assert_eq!(reloaded.record().last_deep_clean_epoch, 5_000);
        assert!((reloaded.record().estimated_d_counter_percent - 13.7).abs() < 0.01);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let file_mode = std::fs::metadata(&state_path).unwrap().permissions().mode() & 0o777;
            assert_eq!(
                file_mode, 0o644,
                "State file must remain readable by CUPS filters (lp) at 0644"
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn load_or_init_rejects_unwritable_parent_dir() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let uid = std::fs::metadata("/proc/self").map(|m| m.uid()).unwrap_or(0);
        if uid == 0 {
            return;
        }

        let dir = std::env::temp_dir().join(format!(
            "pixma-maint-ro-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();

        let err = MaintenanceSupervisor::load_or_init(dir.join("state.json"))
            .expect_err("Read-only parent dir must fail early in load_or_init");
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);

        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_or_init_rejects_corrupted_json_without_overwriting() {
        let dir = std::env::temp_dir().join(format!(
            "pixma-maint-corrupt-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let state_path = dir.join("state.json");
        let corrupt = b"{not valid maintenance json";
        std::fs::write(&state_path, corrupt).unwrap();

        let err = MaintenanceSupervisor::load_or_init(&state_path)
            .expect_err("Corrupted state.json must return InvalidData instead of zeroing state");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(std::fs::read(&state_path).unwrap(), corrupt);

        let _ = std::fs::remove_dir_all(&dir);
    }
}

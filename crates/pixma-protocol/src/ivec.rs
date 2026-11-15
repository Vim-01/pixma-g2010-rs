//! Canon IVEC XML protocol encoders and status response parsers.
//!
//! IVEC (Image and Video Environment Control) is Canon's XML envelope protocol
//! layered over USB bulk endpoints for job control, page setup, device telemetry,
//! and maintenance commands.
//!
//! Namespaces:
//! - `xmlns:ivec="http://www.canon.com/ns/cmd/2008/07/common/"`
//! - `xmlns:vcn="http://www.canon.com/ns/cmd/2008/07/canon/"`

use crate::errors::ProtocolError;
use pixma_core::errors::SupportCodeInfo;
use pixma_core::maintenance::{CleaningTarget, CleaningType, RollerCleanTarget, TestPrintType};
use pixma_core::models::{CartridgeState, DeviceStatus, OperationalState, PrinterModel};
use pixma_core::print_params::{DuplexMode, PrintJobConfig};
use quick_xml::events::Event;
use quick_xml::Reader;

pub const IVEC_COMMON_NS: &str = "http://www.canon.com/ns/cmd/2008/07/common/";
pub const VCN_CANON_NS: &str = "http://www.canon.com/ns/cmd/2008/07/canon/";

// ---------------------------------------------------------------------------
// Print Job Lifecycle XML Builders
// ---------------------------------------------------------------------------

/// Generates the `SetJobConfiguration` IVEC XML command for a print job.
pub fn build_set_job_configuration(job_id: u32, datetime: Option<&str>) -> String {
    let dt = datetime.unwrap_or("20261108000000");
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?><cmd xmlns:ivec="{IVEC_COMMON_NS}"><ivec:contents><ivec:operation>SetJobConfiguration</ivec:operation><ivec:param_set servicetype="print"><ivec:jobID>{job_id:08}</ivec:jobID><ivec:mismatch_mode>none</ivec:mismatch_mode><ivec:datetime>{dt}</ivec:datetime></ivec:param_set></ivec:contents></cmd>"#
    )
}

/// Generates the `StartJob` IVEC XML command for a print, scan, or maintenance session.
pub fn build_start_job(service_type: &str, job_id: u32) -> String {
    if service_type == "print" {
        format!(
            r#"<?xml version="1.0" encoding="utf-8" ?><cmd xmlns:ivec="{IVEC_COMMON_NS}" xmlns:vcn="{VCN_CANON_NS}"><ivec:contents><ivec:operation>StartJob</ivec:operation><ivec:param_set servicetype="print"><ivec:jobID>{job_id:08}</ivec:jobID><ivec:bidi>0</ivec:bidi><vcn:forcepmdetection>OFF</vcn:forcepmdetection><ivec:jobname/><ivec:username/><ivec:computername/><ivec:job_description><![CDATA[{job_id}]]></ivec:job_description><vcn:host_environment>linux</vcn:host_environment></ivec:param_set></ivec:contents></cmd>"#
        )
    } else {
        format!(
            r#"<?xml version="1.0" encoding="utf-8" ?><cmd xmlns:ivec="{IVEC_COMMON_NS}"><ivec:contents><ivec:operation>StartJob</ivec:operation><ivec:param_set servicetype="{service_type}"><ivec:jobID>{job_id:08}</ivec:jobID><ivec:bidi>1</ivec:bidi></ivec:param_set></ivec:contents></cmd>"#
        )
    }
}

/// Generates the `StartJob` IVEC XML command matching Canon `cmdtocanonij3` maintenance framing.
pub fn build_maintenance_start_job(job_id: u32) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8" ?><cmd xmlns:ivec="{IVEC_COMMON_NS}"><ivec:contents><ivec:operation>StartJob</ivec:operation><ivec:param_set servicetype="maintenance"><ivec:jobID>{job_id:08}</ivec:jobID><ivec:bidi>0</ivec:bidi><ivec:jobname/><ivec:username/><ivec:computername/><ivec:job_description><![CDATA[1]]></ivec:job_description></ivec:param_set></ivec:contents></cmd>"#
    )
}

/// Generates the `StartJob` IVEC XML command for BJR520 (`BJRaster3`) print jobs (`CA_BJR520_IJP`).
pub fn build_bjraster_start_job(job_id: u32) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8" ?><cmd xmlns:ivec="{IVEC_COMMON_NS}"><ivec:contents><ivec:operation>StartJob</ivec:operation><ivec:param_set servicetype="print"><ivec:jobID>{job_id:08}</ivec:jobID><ivec:bidi>0</ivec:bidi></ivec:param_set></ivec:contents></cmd>"#
    )
}

/// Generates the `ModeShift` IVEC XML command for BJR520 (`BJRaster3`) print jobs (`CA_BJR520_IJP`).
pub fn build_bjraster_mode_shift(job_id: u32) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8" ?><cmd xmlns:ivec="{IVEC_COMMON_NS}" xmlns:vcn="{VCN_CANON_NS}"><ivec:contents><ivec:operation>VendorCmd</ivec:operation><ivec:param_set servicetype="print"><vcn:ijoperation>ModeShift</vcn:ijoperation><vcn:ijmode>1</vcn:ijmode><ivec:jobID>{job_id:08}</ivec:jobID></ivec:param_set></ivec:contents></cmd>"#
    )
}

/// Generates the `SetJobConfiguration` IVEC XML command for a maintenance session.
pub fn build_set_maintenance_job_configuration(job_id: u32, datetime: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8" ?><cmd xmlns:ivec="{IVEC_COMMON_NS}" xmlns:vcn="{VCN_CANON_NS}"><ivec:contents><ivec:operation>SetJobConfiguration</ivec:operation><ivec:param_set servicetype="maintenance"><ivec:jobID>{job_id:08}</ivec:jobID><ivec:datetime>{datetime}</ivec:datetime></ivec:param_set></ivec:contents></cmd>"#
    )
}

/// Generates the `SetConfiguration` IVEC XML command for a print job.
pub fn build_set_print_configuration(config: &PrintJobConfig) -> String {
    let borderless_str = if config.borderless.is_borderless() {
        "ON"
    } else {
        "OFF"
    };
    let colormode_str = if config.color.grayscale {
        "monochrome"
    } else {
        "color"
    };
    let duplex_print = match config.duplex {
        DuplexMode::Off => "OFF",
        DuplexMode::LongEdge | DuplexMode::ShortEdge => "ON",
    };

    format!(
        r#"<?xml version="1.0" encoding="utf-8" ?><cmd xmlns:ivec="{IVEC_COMMON_NS}"><ivec:contents><ivec:operation>SetConfiguration</ivec:operation><ivec:param_set servicetype="print"><ivec:jobID>{job_id:08}</ivec:jobID><ivec:papersize>{papersize}</ivec:papersize><ivec:papertype>{mediatype}</ivec:papertype><ivec:borderlessprint>{borderless_str}</ivec:borderlessprint><ivec:printcolormode>{colormode_str}</ivec:printcolormode><ivec:duplexprint>{duplex_print}</ivec:duplexprint></ivec:param_set></ivec:contents></cmd>"#,
        job_id = config.job_id,
        papersize = config.paper_size.ivec_xml_value(),
        mediatype = config.media_type.ivec_xml_value(),
    )
}

/// Generates the `SendData` IVEC XML header for BJL time synchronization.
///
/// Format is `RAW` with exact data size `62` bytes.
pub fn build_bjl_set_time_send_data(job_id: u32) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8" ?><cmd xmlns:ivec="{IVEC_COMMON_NS}"><ivec:contents><ivec:operation>SendData</ivec:operation><ivec:param_set servicetype="print"><ivec:jobID>{job_id:08}</ivec:jobID><ivec:format>RAW</ivec:format><ivec:datasize>62</ivec:datasize></ivec:param_set></ivec:contents></cmd>"#
    )
}

/// Generates the per-page `SetPageConfiguration` IVEC XML command from a `PrintJobConfig`.
pub fn build_set_page_configuration(config: &PrintJobConfig, is_last_page: bool) -> String {
    let nextpage_tag = if is_last_page {
        "<vcn:nextpage>OFF</vcn:nextpage>"
    } else {
        "<vcn:nextpage>ON</vcn:nextpage>"
    };
    format!(
        r#"<?xml version="1.0" encoding="utf-8" ?><cmd xmlns:ivec="{IVEC_COMMON_NS}" xmlns:vcn="{VCN_CANON_NS}"><ivec:contents><ivec:operation>VendorCmd</ivec:operation><ivec:param_set servicetype="print"><ivec:jobID>{job_id:08}</ivec:jobID><vcn:ijoperation>SetPageConfiguration</vcn:ijoperation>{nextpage_tag}</ivec:param_set></ivec:contents></cmd>"#,
        job_id = config.job_id
    )
}

/// Generates the `SendData` IVEC XML header preceding page raster payloads.
pub fn build_send_data_header(job_id: u32, page_no: u32, format: &str, data_size: usize) -> String {
    let _ = page_no;
    format!(
        r#"<?xml version="1.0" encoding="utf-8" ?><cmd xmlns:ivec="{IVEC_COMMON_NS}"><ivec:contents><ivec:operation>SendData</ivec:operation><ivec:param_set servicetype="print"><ivec:jobID>{job_id:08}</ivec:jobID><ivec:format>{format}</ivec:format><ivec:datasize>{data_size}</ivec:datasize></ivec:param_set></ivec:contents></cmd>"#
    )
}

/// Generates the `ModeShift` IVEC XML command (e.g. switching interface 0 into binary MP2 mode).
pub fn build_mode_shift(service_type: &str, job_id: u32, mode: u8) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8" ?><cmd xmlns:ivec="{IVEC_COMMON_NS}" xmlns:vcn="{VCN_CANON_NS}"><ivec:contents><ivec:operation>VendorCmd</ivec:operation><ivec:param_set servicetype="{service_type}"><ivec:jobID>{job_id:08}</ivec:jobID><vcn:ijoperation>ModeShift</vcn:ijoperation><vcn:ijmode>{mode}</vcn:ijmode></ivec:param_set></ivec:contents></cmd>"#
    )
}

/// Generates the `EndJob` IVEC XML command terminating an active job.
pub fn build_end_job(service_type: &str, job_id: u32) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8" ?><cmd xmlns:ivec="{IVEC_COMMON_NS}"><ivec:contents><ivec:operation>EndJob</ivec:operation><ivec:param_set servicetype="{service_type}"><ivec:jobID>{job_id:08}</ivec:jobID></ivec:param_set></ivec:contents></cmd>"#
    )
}

/// Generates the `CancelJob` IVEC XML command aborting an active job immediately.
pub fn build_cancel_job(service_type: &str, job_id: u32) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8" ?><cmd xmlns:ivec="{IVEC_COMMON_NS}"><ivec:contents><ivec:operation>CancelJob</ivec:operation><ivec:param_set servicetype="{service_type}"><ivec:jobID>{job_id:08}</ivec:jobID></ivec:param_set></ivec:contents></cmd>"#
    )
}

// ---------------------------------------------------------------------------
// Maintenance XML Builders (servicetype="maintenance")
// ---------------------------------------------------------------------------

/// Generates the `Cleaning` IVEC maintenance command (`regular`, `deep`, or `choke`).
pub fn build_cleaning_cmd(job_id: u32, kind: CleaningType, target: CleaningTarget) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?><cmd xmlns:ivec="{IVEC_COMMON_NS}"><ivec:contents><ivec:operation>Cleaning</ivec:operation><ivec:param_set servicetype="maintenance"><ivec:inkgroup>{target}</ivec:inkgroup><ivec:type>{kind}</ivec:type><ivec:jobID>{job_id:08}</ivec:jobID></ivec:param_set></ivec:contents></cmd>"#,
        kind = kind.ivec_xml_value(),
        target = target.ivec_xml_value(),
    )
}

/// Generates the `RollerCleaning` IVEC maintenance command (`roller` or `platen`).
pub fn build_roller_cleaning_cmd(job_id: u32, target: RollerCleanTarget) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?><cmd xmlns:ivec="{IVEC_COMMON_NS}"><ivec:contents><ivec:operation>RollerCleaning</ivec:operation><ivec:param_set servicetype="maintenance"><ivec:target>{target}</ivec:target><ivec:jobID>{job_id:08}</ivec:jobID></ivec:param_set></ivec:contents></cmd>"#,
        target = target.ivec_xml_value(),
    )
}

/// Generates the `TestPrint` IVEC diagnostic pattern command (e.g. `nozzle_check`).
pub fn build_test_print_cmd(job_id: u32, kind: TestPrintType) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?><cmd xmlns:ivec="{IVEC_COMMON_NS}"><ivec:contents><ivec:operation>TestPrint</ivec:operation><ivec:param_set servicetype="maintenance"><ivec:type>{kind}</ivec:type><ivec:jobID>{job_id:08}</ivec:jobID></ivec:param_set></ivec:contents></cmd>"#,
        kind = kind.ivec_xml_value(),
    )
}

/// Generates the manual head alignment adjustment values command.
pub fn build_manual_alignment_values_cmd(job_id: u32, column_values: &[i8]) -> String {
    let mut columns_xml = String::new();
    for (i, &val) in column_values.iter().enumerate() {
        let col_name = (b'A' + (i as u8).min(25)) as char;
        columns_xml.push_str(&format!(
            "        <vcn:alignment column=\"{col_name}\">{val}</vcn:alignment>\n"
        ));
    }

    format!(
        r#"<?xml version="1.0" encoding="utf-8" ?>
<cmd xmlns:ivec="{IVEC_COMMON_NS}" xmlns:vcn="{VCN_CANON_NS}">
  <ivec:contents>
    <ivec:operation>VendorCmd</ivec:operation>
    <ivec:param_set servicetype="maintenance">
      <vcn:ijoperation>SetAlignmentPattern</vcn:ijoperation>
      <ivec:jobID>{job_id:08}</ivec:jobID>
      <vcn:alignment_values>
{columns_xml}      </vcn:alignment_values>
    </ivec:param_set>
  </ivec:contents>
</cmd>"#
    )
}

// ---------------------------------------------------------------------------
// Status Telemetry XML Builders and Parsers (servicetype="status")
// ---------------------------------------------------------------------------

/// Generates the `GetStatus` query IVEC XML command.
pub fn build_get_status_cmd() -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8" ?>
<cmd xmlns:ivec="{IVEC_COMMON_NS}">
  <ivec:contents>
    <ivec:operation>GetStatus</ivec:operation>
    <ivec:param_set servicetype="status"/>
  </ivec:contents>
</cmd>"#
    )
}

/// Generates the `GetCapability` query IVEC XML command for maintenance operations.
pub fn build_get_maintenance_capability_cmd() -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8" ?>
<cmd xmlns:ivec="{IVEC_COMMON_NS}">
  <ivec:contents>
    <ivec:operation>GetCapability</ivec:operation>
    <ivec:param_set servicetype="maintenance"/>
  </ivec:contents>
</cmd>"#
    )
}

/// Rich IVEC `GetStatusResponse` parse output distinguishing hardware-reported
/// fields from protocol defaults.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ParsedStatusReport {
    pub status: DeviceStatus,
    pub ivec_operation: Option<String>,
    pub ivec_response: Option<String>,
    pub ivec_response_detail: Option<String>,
    pub raw_status: Option<String>,
    pub raw_status_detail: Option<String>,
    pub raw_support_code: Option<String>,
    pub firmware_reported_ink: bool,
    pub firmware_reported_waste: bool,
    pub firmware_reported_version: bool,
    pub raw_xml: String,
}

impl ParsedStatusReport {
    /// Returns `true` if this XML message is a `GetStatusResponse` rather than an
    /// asynchronous job reply such as `StartJobResponse` or `EndJobResponse`.
    pub fn is_status_response(&self) -> bool {
        match self.ivec_operation.as_deref() {
            Some(op) => {
                op.eq_ignore_ascii_case("GetStatusResponse") || op.eq_ignore_ascii_case("GetStatus")
            }
            None => {
                self.raw_status.is_some() || self.ivec_response.as_deref() == Some("GetStatus")
            }
        }
    }

    /// Returns `true` if this XML message is a `GetCapabilityResponse`.
    pub fn is_capability_response(&self) -> bool {
        self.ivec_operation.as_deref().is_some_and(|op| {
            op.eq_ignore_ascii_case("GetCapabilityResponse")
                || op.eq_ignore_ascii_case("GetCapability")
        })
    }
}

/// Parses an IVEC XML status response into a structured `DeviceStatus`.
pub fn parse_status_response(xml: &str) -> Result<DeviceStatus, ProtocolError> {
    parse_status_response_with_meta(xml).map(|report| report.status)
}

/// Parses an IVEC XML status response while retaining raw firmware fields and
/// explicit flags indicating which telemetry sections the printer actually emitted.
pub fn parse_status_response_with_meta(xml: &str) -> Result<ParsedStatusReport, ProtocolError> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut status = DeviceStatus::default();
    let mut current_tag = String::new();
    let mut ivec_operation: Option<String> = None;
    let mut ivec_response: Option<String> = None;
    let mut ivec_response_detail: Option<String> = None;
    let mut status_str = String::new();
    let mut status_detail_str = String::new();
    let mut raw_support_code: Option<String> = None;
    let mut support_code_opt: Option<String> = None;
    let mut in_maintenance_param_set = false;
    let mut maintenance_operation: Option<String> = None;
    let mut maintenance_jobprogress: Option<String> = None;
    let mut firmware_reported_ink = false;
    let mut firmware_reported_waste = false;
    let mut firmware_reported_version = false;
    let mut saw_xml_tag = false;

    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                saw_xml_tag = true;
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                current_tag = normalize_tag(&name);

                if current_tag == "param_set" {
                    in_maintenance_param_set = e.attributes().flatten().any(|attr| {
                        attr.key.as_ref().eq_ignore_ascii_case(b"servicetype")
                            && String::from_utf8_lossy(&attr.value)
                                .trim()
                                .eq_ignore_ascii_case("maintenance")
                    });
                }

                // Check self-closing or inline attributes for ink and wastetank
                if current_tag == "ink" || current_tag == "color" {
                    if parse_ink_attributes(e, &mut status) {
                        firmware_reported_ink = true;
                    }
                } else if (current_tag == "wastetank" || current_tag == "dcounter")
                    && parse_waste_attributes(e, &mut status)
                {
                    firmware_reported_waste = true;
                }
            }
            Ok(Event::Empty(ref e)) => {
                saw_xml_tag = true;
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                let tag = normalize_tag(&name);
                if tag == "ink" || tag == "color" {
                    if parse_ink_attributes(e, &mut status) {
                        firmware_reported_ink = true;
                    }
                } else if (tag == "wastetank" || tag == "dcounter")
                    && parse_waste_attributes(e, &mut status)
                {
                    firmware_reported_waste = true;
                }
            }
            Ok(Event::Text(ref e)) => {
                let text = e
                    .unescape()
                    .map_err(|err| ProtocolError::Xml(err.to_string()))?;
                let text = text.trim();
                if text.is_empty() {
                    continue;
                }
                match current_tag.as_str() {
                    "operation" => ivec_operation = Some(text.to_string()),
                    "response" => ivec_response = Some(text.to_string()),
                    "response_detail" | "responsedetail" => {
                        ivec_response_detail = Some(text.to_string())
                    }
                    "status" => status_str = text.to_lowercase(),
                    "statusdetail" | "status_detail" => status_detail_str = text.to_lowercase(),
                    "maintenance_operation" | "maintenanceoperation" if in_maintenance_param_set => {
                        maintenance_operation = Some(text.to_lowercase());
                    }
                    "jobprogress" | "job_progress" if in_maintenance_param_set => {
                        maintenance_jobprogress = Some(text.to_lowercase());
                    }
                    "supportcode" | "current_support_code" => {
                        let code = text.to_string();
                        raw_support_code = Some(code.clone());
                        if !code.is_empty() && code != "0000" {
                            support_code_opt = Some(code);
                        }
                    }
                    "serialnumber" | "serial_number" => status.serial_number = text.to_string(),
                    "firmwareversion" | "firmware_version" => {
                        status.firmware_version = text.to_string();
                        firmware_reported_version = true;
                    }
                    "model" => {
                        if text.contains("G1010") {
                            status.model = PrinterModel::G1010;
                        } else if text.contains("G3010") {
                            status.model = PrinterModel::G3010;
                        } else if text.contains("G4010") {
                            status.model = PrinterModel::G4010;
                        } else {
                            status.model = PrinterModel::G2010;
                        }
                    }
                    "wastetank" | "dcounter" => {
                        if let Ok(pct) = text.parse::<f32>() {
                            status.waste_absorber_percent = pct;
                            firmware_reported_waste = true;
                        }
                    }
                    "ink_bk" | "bk" => {
                        if let Ok(lvl) = text.parse::<u8>() {
                            status.ink_levels.pigment_black_percent = lvl;
                            firmware_reported_ink = true;
                        }
                    }
                    "ink_c" | "c" => {
                        if let Ok(lvl) = text.parse::<u8>() {
                            status.ink_levels.cyan_percent = lvl;
                            firmware_reported_ink = true;
                        }
                    }
                    "ink_m" | "m" => {
                        if let Ok(lvl) = text.parse::<u8>() {
                            status.ink_levels.magenta_percent = lvl;
                            firmware_reported_ink = true;
                        }
                    }
                    "ink_y" | "y" => {
                        if let Ok(lvl) = text.parse::<u8>() {
                            status.ink_levels.yellow_percent = lvl;
                            firmware_reported_ink = true;
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::End(ref e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if normalize_tag(&name) == "param_set" {
                    in_maintenance_param_set = false;
                }
                current_tag.clear();
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(e) => return Err(ProtocolError::Xml(e.to_string())),
        }
        buf.clear();
    }

    if !saw_xml_tag {
        return Err(ProtocolError::Xml(
            "No IVEC XML elements found in response".to_string(),
        ));
    }

    // Map status detail and operational state
    status.status_detail = if !status_detail_str.is_empty() {
        status_detail_str.clone()
    } else if !status_str.is_empty() {
        status_str.clone()
    } else {
        "idle".to_string()
    };

    status.state = match status_detail_str.as_str() {
        "coveropen" => OperationalState::CoverOpen,
        "paperout" => OperationalState::PaperOut,
        "paperjam" => OperationalState::PaperJam,
        "cleaning" => OperationalState::Cleaning,
        "testing" => OperationalState::Testing,
        "waiting" => OperationalState::Waiting,
        "printing" => OperationalState::Printing,
        "scanning" => OperationalState::Scanning,
        _ => match status_str.as_str() {
            "idle" => OperationalState::Idle,
            "processing" => {
                let is_maint_processing = matches!(
                    maintenance_jobprogress.as_deref(),
                    None | Some("processing")
                );
                if is_maint_processing {
                    match maintenance_operation.as_deref() {
                        Some("cleaning" | "rollercleaning" | "roller_cleaning") => {
                            OperationalState::Cleaning
                        }
                        Some("testprint" | "test_print" | "alignment") => OperationalState::Testing,
                        _ => OperationalState::Printing,
                    }
                } else {
                    OperationalState::Printing
                }
            }
            "stopped" => OperationalState::Offline,
            "support_error" => OperationalState::ServiceError,
            "operator_call" => OperationalState::OperatorCall,
            _ => OperationalState::Idle,
        },
    };

    // Process support code and printhead health
    if let Some(ref code) = support_code_opt {
        status.support_code = Some(code.clone());
        if let Some(info) = SupportCodeInfo::lookup(code) {
            status.lcd_code = Some(info.lcd_code.to_string());
        }

        match code.as_str() {
            "1401" | "1403" => {
                status.black_head_state = CartridgeState::NotRecognized;
                status.color_head_state = CartridgeState::NotRecognized;
            }
            "1470" => {
                status.black_head_state = CartridgeState::NotRecognized;
            }
            "1471" => {
                status.black_head_state = CartridgeState::NotInstalled;
                status.color_head_state = CartridgeState::NotInstalled;
            }
            "1472" => {
                status.black_head_state = CartridgeState::LockUnlatched;
                status.color_head_state = CartridgeState::LockUnlatched;
            }
            "5200" => {
                status.black_head_state = CartridgeState::Overheated;
                status.color_head_state = CartridgeState::Overheated;
            }
            _ => {}
        }
    }

    // Only flag low ink warning when the firmware actually reported ink levels.
    if firmware_reported_ink
        && (status.ink_levels.pigment_black_percent <= 15
            || status.ink_levels.cyan_percent <= 15
            || status.ink_levels.magenta_percent <= 15
            || status.ink_levels.yellow_percent <= 15)
    {
        status.ink_levels.low_ink_warning = true;
    }

    Ok(ParsedStatusReport {
        status,
        ivec_operation,
        ivec_response,
        ivec_response_detail,
        raw_status: if status_str.is_empty() {
            None
        } else {
            Some(status_str)
        },
        raw_status_detail: if status_detail_str.is_empty() {
            None
        } else {
            Some(status_detail_str)
        },
        raw_support_code,
        firmware_reported_ink,
        firmware_reported_waste,
        firmware_reported_version,
        raw_xml: xml.to_string(),
    })
}

/// Parses `<ivec:Cleaning>` sections from an IVEC `GetCapabilityResponse` (`servicetype="maintenance"`),
/// returning `(cleaning_type, supported_ink_groups)` pairs such as `("regular", ["all", "group1", "group2"])`.
pub fn parse_maintenance_cleaning_groups(xml: &str) -> Result<Vec<(String, Vec<String>)>, ProtocolError> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut out = Vec::new();
    let mut current_tag = String::new();
    let mut in_cleaning = false;
    let mut current_type: Option<String> = None;
    let mut current_groups: Vec<String> = Vec::new();
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                current_tag = normalize_tag(&name);
                if current_tag == "cleaning" {
                    in_cleaning = true;
                } else if in_cleaning && current_tag == "type" {
                    current_type = e.attributes().flatten().find_map(|attr| {
                        attr.key
                            .as_ref()
                            .eq_ignore_ascii_case(b"value")
                            .then(|| String::from_utf8_lossy(&attr.value).trim().to_lowercase())
                            .filter(|s| !s.is_empty())
                    });
                    current_groups.clear();
                }
            }
            Ok(Event::Text(ref e)) => {
                if in_cleaning && current_tag == "inkgroup" && current_type.is_some() {
                    let text = e
                        .unescape()
                        .map_err(|err| ProtocolError::Xml(err.to_string()))?;
                    for token in text.split(',').map(|s| s.trim().to_lowercase()) {
                        if !token.is_empty() && !current_groups.contains(&token) {
                            current_groups.push(token);
                        }
                    }
                }
            }
            Ok(Event::End(ref e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                let tag = normalize_tag(&name);
                if tag == "type" {
                    if let Some(kind) = current_type.take() {
                        out.push((kind, std::mem::take(&mut current_groups)));
                    }
                } else if tag == "cleaning" {
                    in_cleaning = false;
                    current_type = None;
                    current_groups.clear();
                }
                current_tag.clear();
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(e) => return Err(ProtocolError::Xml(e.to_string())),
        }
        buf.clear();
    }

    Ok(out)
}

fn normalize_tag(tag: &str) -> String {
    let raw = tag.split(':').next_back().unwrap_or(tag);
    raw.to_lowercase()
}

fn parse_ink_attributes(e: &quick_xml::events::BytesStart, status: &mut DeviceStatus) -> bool {
    let mut color = String::new();
    let mut level: Option<u8> = None;

    for attr in e.attributes().flatten() {
        let key = String::from_utf8_lossy(attr.key.as_ref()).to_lowercase();
        let val = String::from_utf8_lossy(&attr.value).to_string();

        if key == "id" || key == "color" {
            color = val.to_uppercase();
        } else if key == "level" || key == "percent" {
            if let Ok(v) = val.parse::<u8>() {
                level = Some(v);
            }
        }
    }

    let Some(level) = level else {
        return false;
    };

    match color.as_str() {
        "BK" | "BLACK" | "PGBK" => {
            status.ink_levels.pigment_black_percent = level;
            true
        }
        "C" | "CYAN" => {
            status.ink_levels.cyan_percent = level;
            true
        }
        "M" | "MAGENTA" => {
            status.ink_levels.magenta_percent = level;
            true
        }
        "Y" | "YELLOW" => {
            status.ink_levels.yellow_percent = level;
            true
        }
        _ => false,
    }
}

fn parse_waste_attributes(e: &quick_xml::events::BytesStart, status: &mut DeviceStatus) -> bool {
    let mut updated = false;
    for attr in e.attributes().flatten() {
        let key = String::from_utf8_lossy(attr.key.as_ref()).to_lowercase();
        let val = String::from_utf8_lossy(&attr.value).to_string();

        if key == "percent" || key == "level" {
            if let Ok(v) = val.parse::<f32>() {
                status.waste_absorber_percent = v;
                updated = true;
            }
        }
    }
    updated
}

#[derive(Default)]
struct RawCmdCapture {
    is_cleaning: bool,
    is_send_data: bool,
    datasize: Option<usize>,
    kinds: Vec<CleaningType>,
}

enum RawCaptureField {
    Operation,
    Kind,
    DataSize,
}

fn find_raw_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn bounded_find_subslice(
    data: &[u8],
    start: usize,
    bound: usize,
    needle: &[u8],
    failed_until: &mut usize,
) -> Option<usize> {
    let clamped_bound = bound.min(data.len());
    if start + needle.len() > clamped_bound || clamped_bound <= *failed_until {
        return None;
    }
    let eff_start = start.max(failed_until.saturating_sub(needle.len().saturating_sub(1)));
    if let Some(rel) = find_raw_subslice(&data[eff_start..clamped_bound], needle) {
        Some(eff_start + rel)
    } else {
        *failed_until = (*failed_until).max(clamped_bound);
        None
    }
}

/// Scans an XML opening tag slice (`after '<'`) for its unquoted closing `>`.
/// Aborts immediately on any `<` (illegal inside XML 1.0 tag headers and attribute values)
/// so malformed `<tag` sequences cannot swallow subsequent `<cmd>` elements or go quadratic.
fn find_xml_open_tag_end(slice: &[u8]) -> Result<usize, Option<usize>> {
    let mut quote: Option<u8> = None;
    for (idx, &b) in slice.iter().enumerate() {
        if b == b'<' {
            return Err(Some(idx));
        }
        match quote {
            Some(q) if b == q => quote = None,
            Some(_) => {}
            None if b == b'"' || b == b'\'' => quote = Some(b),
            None if b == b'>' => return Ok(idx),
            None => {}
        }
    }
    Err(None)
}

fn is_xml_markup_decl_start(after_bang: &[u8]) -> bool {
    const KEYWORDS: [&[u8]; 5] = [
        b"DOCTYPE",
        b"ELEMENT",
        b"ATTLIST",
        b"ENTITY",
        b"NOTATION",
    ];
    KEYWORDS.iter().any(|kw| {
        after_bang.len() > kw.len()
            && after_bang[..kw.len()].eq_ignore_ascii_case(kw)
            && after_bang[kw.len()].is_ascii_whitespace()
    })
}

/// Scans a `<!(DOCTYPE|...)` declaration respecting quotes, `[...]` internal subsets,
/// and DTD comments while aborting if an invalid element tag like `<cmd>` appears unquoted.
fn find_xml_decl_end(slice: &[u8]) -> Result<usize, Option<usize>> {
    let mut quote: Option<u8> = None;
    let mut bracket_depth = 0usize;
    let mut j = 0usize;
    while j < slice.len() {
        if let Some(q) = quote {
            if slice[j] == q {
                quote = None;
            }
            j += 1;
            continue;
        }
        if bracket_depth > 0 && slice[j..].starts_with(b"<!--") {
            let Some(end_rel) = find_raw_subslice(&slice[j + 4..], b"-->") else {
                return Err(None);
            };
            j = j + 4 + end_rel + 3;
            continue;
        }
        if bracket_depth > 0 && slice[j..].starts_with(b"<?") {
            let Some(end_rel) = find_raw_subslice(&slice[j + 2..], b"?>") else {
                return Err(None);
            };
            j = j + 2 + end_rel + 2;
            continue;
        }
        match slice[j] {
            b'"' | b'\'' => quote = Some(slice[j]),
            b'[' => bracket_depth += 1,
            b']' => bracket_depth = bracket_depth.saturating_sub(1),
            b'<' => {
                if bracket_depth == 0
                    || !matches!(slice.get(j + 1).copied(), Some(b'!') | Some(b'?'))
                {
                    return Err(Some(j));
                }
            }
            b'>' if bracket_depth == 0 => return Ok(j),
            _ => {}
        }
        j += 1;
    }
    Err(None)
}

fn trim_xml_local_name(raw: &[u8]) -> Option<&str> {
    let s = std::str::from_utf8(raw).ok()?.trim();
    Some(s.rsplit(':').next().unwrap_or(s))
}

fn decode_xml_text_or_raw(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    if let Ok(decoded) = quick_xml::escape::unescape(s) {
        return decoded.into_owned();
    }
    // Piecewise fallback so one unrecognized entity cannot block decoding adjacent valid refs.
    // Scan only ASCII entity-name / char-ref characters after '&' so offsets stay UTF-8 char-aligned.
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0usize;
    while i < bytes.len() {
        let Some(amp_rel) = s[i..].find('&') else {
            out.push_str(&s[i..]);
            break;
        };
        let amp = i + amp_rel;
        out.push_str(&s[i..amp]);
        let mut end = amp + 1;
        while end < bytes.len()
            && matches!(bytes[end], b'#' | b'0'..=b'9' | b'a'..=b'z' | b'A'..=b'Z')
        {
            end += 1;
        }
        if bytes.get(end) == Some(&b';') {
            let candidate = &s[amp..=end];
            if let Ok(decoded) = quick_xml::escape::unescape(candidate) {
                out.push_str(&decoded);
            } else {
                out.push_str(candidate);
            }
            i = end + 1;
        } else {
            // None of s[amp + 1..end] is '&' or ';', so advance directly to `end` in O(N).
            out.push_str(&s[amp..end]);
            i = end;
        }
    }
    out
}

fn append_raw_captured_bytes(
    capture: &mut Option<(RawCaptureField, Vec<u8>)>,
    chunk: &[u8],
    unescape_refs: bool,
) {
    const MAX_CAPTURE_BYTES: usize = 256;
    let Some((_, buf)) = capture.as_mut() else {
        return;
    };
    if buf.len() >= MAX_CAPTURE_BYTES || chunk.is_empty() {
        return;
    }
    let normalized;
    let slice: &[u8] = if unescape_refs && chunk.contains(&b'&') {
        normalized = decode_xml_text_or_raw(&String::from_utf8_lossy(chunk));
        normalized.as_bytes()
    } else {
        chunk
    };
    let slice = if buf.is_empty() {
        slice.trim_ascii_start()
    } else {
        slice
    };
    let remaining = MAX_CAPTURE_BYTES.saturating_sub(buf.len());
    if remaining > 0 && !slice.is_empty() {
        let take = slice.len().min(remaining);
        buf.extend_from_slice(&slice[..take]);
    }
}

fn record_raw_captured_element(target: &mut RawCmdCapture, field: RawCaptureField, buf: &[u8]) {
    let val = String::from_utf8_lossy(buf);
    let val = val.trim();
    match field {
        RawCaptureField::Operation => {
            if val == "Cleaning" {
                target.is_cleaning = true;
            } else if val == "SendData" {
                target.is_send_data = true;
            }
        }
        RawCaptureField::Kind => match val {
            "regular" => target.kinds.push(CleaningType::Regular),
            "deep" => target.kinds.push(CleaningType::Deep),
            "choke" => target.kinds.push(CleaningType::InkFlush),
            _ => {}
        },
        RawCaptureField::DataSize => {
            if let Ok(sz) = val.parse::<usize>() {
                target.datasize = Some(sz);
            }
        }
    }
}

/// Extracts all IVEC `Cleaning` operations (`regular`, `deep`, `choke` / InkFlush) from a raw
/// USB byte stream containing IVEC `<cmd>` XML blocks (potentially interleaved with NUL padding,
/// XML processing instructions, DTD internal subsets, comments, or binary `SendData` raster payloads).
///
/// Runs in guaranteed `O(N)` time so crafted or malformed streams cannot cause backtracking DoS
/// or bypass safety interlocks before USB transmission.
pub fn extract_ivec_cleaning_actions(data: &[u8]) -> Vec<CleaningType> {
    let mut actions = Vec::new();
    let mut in_cmd: Option<RawCmdCapture> = None;
    let mut fallback = RawCmdCapture::default();
    let mut capture: Option<(RawCaptureField, Vec<u8>)> = None;

    let mut no_comment_end_before = 0usize;
    let mut no_cdata_end_before = 0usize;
    let mut no_pi_end_before = 0usize;
    let mut no_decl_end_before = 0usize;
    let mut send_data_end: Option<usize> = None;

    let mut pos = 0usize;
    while pos < data.len() {
        let Some(rel_lt) = data[pos..].iter().position(|&b| b == b'<') else {
            append_raw_captured_bytes(&mut capture, &data[pos..], true);
            break;
        };
        let lt = pos + rel_lt;
        append_raw_captured_bytes(&mut capture, &data[pos..lt], true);

        if let Some(end) = send_data_end {
            if lt >= end {
                send_data_end = None;
            }
        }
        let scan_bound = send_data_end.unwrap_or(data.len()).max(lt + 1);
        let rest = &data[lt + 1..scan_bound];

        if rest.starts_with(b"!--") {
            if let Some(end_abs) = bounded_find_subslice(
                data,
                lt + 4,
                scan_bound,
                b"-->",
                &mut no_comment_end_before,
            ) {
                pos = end_abs + 3;
            } else {
                pos = lt + 4;
            }
            continue;
        }

        if rest.starts_with(b"![CDATA[") {
            if let Some(end_abs) = bounded_find_subslice(
                data,
                lt + 9,
                scan_bound,
                b"]]>",
                &mut no_cdata_end_before,
            ) {
                append_raw_captured_bytes(&mut capture, &data[lt + 9..end_abs], false);
                pos = end_abs + 3;
            } else {
                pos = lt + 9;
            }
            continue;
        }

        if rest.starts_with(b"?") {
            if let Some(end_abs) =
                bounded_find_subslice(data, lt + 2, scan_bound, b"?>", &mut no_pi_end_before)
            {
                pos = end_abs + 2;
            } else {
                pos = lt + 2;
            }
            continue;
        }

        if rest.starts_with(b"!") {
            let decl_slice = &data[lt + 2..scan_bound];
            if is_xml_markup_decl_start(decl_slice) {
                if scan_bound > no_decl_end_before {
                    match find_xml_decl_end(decl_slice) {
                        Ok(end_rel) => {
                            pos = lt + 2 + end_rel + 1;
                            continue;
                        }
                        Err(Some(next_lt_rel)) => {
                            pos = lt + 2 + next_lt_rel;
                            continue;
                        }
                        Err(None) => {
                            no_decl_end_before = no_decl_end_before.max(scan_bound);
                        }
                    }
                }
                pos = lt + 2;
                continue;
            }
            match decl_slice.iter().position(|&b| b == b'>' || b == b'<') {
                Some(idx) if decl_slice[idx] == b'>' => pos = lt + 2 + idx + 1,
                Some(idx) => pos = lt + 2 + idx,
                None if scan_bound == data.len() => break,
                None => pos = scan_bound,
            }
            continue;
        }

        if rest.starts_with(b"/") {
            let after_slash = &rest[1..];
            let Some(stop) = after_slash.iter().position(|&b| b == b'>' || b == b'<') else {
                if scan_bound == data.len() {
                    break;
                }
                pos = scan_bound;
                continue;
            };
            if after_slash[stop] == b'<' {
                pos = lt + 2 + stop;
                continue;
            }
            let close_name = &after_slash[..stop];
            let starts_valid_name = close_name
                .iter()
                .find(|b| !b.is_ascii_whitespace())
                .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_');
            let next_pos = lt + 2 + stop + 1;
            if starts_valid_name {
                if let Some(local) = trim_xml_local_name(close_name) {
                    match local {
                        "operation"
                            if matches!(capture, Some((RawCaptureField::Operation, _))) =>
                        {
                            if let Some((field, buf)) = capture.take() {
                                let target = in_cmd.as_mut().unwrap_or(&mut fallback);
                                record_raw_captured_element(target, field, &buf);
                            }
                        }
                        "type" if matches!(capture, Some((RawCaptureField::Kind, _))) => {
                            if let Some((field, buf)) = capture.take() {
                                let target = in_cmd.as_mut().unwrap_or(&mut fallback);
                                record_raw_captured_element(target, field, &buf);
                            }
                        }
                        "datasize" if matches!(capture, Some((RawCaptureField::DataSize, _))) => {
                            if let Some((field, buf)) = capture.take() {
                                let target = in_cmd.as_mut().unwrap_or(&mut fallback);
                                record_raw_captured_element(target, field, &buf);
                            }
                        }
                        "cmd" => {
                            capture = None;
                            if let Some(cap) = in_cmd.take() {
                                if cap.is_cleaning {
                                    actions.extend(cap.kinds);
                                } else if cap.is_send_data {
                                    if let Some(n) = cap.datasize.filter(|&n| n > 0) {
                                        if let Some(end) = next_pos.checked_add(n) {
                                            if end <= data.len() {
                                                send_data_end = Some(end);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            pos = next_pos;
            continue;
        }

        let Some(first) = rest.first().copied() else {
            if scan_bound == data.len() {
                break;
            }
            pos = scan_bound;
            continue;
        };
        if !(first.is_ascii_alphabetic() || first == b'_') {
            pos = lt + 1;
            continue;
        }

        let gt_rel = match find_xml_open_tag_end(rest) {
            Ok(gt_rel) => gt_rel,
            Err(Some(next_lt_rel)) => {
                pos = lt + 1 + next_lt_rel;
                continue;
            }
            Err(None) => {
                if scan_bound == data.len() {
                    break;
                }
                pos = scan_bound;
                continue;
            }
        };
        let open_inner = &rest[..gt_rel];
        let trimmed_inner = open_inner.trim_ascii_end();
        let self_closing = trimmed_inner.ends_with(b"/");
        let name_end = trimmed_inner
            .iter()
            .position(|b| b.is_ascii_whitespace() || *b == b'/')
            .unwrap_or(trimmed_inner.len());

        if !self_closing {
            if let Some(local) = trim_xml_local_name(&trimmed_inner[..name_end]) {
                match local {
                    "cmd" => {
                        capture = None;
                        if let Some(prev) = in_cmd.take() {
                            if prev.is_cleaning {
                                actions.extend(prev.kinds);
                            }
                        }
                        in_cmd = Some(RawCmdCapture::default());
                    }
                    "operation" => capture = Some((RawCaptureField::Operation, Vec::new())),
                    "type" => capture = Some((RawCaptureField::Kind, Vec::new())),
                    "datasize" => capture = Some((RawCaptureField::DataSize, Vec::new())),
                    _ => {}
                }
            }
        }
        pos = lt + 1 + gt_rel + 1;
    }

    if let Some(cap) = in_cmd.take() {
        if cap.is_cleaning {
            actions.extend(cap.kinds);
        }
    }
    if fallback.is_cleaning {
        actions.extend(fallback.kinds);
    }

    actions
}

#[cfg(test)]
mod tests {
    use super::*;
    use pixma_core::maintenance::{CleaningTarget, CleaningType, RollerCleanTarget, TestPrintType};
    use pixma_core::models::OperationalState;
    use pixma_core::print_params::PrintJobConfig;

    #[test]
    fn builds_set_job_configuration_xml() {
        let xml = build_set_job_configuration(1, Some("20261108203500"));
        assert!(xml.contains("<ivec:operation>SetJobConfiguration</ivec:operation>"));
        assert!(xml.contains("<ivec:jobID>00000001</ivec:jobID>"));
        assert!(xml.contains("<ivec:datetime>20261108203500</ivec:datetime>"));
        assert!(xml.contains("<ivec:mismatch_mode>none</ivec:mismatch_mode>"));
    }

    #[test]
    fn builds_start_job_xml() {
        let xml = build_start_job("print", 2);
        assert!(xml.contains("<ivec:operation>StartJob</ivec:operation>"));
        assert!(xml.contains("<ivec:jobID>00000002</ivec:jobID>"));
        assert!(xml.contains("servicetype=\"print\""));
    }

    #[test]
    fn builds_bjl_set_time_send_data_xml() {
        let xml = build_bjl_set_time_send_data(3);
        assert!(xml.contains("<ivec:operation>SendData</ivec:operation>"));
        assert!(xml.contains("<ivec:format>RAW</ivec:format>"));
        assert!(xml.contains("<ivec:datasize>62</ivec:datasize>"));
    }

    #[test]
    fn builds_set_page_configuration_xml() {
        let config = PrintJobConfig::default();
        let xml1 = build_set_page_configuration(&config, false);
        assert!(xml1.contains("<vcn:ijoperation>SetPageConfiguration</vcn:ijoperation>"));
        assert!(xml1.contains("<vcn:nextpage>ON</vcn:nextpage>"));

        let xml2 = build_set_page_configuration(&config, true);
        assert!(xml2.contains("<vcn:ijoperation>SetPageConfiguration</vcn:ijoperation>"));
        assert!(xml2.contains("<vcn:nextpage>OFF</vcn:nextpage>"));
    }

    #[test]
    fn builds_send_data_header_xml() {
        let xml = build_send_data_header(2, 1, "PWGRaster", 123456);
        assert!(xml.contains("<ivec:operation>SendData</ivec:operation>"));
        assert!(xml.contains("<ivec:format>PWGRaster</ivec:format>"));
        assert!(xml.contains("<ivec:datasize>123456</ivec:datasize>"));
    }

    #[test]
    fn builds_mode_shift_end_cancel_xml() {
        let shift = build_mode_shift("scan", 1, 1);
        assert!(shift.contains("<vcn:ijoperation>ModeShift</vcn:ijoperation>"));
        assert!(shift.contains("<vcn:ijmode>1</vcn:ijmode>"));

        let end = build_end_job("print", 1);
        assert!(end.contains("<ivec:operation>EndJob</ivec:operation>"));

        let cancel = build_cancel_job("print", 1);
        assert!(cancel.contains("<ivec:operation>CancelJob</ivec:operation>"));
    }

    #[test]
    fn builds_maintenance_xml_commands() {
        let start = build_maintenance_start_job(1);
        assert!(start.contains("<ivec:operation>StartJob</ivec:operation>"));
        assert!(start.contains("servicetype=\"maintenance\""));
        assert!(start.contains("<ivec:bidi>0</ivec:bidi>"));

        let clean = build_cleaning_cmd(1, CleaningType::InkFlush, CleaningTarget::All);
        assert!(clean.contains("<ivec:operation>Cleaning</ivec:operation>"));
        assert!(clean.contains("<ivec:type>choke</ivec:type>"));
        assert!(clean.contains("<ivec:inkgroup>all</ivec:inkgroup>"));

        let clean_col = build_cleaning_cmd(1, CleaningType::Regular, CleaningTarget::ColorOnly);
        assert!(clean_col.contains("<ivec:inkgroup>group2</ivec:inkgroup>"));
        let clean_bk = build_cleaning_cmd(1, CleaningType::Regular, CleaningTarget::BlackOnly);
        assert!(clean_bk.contains("<ivec:inkgroup>group1</ivec:inkgroup>"));

        let roller = build_roller_cleaning_cmd(2, RollerCleanTarget::FeedRoller);
        assert!(roller.contains("<ivec:operation>RollerCleaning</ivec:operation>"));
        assert!(roller.contains("<ivec:target>roller</ivec:target>"));

        let test_print = build_test_print_cmd(3, TestPrintType::NozzleCheck);
        assert!(test_print.contains("<ivec:operation>TestPrint</ivec:operation>"));
        assert!(test_print.contains("<ivec:type>nozzle_check</ivec:type>"));

        let align = build_manual_alignment_values_cmd(4, &[1, -2, 0]);
        assert!(align.contains("<vcn:ijoperation>SetAlignmentPattern</vcn:ijoperation>"));
        assert!(align.contains("<vcn:alignment column=\"A\">1</vcn:alignment>"));
        assert!(align.contains("<vcn:alignment column=\"B\">-2</vcn:alignment>"));
    }

    #[test]
    fn parses_status_response_cleanly() {
        let xml = r#"<?xml version="1.0" encoding="utf-8" ?>
<cmd xmlns:ivec="http://www.canon.com/ns/cmd/2008/07/common/" xmlns:vcn="http://www.canon.com/ns/cmd/2008/07/canon/">
  <ivec:contents>
    <ivec:response>GetStatus</ivec:response>
    <ivec:param_set servicetype="status">
      <ivec:status>processing</ivec:status>
      <vcn:statusdetail>printing</vcn:statusdetail>
      <vcn:serialnumber>KNDG56789</vcn:serialnumber>
      <vcn:firmwareversion>1.020</vcn:firmwareversion>
      <vcn:ink>
        <vcn:color id="BK" level="75"/>
        <vcn:color id="C" level="60"/>
        <vcn:color id="M" level="80"/>
        <vcn:color id="Y" level="90"/>
      </vcn:ink>
      <vcn:wastetank percent="24.5"/>
    </ivec:param_set>
  </ivec:contents>
</cmd>"#;

        let status = parse_status_response(xml).unwrap();
        assert_eq!(status.state, OperationalState::Printing);
        assert_eq!(status.serial_number, "KNDG56789");
        assert_eq!(status.firmware_version, "1.020");
        assert_eq!(status.ink_levels.pigment_black_percent, 75);
        assert_eq!(status.ink_levels.cyan_percent, 60);
        assert_eq!(status.ink_levels.magenta_percent, 80);
        assert_eq!(status.ink_levels.yellow_percent, 90);
        assert!(!status.ink_levels.low_ink_warning);
        assert_eq!(status.waste_absorber_percent, 24.5);
    }

    #[test]
    fn parses_status_with_thermal_support_code() {
        let xml = r#"<?xml version="1.0" encoding="utf-8" ?>
<cmd xmlns:ivec="http://www.canon.com/ns/cmd/2008/07/common/" xmlns:vcn="http://www.canon.com/ns/cmd/2008/07/canon/">
  <ivec:contents>
    <ivec:response>GetStatus</ivec:response>
    <ivec:param_set servicetype="status">
      <ivec:status>support_error</ivec:status>
      <vcn:statusdetail>error</vcn:statusdetail>
      <vcn:supportcode>5200</vcn:supportcode>
    </ivec:param_set>
  </ivec:contents>
</cmd>"#;

        let status = parse_status_response(xml).unwrap();
        assert_eq!(status.support_code, Some("5200".to_string()));
        assert_eq!(status.lcd_code, Some("P08".to_string()));
        assert_eq!(status.black_head_state, CartridgeState::Overheated);
        assert_eq!(status.color_head_state, CartridgeState::Overheated);
    }

    #[test]
    fn minimal_g2010_xml_marks_unreported_ink_and_waste() {
        let xml = r#"<?xml version="1.0" encoding="utf-8" ?>
<cmd xmlns:ivec="http://www.canon.com/ns/cmd/2008/07/common/" xmlns:vcn="http://www.canon.com/ns/cmd/2008/07/canon/">
  <ivec:contents>
    <ivec:operation>GetStatusResponse</ivec:operation>
    <ivec:response>OK</ivec:response>
    <ivec:param_set servicetype="status">
      <ivec:status>idle</ivec:status>
      <ivec:status_detail/>
      <ivec:current_support_code/>
    </ivec:param_set>
  </ivec:contents>
</cmd>"#;

        let report = parse_status_response_with_meta(xml).unwrap();
        assert!(report.is_status_response());
        assert_eq!(
            report.ivec_operation.as_deref(),
            Some("GetStatusResponse")
        );
        assert_eq!(report.ivec_response.as_deref(), Some("OK"));
        assert_eq!(report.raw_status.as_deref(), Some("idle"));
        assert_eq!(report.raw_status_detail, None);
        assert!(!report.firmware_reported_ink);
        assert!(!report.firmware_reported_waste);
        assert!(!report.firmware_reported_version);
        assert_eq!(report.status.state, OperationalState::Idle);
        assert!(!report.status.ink_levels.low_ink_warning);
    }

    #[test]
    fn start_job_response_is_not_treated_as_status_response() {
        let xml = r#"<?xml version="1.0" encoding="utf-8" ?>
<cmd xmlns:ivec="http://www.canon.com/ns/cmd/2008/07/common/">
  <ivec:contents>
    <ivec:operation>StartJobResponse</ivec:operation>
    <ivec:param_set servicetype="maintenance">
      <ivec:response>OK</ivec:response>
      <ivec:response_detail/>
      <ivec:jobID>00000001</ivec:jobID>
    </ivec:param_set>
  </ivec:contents>
</cmd>"#;

        let report = parse_status_response_with_meta(xml).unwrap();
        assert!(!report.is_status_response());
        assert_eq!(
            report.ivec_operation.as_deref(),
            Some("StartJobResponse")
        );
        assert_eq!(report.ivec_response.as_deref(), Some("OK"));
    }

    #[test]
    fn parses_active_maintenance_cleaning_and_testprint_states() {
        let cleaning_xml = r#"<?xml version="1.0" encoding="utf-8" ?>
<cmd xmlns:ivec="http://www.canon.com/ns/cmd/2008/07/common/" xmlns:vcn="http://www.canon.com/ns/cmd/2008/07/canon/">
  <ivec:contents>
    <ivec:operation>GetStatusResponse</ivec:operation>
    <ivec:response>OK</ivec:response>
    <ivec:param_set servicetype="status">
      <ivec:status>processing</ivec:status>
      <ivec:status_detail/>
    </ivec:param_set>
    <ivec:param_set servicetype="maintenance">
      <ivec:jobinfo jobID="00000002">
        <ivec:maintenance_operation>Cleaning</ivec:maintenance_operation>
        <ivec:type>regular</ivec:type>
        <ivec:jobprogress>processing</ivec:jobprogress>
      </ivec:jobinfo>
    </ivec:param_set>
  </ivec:contents>
</cmd>"#;
        assert_eq!(
            parse_status_response(cleaning_xml).unwrap().state,
            OperationalState::Cleaning
        );

        let test_print_xml = cleaning_xml.replace("Cleaning", "TestPrint");
        assert_eq!(
            parse_status_response(&test_print_xml).unwrap().state,
            OperationalState::Testing
        );

        // Stale completed maintenance jobinfo must NOT mask an active print job.
        let printing_after_clean = cleaning_xml.replace(
            "<ivec:jobprogress>processing</ivec:jobprogress>",
            "<ivec:jobprogress>completed</ivec:jobprogress>",
        );
        assert_eq!(
            parse_status_response(&printing_after_clean).unwrap().state,
            OperationalState::Printing
        );

        // Idle printer stays Idle even if maintenance history is still present.
        let idle_xml = cleaning_xml.replace(
            "<ivec:status>processing</ivec:status>",
            "<ivec:status>idle</ivec:status>",
        );
        assert_eq!(
            parse_status_response(&idle_xml).unwrap().state,
            OperationalState::Idle
        );
    }

    #[test]
    fn parses_maintenance_cleaning_capability_groups() {
        let cap_cmd = build_get_maintenance_capability_cmd();
        assert!(cap_cmd.contains("<ivec:operation>GetCapability</ivec:operation>"));
        assert!(cap_cmd.contains("servicetype=\"maintenance\""));

        let cap_resp = r#"<?xml version="1.0" encoding="utf-8" ?>
<cmd xmlns:ivec="http://www.canon.com/ns/cmd/2008/07/common/">
  <ivec:contents>
    <ivec:operation>GetCapabilityResponse</ivec:operation>
    <ivec:response>OK</ivec:response>
    <ivec:param_set servicetype="maintenance">
      <ivec:Cleaning>
        <ivec:type value="regular">
          <ivec:inkgroup>all,group1,group2</ivec:inkgroup>
        </ivec:type>
        <ivec:type value="deep">
          <ivec:inkgroup>all,group1</ivec:inkgroup>
        </ivec:type>
        <ivec:type value="choke">
          <ivec:inkgroup>all,group1,group2</ivec:inkgroup>
        </ivec:type>
      </ivec:Cleaning>
    </ivec:param_set>
  </ivec:contents>
</cmd>"#;

        let groups = parse_maintenance_cleaning_groups(cap_resp).unwrap();
        assert_eq!(groups.len(), 3);
        assert_eq!(
            groups[0],
            (
                "regular".to_string(),
                vec!["all".into(), "group1".into(), "group2".into()]
            )
        );
        assert_eq!(
            groups[1],
            ("deep".to_string(), vec!["all".into(), "group1".into()])
        );
    }

    #[test]
    fn cleaning_groups_and_nul_padded_status_edge_cases() {
        assert_eq!(CleaningTarget::All.ivec_xml_value(), "all");
        assert_eq!(CleaningTarget::BlackOnly.ivec_xml_value(), "group1");
        assert_eq!(CleaningTarget::ColorOnly.ivec_xml_value(), "group2");

        let start_color = build_maintenance_start_job(42);
        assert!(start_color.contains("<ivec:operation>StartJob</ivec:operation>"));
        assert!(start_color.contains("<ivec:jobID>00000042</ivec:jobID>"));

        let nul_status = "<cmd xmlns:ivec=\"http://www.canon.com/ns/cmd/2008/07/common/\"><ivec:contents><ivec:operation>GetStatusResponse</ivec:operation><ivec:response>OK</ivec:response><ivec:status>idle</ivec:status></ivec:contents></cmd>\0\0\0";
        let meta = parse_status_response_with_meta(nul_status).unwrap();
        assert_eq!(meta.status.state, OperationalState::Idle);
        assert_eq!(meta.raw_status.as_deref(), Some("idle"));

        let not_idle_when_maint_completed = r#"<cmd xmlns:ivec="http://www.canon.com/ns/cmd/2008/07/common/"><ivec:contents><ivec:operation>GetStatusResponse</ivec:operation><ivec:status>processing</ivec:status><ivec:maintenance_jobprogress>completed</ivec:maintenance_jobprogress><ivec:maintenance_operation>cleaning</ivec:maintenance_operation></ivec:contents></cmd>"#;
        assert_eq!(
            parse_status_response(not_idle_when_maint_completed)
                .unwrap()
                .state,
            OperationalState::Printing
        );

        assert!(parse_status_response("not xml at all").is_err());
        assert!(parse_status_response("<cmd><ivec:broken").is_err());
    }

    #[test]
    fn extract_ivec_cleaning_actions_handles_reordered_tags_prefixes_and_oversized_tags() {
        let ws = " ".repeat(1024);
        let long_attr = "a".repeat(600);
        let close_ws = " ".repeat(150);
        let xml = format!(
            "<cmd xmlns:ns0=\"urn:test\" data-pad=\"{long_attr}\">\
             <ns0:type x=\"{long_attr}\">choke</ns0:type{close_ws}>{ws}\
             <ns0:operation y=\"{long_attr}\">Cleaning</ns0:operation{close_ws}></cmd{close_ws}>\
             <cmd><operation>GetStatus</operation><type>regular</type></cmd>\
             <cmd><ivec:operation>Cleaning</ivec:operation><ivec:type>deep</ivec:type></cmd>"
        );
        let parsed = extract_ivec_cleaning_actions(xml.as_bytes());
        assert_eq!(parsed, vec![CleaningType::InkFlush, CleaningType::Deep]);
    }

    #[test]
    fn extract_ivec_cleaning_actions_resists_obfuscation_dtd_pis_charrefs_and_binary_senddata() {
        // 1. Commented StartJob + commented </cmd> + quoted '>' attr + whitespace in </ns:cmd >
        let commented_prefix = b"<!-- <cmd><ivec:operation>StartJob</ivec:operation> -->\
            <ns0:cmd note=\"x>y\"><!-- </cmd> -->\
            <ns0:type>choke</ns0:type><ns0:type>regular</ns0:type>\
            <ns0:operation><!-- comment -->Cleaning</ns0:operation></ns0:cmd   >";
        assert_eq!(
            extract_ivec_cleaning_actions(commented_prefix),
            vec![CleaningType::InkFlush, CleaningType::Regular]
        );

        // 2. [HIGH #2] Processing instructions containing '<cmd>' or '</cmd>' or splitting keywords
        let pi_split = b"<cmd><?pi </cmd> <cmd> ?><ivec:operation>Clean<?pi >?>ing</ivec:operation>\
            <ivec:type>ch<?pi ?>oke</ivec:type></cmd>";
        assert_eq!(
            extract_ivec_cleaning_actions(pi_split),
            vec![CleaningType::InkFlush]
        );

        // 3. [MEDIUM #3a] DOCTYPE internal subset with "<!--" inside entity must not swallow <cmd>
        let doctype_entity = b"<?xml version=\"1.0\"?>\
            <!DOCTYPE cmd [ <!ENTITY x \"<!--\"> <!ELEMENT cmd ANY> ]>\
            <cmd><ivec:operation>Cleaning</ivec:operation><ivec:type>choke</ivec:type></cmd><!-- end -->";
        assert_eq!(
            extract_ivec_cleaning_actions(doctype_entity),
            vec![CleaningType::InkFlush]
        );

        // 4. [MEDIUM #3b] Binary SendData with repeated "<!--" must stay bounded by <ivec:datasize>
        let binary_payload = b"<!--".repeat(25_000);
        let mut send_data_then_choke = format!(
            "<cmd><ivec:operation>SendData</ivec:operation><ivec:datasize>{}</ivec:datasize></cmd>",
            binary_payload.len()
        )
        .into_bytes();
        send_data_then_choke.extend_from_slice(&binary_payload);
        send_data_then_choke.extend_from_slice(
            b"<cmd><ivec:operation>Cleaning</ivec:operation><ivec:type>choke</ivec:type></cmd><!-- end -->",
        );
        assert_eq!(
            extract_ivec_cleaning_actions(&send_data_then_choke),
            vec![CleaningType::InkFlush]
        );

        // 5. [MEDIUM #4] XML numeric character refs & char-ref whitespace padding
        let ref_pad = "&#x20;".repeat(200);
        let char_ref_cmd = format!(
            "<cmd><ivec:operation>{ref_pad}&#67;leaning<!-- c -->&#x20;</ivec:operation>\
             <ivec:type>{ref_pad}&#99;h&#x6f;ke{ref_pad}</ivec:type></cmd>"
        );
        assert_eq!(
            extract_ivec_cleaning_actions(char_ref_cmd.as_bytes()),
            vec![CleaningType::InkFlush]
        );

        // 6. Malformed tag-swallowing prefixes before <cmd> must never mask Cleaning/choke
        for prefix in [b"<?".as_slice(), b"<!", b"</a", b"<a", b"<a\"", b"<"] {
            let mut crafted = Vec::from(
                b"<cmd><ivec:operation>StartJob</ivec:operation></cmd>".as_slice(),
            );
            crafted.extend_from_slice(prefix);
            crafted.extend_from_slice(
                b"<cmd><ivec:operation>Cleaning</ivec:operation><ivec:type>choke</ivec:type></cmd>\">",
            );
            assert_eq!(
                extract_ivec_cleaning_actions(&crafted),
                vec![CleaningType::InkFlush],
                "prefix {:?} bypassed choke detection",
                String::from_utf8_lossy(prefix)
            );
        }

        // 7. [LOW #5] Unclosed delimiters (including '<', '<?', '<!') repeated 25k times stay O(N)
        for rep in [
            b"<!--".as_slice(),
            b"<![CDATA[",
            b"<?",
            b"<!",
            b"<a",
            b"<a\"",
            b"</a",
            b"<",
        ] {
            let mut pathological = rep.repeat(25_000);
            pathological.extend_from_slice(
                b"<cmd><ivec:operation>Cleaning</ivec:operation><ivec:type>choke</ivec:type></cmd>",
            );
            assert_eq!(
                extract_ivec_cleaning_actions(&pathological),
                vec![CleaningType::InkFlush]
            );
        }

        // 8. Cycle 6 [HIGH #2]: DOCTYPE internal-subset PI containing unbalanced quote/bracket
        let doctype_pi_quote = b"<!DOCTYPE cmd [ <?pi \" ] ?> ]>\
            <cmd><ivec:operation>Cleaning</ivec:operation><ivec:type>choke</ivec:type>\
            <ivec:job_description>\"</ivec:job_description></cmd>";
        assert_eq!(
            extract_ivec_cleaning_actions(doctype_pi_quote),
            vec![CleaningType::InkFlush]
        );
        let mut doctype_pi_pathological = b"<!DOCTYPE [ <?pi ".repeat(10_000);
        doctype_pi_pathological.extend_from_slice(
            b"<cmd><ivec:operation>Cleaning</ivec:operation><ivec:type>choke</ivec:type></cmd>",
        );
        assert_eq!(
            extract_ivec_cleaning_actions(&doctype_pi_pathological),
            vec![CleaningType::InkFlush]
        );

        // 9. Cycle 6 [HIGH #1] & [LOW #3]: UTF-8 boundary panic resistance & leading-zero char-refs
        let non_utf8_fallback = b"<cmd><ivec:type>&0123456789abcd\x80</ivec:type>\
            <ivec:operation>Cleaning</ivec:operation><ivec:type>choke</ivec:type></cmd>";
        assert_eq!(
            extract_ivec_cleaning_actions(non_utf8_fallback),
            vec![CleaningType::InkFlush]
        );
        let multibyte_utf8_fallback = "<cmd><ivec:operation>&0123456789abcdé</ivec:operation>\
            <ivec:operation>Cleaning</ivec:operation><ivec:type>choke</ivec:type></cmd>";
        assert_eq!(
            extract_ivec_cleaning_actions(multibyte_utf8_fallback.as_bytes()),
            vec![CleaningType::InkFlush]
        );
        assert_eq!(
            decode_xml_text_or_raw("&#000000000000000099;hoke&e;&0123456789abcdé"),
            "choke&e;&0123456789abcdé"
        );

        // 10. Cycle 7 [HIGH]: SendData payloads ending with partial XML prefixes at boundary
        let boundary_cases: &[(&[u8], &[u8])] = &[
            (b"<", b"!DOCTYPE><cmd><ivec:operation>Cleaning</ivec:operation><ivec:type>choke</ivec:type></cmd>"),
            (b"<", b"<!--><cmd><ivec:operation>Cleaning</ivec:operation><ivec:type>choke</ivec:type></cmd>"),
            (b"<", b"?><cmd><ivec:operation>Cleaning</ivec:operation><ivec:type>choke</ivec:type></cmd>"),
            (b"<!", b"DOCTYPE><cmd><ivec:operation>Cleaning</ivec:operation><ivec:type>choke</ivec:type></cmd>"),
            (b"</", b"cmd><cmd><ivec:operation>Cleaning</ivec:operation><ivec:type>choke</ivec:type></cmd>"),
            (b"<?", b"pi><cmd><ivec:operation>Cleaning</ivec:operation><ivec:type>choke</ivec:type></cmd>"),
            (b"<", b"!DOCTYPE>"),
        ];
        for (payload, suffix) in boundary_cases {
            let mut stream = format!(
                "<cmd><ivec:operation>SendData</ivec:operation><ivec:datasize>{}</ivec:datasize></cmd>",
                payload.len()
            )
            .into_bytes();
            stream.extend_from_slice(payload);
            stream.extend_from_slice(suffix);
            let expected = if suffix.windows(b"choke".len()).any(|w| w == b"choke") {
                vec![CleaningType::InkFlush]
            } else {
                vec![]
            };
            assert_eq!(
                extract_ivec_cleaning_actions(&stream),
                expected,
                "SendData boundary failed for payload={:?}",
                String::from_utf8_lossy(payload)
            );
        }
    }
}

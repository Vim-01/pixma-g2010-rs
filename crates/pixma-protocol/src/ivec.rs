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

/// Parses an IVEC XML status response into a structured `DeviceStatus`.
pub fn parse_status_response(xml: &str) -> Result<DeviceStatus, ProtocolError> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut status = DeviceStatus::default();
    let mut current_tag = String::new();
    let mut status_str = String::new();
    let mut status_detail_str = String::new();
    let mut support_code_opt: Option<String> = None;

    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                current_tag = normalize_tag(&name);

                // Check self-closing or inline attributes for ink and wastetank
                if current_tag == "ink" || current_tag == "color" {
                    parse_ink_attributes(e, &mut status);
                } else if current_tag == "wastetank" || current_tag == "dcounter" {
                    parse_waste_attributes(e, &mut status);
                }
            }
            Ok(Event::Empty(ref e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                let tag = normalize_tag(&name);
                if tag == "ink" || tag == "color" {
                    parse_ink_attributes(e, &mut status);
                } else if tag == "wastetank" || tag == "dcounter" {
                    parse_waste_attributes(e, &mut status);
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
                    "status" => status_str = text.to_lowercase(),
                    "statusdetail" | "status_detail" => status_detail_str = text.to_lowercase(),
                    "supportcode" | "current_support_code" => {
                        let code = text.to_string();
                        if !code.is_empty() && code != "0000" {
                            support_code_opt = Some(code);
                        }
                    }
                    "serialnumber" => status.serial_number = text.to_string(),
                    "firmwareversion" => status.firmware_version = text.to_string(),
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
                        }
                    }
                    "ink_bk" | "bk" => {
                        if let Ok(lvl) = text.parse::<u8>() {
                            status.ink_levels.pigment_black_percent = lvl;
                        }
                    }
                    "ink_c" | "c" => {
                        if let Ok(lvl) = text.parse::<u8>() {
                            status.ink_levels.cyan_percent = lvl;
                        }
                    }
                    "ink_m" | "m" => {
                        if let Ok(lvl) = text.parse::<u8>() {
                            status.ink_levels.magenta_percent = lvl;
                        }
                    }
                    "ink_y" | "y" => {
                        if let Ok(lvl) = text.parse::<u8>() {
                            status.ink_levels.yellow_percent = lvl;
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::End(_)) => {
                current_tag.clear();
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(e) => return Err(ProtocolError::Xml(e.to_string())),
        }
        buf.clear();
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
            "processing" => OperationalState::Printing,
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

    // Set low ink warning flag if any tank is below 15%
    if status.ink_levels.pigment_black_percent <= 15
        || status.ink_levels.cyan_percent <= 15
        || status.ink_levels.magenta_percent <= 15
        || status.ink_levels.yellow_percent <= 15
    {
        status.ink_levels.low_ink_warning = true;
    }

    Ok(status)
}

fn normalize_tag(tag: &str) -> String {
    let raw = tag.split(':').next_back().unwrap_or(tag);
    raw.to_lowercase()
}

fn parse_ink_attributes(e: &quick_xml::events::BytesStart, status: &mut DeviceStatus) {
    let mut color = String::new();
    let mut level = 100u8;

    for attr in e.attributes().flatten() {
        let key = String::from_utf8_lossy(attr.key.as_ref()).to_lowercase();
        let val = String::from_utf8_lossy(&attr.value).to_string();

        if key == "id" || key == "color" {
            color = val.to_uppercase();
        } else if key == "level" || key == "percent" {
            if let Ok(v) = val.parse::<u8>() {
                level = v;
            }
        }
    }

    match color.as_str() {
        "BK" | "BLACK" | "PGBK" => status.ink_levels.pigment_black_percent = level,
        "C" | "CYAN" => status.ink_levels.cyan_percent = level,
        "M" | "MAGENTA" => status.ink_levels.magenta_percent = level,
        "Y" | "YELLOW" => status.ink_levels.yellow_percent = level,
        _ => {}
    }
}

fn parse_waste_attributes(e: &quick_xml::events::BytesStart, status: &mut DeviceStatus) {
    for attr in e.attributes().flatten() {
        let key = String::from_utf8_lossy(attr.key.as_ref()).to_lowercase();
        let val = String::from_utf8_lossy(&attr.value).to_string();

        if key == "percent" || key == "level" {
            if let Ok(v) = val.parse::<f32>() {
                status.waste_absorber_percent = v;
            }
        }
    }
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
        let clean = build_cleaning_cmd(1, CleaningType::InkFlush, CleaningTarget::All);
        assert!(clean.contains("<ivec:operation>Cleaning</ivec:operation>"));
        assert!(clean.contains("<ivec:type>choke</ivec:type>"));
        assert!(clean.contains("<ivec:inkgroup>all</ivec:inkgroup>"));

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
}

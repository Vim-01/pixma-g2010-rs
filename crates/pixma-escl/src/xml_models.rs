//! eSCL (AirScan) XML schema serialization, deserialization, and capability models.
//!
//! Conforms to eSCL Version 2.63 and PWG Semantic Model for scanner capabilities,
//! scanner telemetry status, and incoming scan job configuration requests.

use quick_xml::events::Event;
use quick_xml::reader::Reader;

/// Telemetry info for an active or completed eSCL scan job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EsclJobInfo {
    pub uuid: String,
    pub age_seconds: u32,
    pub state: String,
    pub reason: String,
}

/// Parsed parameters from an incoming eSCL `ScanSettings` XML payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EsclScanRequest {
    pub x_resolution: u32,
    pub y_resolution: u32,
    pub color_mode: String,
    pub document_format: String,
    pub x_offset: Option<u32>,
    pub y_offset: Option<u32>,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

impl Default for EsclScanRequest {
    fn default() -> Self {
        Self {
            x_resolution: 300,
            y_resolution: 300,
            color_mode: "RGB24".to_string(),
            document_format: "application/pdf".to_string(),
            x_offset: None,
            y_offset: None,
            width: None,
            height: None,
        }
    }
}

/// Builds the static eSCL 2.63 `ScannerCapabilities` XML document.
pub fn build_scanner_capabilities_xml() -> String {
    r#"<?xml version="1.0" encoding="UTF-8"?>
<scan:ScannerCapabilities xmlns:scan="http://schemas.hp.com/imaging/escl/2011/05/03" xmlns:pwg="http://www.pwg.org/schemas/2010/12/sm">
  <pwg:Version>2.63</pwg:Version>
  <pwg:MakeAndModel>Canon PIXMA G2010 series</pwg:MakeAndModel>
  <pwg:Manufacturer>Canon</pwg:Manufacturer>
  <scan:Platen>
    <scan:PlatenInputCaps>
      <scan:MinWidth>300</scan:MinWidth>
      <scan:MinHeight>300</scan:MinHeight>
      <scan:MaxWidth>2550</scan:MaxWidth>
      <scan:MaxHeight>3508</scan:MaxHeight>
      <scan:MaxOpticalXResolution>600</scan:MaxOpticalXResolution>
      <scan:MaxOpticalYResolution>600</scan:MaxOpticalYResolution>
      <scan:RisSupported>false</scan:RisSupported>
      <scan:SettingProfiles>
        <scan:SettingProfile>
          <scan:ColorModes>
            <scan:ColorMode>RGB24</scan:ColorMode>
            <scan:ColorMode>Grayscale8</scan:ColorMode>
            <scan:ColorMode>BlackAndWhite1</scan:ColorMode>
          </scan:ColorModes>
          <scan:DocumentFormats>
            <scan:DocumentFormat>application/pdf</scan:DocumentFormat>
            <scan:DocumentFormat>image/png</scan:DocumentFormat>
            <scan:DocumentFormat>image/jpeg</scan:DocumentFormat>
            <scan:DocumentFormatExt>application/pdf</scan:DocumentFormatExt>
            <scan:DocumentFormatExt>image/png</scan:DocumentFormatExt>
            <scan:DocumentFormatExt>image/jpeg</scan:DocumentFormatExt>
          </scan:DocumentFormats>
          <scan:SupportedResolutions>
            <scan:DiscreteResolutions>
              <scan:DiscreteResolution>
                <scan:XResolution>75</scan:XResolution>
                <scan:YResolution>75</scan:YResolution>
              </scan:DiscreteResolution>
              <scan:DiscreteResolution>
                <scan:XResolution>100</scan:XResolution>
                <scan:YResolution>100</scan:YResolution>
              </scan:DiscreteResolution>
              <scan:DiscreteResolution>
                <scan:XResolution>150</scan:XResolution>
                <scan:YResolution>150</scan:YResolution>
              </scan:DiscreteResolution>
              <scan:DiscreteResolution>
                <scan:XResolution>200</scan:XResolution>
                <scan:YResolution>200</scan:YResolution>
              </scan:DiscreteResolution>
              <scan:DiscreteResolution>
                <scan:XResolution>300</scan:XResolution>
                <scan:YResolution>300</scan:YResolution>
              </scan:DiscreteResolution>
              <scan:DiscreteResolution>
                <scan:XResolution>400</scan:XResolution>
                <scan:YResolution>400</scan:YResolution>
              </scan:DiscreteResolution>
              <scan:DiscreteResolution>
                <scan:XResolution>600</scan:XResolution>
                <scan:YResolution>600</scan:YResolution>
              </scan:DiscreteResolution>
              <scan:DiscreteResolution>
                <scan:XResolution>1200</scan:XResolution>
                <scan:YResolution>1200</scan:YResolution>
              </scan:DiscreteResolution>
            </scan:DiscreteResolutions>
          </scan:SupportedResolutions>
        </scan:SettingProfile>
      </scan:SettingProfiles>
    </scan:PlatenInputCaps>
  </scan:Platen>
</scan:ScannerCapabilities>"#
        .to_string()
}

/// Builds an eSCL 2.63 `ScannerStatus` XML document reflecting current state and jobs.
pub fn build_scanner_status_xml(state: &str, jobs: &[EsclJobInfo]) -> String {
    let mut jobs_xml = String::new();
    for job in jobs {
        jobs_xml.push_str(&format!(
            r#"    <scan:JobInfo>
      <pwg:JobUri>/eSCL/ScanJobs/{uuid}</pwg:JobUri>
      <pwg:JobUuid>{uuid}</pwg:JobUuid>
      <scan:Age>{age}</scan:Age>
      <pwg:JobState>{job_state}</pwg:JobState>
      <pwg:JobStateReasons>
        <pwg:JobStateReason>{reason}</pwg:JobStateReason>
      </pwg:JobStateReasons>
    </scan:JobInfo>
"#,
            uuid = job.uuid,
            age = job.age_seconds,
            job_state = job.state,
            reason = job.reason
        ));
    }

    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<scan:ScannerStatus xmlns:scan="http://schemas.hp.com/imaging/escl/2011/05/03" xmlns:pwg="http://www.pwg.org/schemas/2010/12/sm">
  <pwg:Version>2.63</pwg:Version>
  <pwg:State>{state}</pwg:State>
  <scan:Jobs>
{jobs_xml}  </scan:Jobs>
</scan:ScannerStatus>"#,
        state = state,
        jobs_xml = jobs_xml
    )
}

/// Parses an eSCL `ScanSettings` XML payload into structured `EsclScanRequest`.
pub fn parse_scan_settings_xml(xml: &str) -> Result<EsclScanRequest, String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut request = EsclScanRequest::default();
    let mut current_tag = String::new();

    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                let local = match name.rsplit_once(':') {
                    Some((_, l)) => l.to_string(),
                    None => name,
                };
                current_tag = local;
            }
            Ok(Event::Text(ref e)) => {
                let text = match e.unescape() {
                    Ok(t) => t.trim().to_string(),
                    Err(_) => String::from_utf8_lossy(e.as_ref()).trim().to_string(),
                };
                if text.is_empty() {
                    continue;
                }

                match current_tag.as_str() {
                    "XResolution" => {
                        if let Ok(val) = text.parse::<u32>() {
                            request.x_resolution = val;
                        }
                    }
                    "YResolution" => {
                        if let Ok(val) = text.parse::<u32>() {
                            request.y_resolution = val;
                        }
                    }
                    "ColorMode" => {
                        request.color_mode = text;
                    }
                    "DocumentFormat" => {
                        request.document_format = text;
                    }
                    "DocumentFormatExt" => {
                        request.document_format = text;
                    }
                    "XOffset" => {
                        if let Ok(val) = text.parse::<u32>() {
                            request.x_offset = Some(val);
                        }
                    }
                    "YOffset" => {
                        if let Ok(val) = text.parse::<u32>() {
                            request.y_offset = Some(val);
                        }
                    }
                    "Width" => {
                        if let Ok(val) = text.parse::<u32>() {
                            request.width = Some(val);
                        }
                    }
                    "Height" => {
                        if let Ok(val) = text.parse::<u32>() {
                            request.height = Some(val);
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::End(_)) => {
                current_tag.clear();
            }
            Ok(Event::Eof) => break,
            Err(e) => {
                return Err(format!(
                    "XML parse error at position {}: {e}",
                    reader.buffer_position()
                ));
            }
            _ => {}
        }
        buf.clear();
    }

    Ok(request)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_capabilities_xml_contains_g2010_metadata() {
        let caps = build_scanner_capabilities_xml();
        assert!(caps.contains("Canon PIXMA G2010 series"));
        assert!(caps.contains("<scan:MaxWidth>2550</scan:MaxWidth>"));
        assert!(caps.contains("<scan:MaxHeight>3508</scan:MaxHeight>"));
        assert!(caps.contains("<scan:ColorMode>RGB24</scan:ColorMode>"));
        assert!(caps.contains("<scan:DocumentFormat>application/pdf</scan:DocumentFormat>"));
        assert!(caps.contains("<scan:DocumentFormat>image/png</scan:DocumentFormat>"));
        assert!(caps.contains("<scan:DocumentFormat>image/jpeg</scan:DocumentFormat>"));
    }

    #[test]
    fn test_status_xml_generation() {
        let jobs = vec![EsclJobInfo {
            uuid: "12345678-1234-1234-1234-123456789abc".to_string(),
            age_seconds: 5,
            state: "Completed".to_string(),
            reason: "JobCompletedSuccessfully".to_string(),
        }];
        let status = build_scanner_status_xml("Idle", &jobs);
        assert!(status.contains("<pwg:State>Idle</pwg:State>"));
        assert!(status.contains("12345678-1234-1234-1234-123456789abc"));
        assert!(status.contains("<pwg:JobState>Completed</pwg:JobState>"));
    }

    #[test]
    fn test_parse_scan_settings_xml() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<scan:ScanSettings xmlns:scan="http://schemas.hp.com/imaging/escl/2011/05/03" xmlns:pwg="http://www.pwg.org/schemas/2010/12/sm">
  <pwg:Version>2.63</pwg:Version>
  <scan:ColorMode>Grayscale8</scan:ColorMode>
  <scan:XResolution>600</scan:XResolution>
  <scan:YResolution>600</scan:YResolution>
  <scan:DocumentFormat>image/png</scan:DocumentFormat>
  <pwg:ScanRegions>
    <pwg:ScanRegion>
      <pwg:XOffset>100</pwg:XOffset>
      <pwg:YOffset>200</pwg:YOffset>
      <pwg:Width>1200</pwg:Width>
      <pwg:Height>1600</pwg:Height>
    </pwg:ScanRegion>
  </pwg:ScanRegions>
</scan:ScanSettings>"#;

        let req = parse_scan_settings_xml(xml).expect("Scan settings parsing failed");
        assert_eq!(req.x_resolution, 600);
        assert_eq!(req.y_resolution, 600);
        assert_eq!(req.color_mode, "Grayscale8");
        assert_eq!(req.document_format, "image/png");
        assert_eq!(req.x_offset, Some(100));
        assert_eq!(req.y_offset, Some(200));
        assert_eq!(req.width, Some(1200));
        assert_eq!(req.height, Some(1600));
    }
}

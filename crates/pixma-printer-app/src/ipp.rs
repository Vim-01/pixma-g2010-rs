//! Minimal pure-Rust IPP/2.0 binary protocol request parser and response builder.
//!
//! Conforms to RFC 8010 (Encoding and Transport) and RFC 8011 (Model and Semantics).

use pixma_core::models::{DeviceStatus, OperationalState};
use thiserror::Error;

// ---------------------------------------------------------------------------
// IPP Protocol Constants
// ---------------------------------------------------------------------------

// Operation IDs
pub const OP_PRINT_JOB: u16 = 0x0002;
pub const OP_VALIDATE_JOB: u16 = 0x0004;
pub const OP_CREATE_JOB: u16 = 0x0005;
pub const OP_SEND_DOCUMENT: u16 = 0x0006;
pub const OP_CANCEL_JOB: u16 = 0x0008;
pub const OP_GET_JOB_ATTRIBUTES: u16 = 0x0009;
pub const OP_GET_JOBS: u16 = 0x000A;
pub const OP_GET_PRINTER_ATTRIBUTES: u16 = 0x000B;

// Status Codes
pub const STATUS_SUCCESSFUL_OK: u16 = 0x0000;
pub const STATUS_CLIENT_ERROR_BAD_REQUEST: u16 = 0x0400;
pub const STATUS_CLIENT_ERROR_NOT_FOUND: u16 = 0x0405;
pub const STATUS_SERVER_ERROR_INTERNAL: u16 = 0x0500;
pub const STATUS_SERVER_ERROR_NOT_SUPPORTED: u16 = 0x0501;

// Delimiter Tags
pub const TAG_OPERATION_ATTRIBUTES: u8 = 0x01;
pub const TAG_JOB_ATTRIBUTES: u8 = 0x02;
pub const TAG_END_OF_ATTRIBUTES: u8 = 0x03;
pub const TAG_PRINTER_ATTRIBUTES: u8 = 0x04;
pub const TAG_UNSUPPORTED_ATTRIBUTES: u8 = 0x05;

// Value Syntax Tags
pub const TAG_INTEGER: u8 = 0x21;
pub const TAG_BOOLEAN: u8 = 0x22;
pub const TAG_ENUM: u8 = 0x23;
pub const TAG_OCTET_STRING: u8 = 0x30;
pub const TAG_TEXT_WITHOUT_LANGUAGE: u8 = 0x41;
pub const TAG_NAME_WITHOUT_LANGUAGE: u8 = 0x42;
pub const TAG_KEYWORD: u8 = 0x44;
pub const TAG_URI: u8 = 0x45;
pub const TAG_URI_SCHEME: u8 = 0x46;
pub const TAG_CHARSET: u8 = 0x47;
pub const TAG_NATURAL_LANGUAGE: u8 = 0x48;
pub const TAG_MIME_MEDIA_TYPE: u8 = 0x49;

// Printer State Enums (RFC 8011 section 5.4.11)
pub const PRINTER_STATE_IDLE: i32 = 3;
pub const PRINTER_STATE_PROCESSING: i32 = 4;
pub const PRINTER_STATE_STOPPED: i32 = 5;

// Job State Enums (RFC 8011 section 5.3.7)
pub const JOB_STATE_PROCESSING: i32 = 5;
pub const JOB_STATE_COMPLETED: i32 = 9;

#[derive(Debug, Error)]
pub enum IppError {
    #[error("IPP buffer underflow: expected at least {expected} bytes, got {actual}")]
    BufferUnderflow { expected: usize, actual: usize },
    #[error("Invalid IPP attribute syntax: {0}")]
    InvalidSyntax(String),
}

/// A single IPP attribute value tagged with its syntax type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IppValue {
    pub tag: u8,
    pub data: Vec<u8>,
}

impl IppValue {
    pub fn text(tag: u8, s: &str) -> Self {
        Self {
            tag,
            data: s.as_bytes().to_vec(),
        }
    }

    pub fn integer(val: i32) -> Self {
        Self {
            tag: TAG_INTEGER,
            data: val.to_be_bytes().to_vec(),
        }
    }

    pub fn enumeration(val: i32) -> Self {
        Self {
            tag: TAG_ENUM,
            data: val.to_be_bytes().to_vec(),
        }
    }

    pub fn boolean(val: bool) -> Self {
        Self {
            tag: TAG_BOOLEAN,
            data: vec![if val { 1 } else { 0 }],
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        std::str::from_utf8(&self.data).ok()
    }

    pub fn as_i32(&self) -> Option<i32> {
        if self.data.len() == 4 {
            let arr: [u8; 4] = self.data[..4].try_into().ok()?;
            Some(i32::from_be_bytes(arr))
        } else {
            None
        }
    }
}

/// An IPP attribute containing a name and one or more values (`1setOf`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IppAttribute {
    pub name: String,
    pub values: Vec<IppValue>,
}

impl IppAttribute {
    pub fn new_single(name: impl Into<String>, value: IppValue) -> Self {
        Self {
            name: name.into(),
            values: vec![value],
        }
    }

    pub fn new_set(name: impl Into<String>, values: Vec<IppValue>) -> Self {
        Self {
            name: name.into(),
            values,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn first_value(&self) -> Option<&IppValue> {
        self.values.first()
    }
}

/// A parsed IPP binary request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IppRequest {
    pub version_major: u8,
    pub version_minor: u8,
    pub operation_id: u16,
    pub request_id: u32,
    pub operation_attributes: Vec<IppAttribute>,
    pub job_attributes: Vec<IppAttribute>,
    pub printer_attributes: Vec<IppAttribute>,
    pub data: Vec<u8>,
}

impl IppRequest {
    /// Parses an IPP binary request buffer.
    pub fn parse(buf: &[u8]) -> Result<Self, IppError> {
        if buf.len() < 8 {
            return Err(IppError::BufferUnderflow {
                expected: 8,
                actual: buf.len(),
            });
        }

        let version_major = buf[0];
        let version_minor = buf[1];
        let operation_id = u16::from_be_bytes([buf[2], buf[3]]);
        let request_id = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);

        let mut offset = 8;
        let mut current_group: Option<u8> = None;

        let mut operation_attributes: Vec<IppAttribute> = Vec::new();
        let mut job_attributes: Vec<IppAttribute> = Vec::new();
        let mut printer_attributes: Vec<IppAttribute> = Vec::new();

        while offset < buf.len() {
            let tag = buf[offset];
            offset += 1;

            if tag == TAG_END_OF_ATTRIBUTES {
                // All remaining bytes are document data payload
                break;
            }

            if tag <= 0x05 {
                // Group delimiter tag
                current_group = Some(tag);
                continue;
            }

            // Attribute syntax tag >= 0x10
            if offset + 2 > buf.len() {
                return Err(IppError::BufferUnderflow {
                    expected: offset + 2,
                    actual: buf.len(),
                });
            }
            let name_len = u16::from_be_bytes([buf[offset], buf[offset + 1]]) as usize;
            offset += 2;

            if offset + name_len > buf.len() {
                return Err(IppError::BufferUnderflow {
                    expected: offset + name_len,
                    actual: buf.len(),
                });
            }
            let name_bytes = &buf[offset..offset + name_len];
            let name = String::from_utf8_lossy(name_bytes).to_string();
            offset += name_len;

            if offset + 2 > buf.len() {
                return Err(IppError::BufferUnderflow {
                    expected: offset + 2,
                    actual: buf.len(),
                });
            }
            let val_len = u16::from_be_bytes([buf[offset], buf[offset + 1]]) as usize;
            offset += 2;

            if offset + val_len > buf.len() {
                return Err(IppError::BufferUnderflow {
                    expected: offset + val_len,
                    actual: buf.len(),
                });
            }
            let val_data = buf[offset..offset + val_len].to_vec();
            offset += val_len;

            let value = IppValue {
                tag,
                data: val_data,
            };

            // If name_len == 0, this is an additional value for the preceding attribute
            let target_list = match current_group {
                Some(TAG_OPERATION_ATTRIBUTES) => &mut operation_attributes,
                Some(TAG_JOB_ATTRIBUTES) => &mut job_attributes,
                Some(TAG_PRINTER_ATTRIBUTES) => &mut printer_attributes,
                _ => &mut operation_attributes,
            };

            if name_len == 0 && !target_list.is_empty() {
                let last = target_list.last_mut().unwrap();
                last.values.push(value);
            } else {
                target_list.push(IppAttribute::new_single(name, value));
            }
        }

        let data = if offset <= buf.len() {
            buf[offset..].to_vec()
        } else {
            Vec::new()
        };

        Ok(Self {
            version_major,
            version_minor,
            operation_id,
            request_id,
            operation_attributes,
            job_attributes,
            printer_attributes,
            data,
        })
    }

    /// Looks up an operation attribute by name.
    pub fn get_operation_attribute(&self, name: &str) -> Option<&IppAttribute> {
        self.operation_attributes
            .iter()
            .find(|a| a.name().eq_ignore_ascii_case(name))
    }

    /// Returns requested document format string (e.g. `image/pwg-raster`).
    pub fn document_format(&self) -> Option<&str> {
        self.get_operation_attribute("document-format")
            .and_then(|a| a.first_value())
            .and_then(|v| v.as_str())
    }
}

/// An IPP binary response builder.
#[derive(Debug, Clone)]
pub struct IppResponse {
    pub version_major: u8,
    pub version_minor: u8,
    pub status_code: u16,
    pub request_id: u32,
    pub operation_attributes: Vec<IppAttribute>,
    pub job_attributes: Vec<IppAttribute>,
    pub printer_attributes: Vec<IppAttribute>,
}

impl IppResponse {
    /// Creates a new IPP response echoing the request's version and request-id.
    pub fn new(version_major: u8, version_minor: u8, status_code: u16, request_id: u32) -> Self {
        let mut resp = Self {
            version_major,
            version_minor,
            status_code,
            request_id,
            operation_attributes: Vec::new(),
            job_attributes: Vec::new(),
            printer_attributes: Vec::new(),
        };

        // Standard IPP operational attributes: charset and natural-language
        resp.operation_attributes.push(IppAttribute::new_single(
            "attributes-charset",
            IppValue::text(TAG_CHARSET, "utf-8"),
        ));
        resp.operation_attributes.push(IppAttribute::new_single(
            "attributes-natural-language",
            IppValue::text(TAG_NATURAL_LANGUAGE, "en-us"),
        ));

        resp
    }

    /// Appends an attribute to the printer-attributes group.
    pub fn add_printer_attribute(&mut self, attr: IppAttribute) {
        self.printer_attributes.push(attr);
    }

    /// Appends an attribute to the job-attributes group.
    pub fn add_job_attribute(&mut self, attr: IppAttribute) {
        self.job_attributes.push(attr);
    }

    /// Builds a `Get-Printer-Attributes` response populated with Canon PIXMA G2010 capabilities
    /// and live device status.
    pub fn build_printer_attributes(
        version_major: u8,
        version_minor: u8,
        request_id: u32,
        status: &DeviceStatus,
    ) -> Self {
        let mut resp = Self::new(
            version_major,
            version_minor,
            STATUS_SUCCESSFUL_OK,
            request_id,
        );

        let (state_enum, state_reason) = match status.state {
            OperationalState::Idle => (PRINTER_STATE_IDLE, "none"),
            OperationalState::Printing
            | OperationalState::Scanning
            | OperationalState::Cleaning
            | OperationalState::Testing
            | OperationalState::Waiting => (PRINTER_STATE_PROCESSING, "none"),
            OperationalState::PaperOut => (PRINTER_STATE_STOPPED, "media-empty"),
            OperationalState::PaperJam => (PRINTER_STATE_STOPPED, "media-jam"),
            OperationalState::CoverOpen => (PRINTER_STATE_STOPPED, "cover-open"),
            OperationalState::ServiceError | OperationalState::OperatorCall => {
                (PRINTER_STATE_STOPPED, "other")
            }
            OperationalState::Offline => (PRINTER_STATE_STOPPED, "shutdown"),
        };

        // 1. printer-name
        resp.add_printer_attribute(IppAttribute::new_single(
            "printer-name",
            IppValue::text(TAG_NAME_WITHOUT_LANGUAGE, "Canon PIXMA G2010"),
        ));

        // 2. printer-state
        resp.add_printer_attribute(IppAttribute::new_single(
            "printer-state",
            IppValue::enumeration(state_enum),
        ));

        // 3. printer-state-reasons
        resp.add_printer_attribute(IppAttribute::new_single(
            "printer-state-reasons",
            IppValue::text(TAG_KEYWORD, state_reason),
        ));

        // 4. document-format-supported: application/pdf, application/vnd.cups-pdf, image/pwg-raster, image/png, image/jpeg, application/octet-stream
        resp.add_printer_attribute(IppAttribute::new_set(
            "document-format-supported",
            vec![
                IppValue::text(TAG_MIME_MEDIA_TYPE, "application/pdf"),
                IppValue::text(TAG_MIME_MEDIA_TYPE, "application/vnd.cups-pdf"),
                IppValue::text(TAG_MIME_MEDIA_TYPE, "image/pwg-raster"),
                IppValue::text(TAG_MIME_MEDIA_TYPE, "image/png"),
                IppValue::text(TAG_MIME_MEDIA_TYPE, "image/jpeg"),
                IppValue::text(TAG_MIME_MEDIA_TYPE, "application/octet-stream"),
            ],
        ));
        resp.add_printer_attribute(IppAttribute::new_single(
            "document-format-default",
            IppValue::text(TAG_MIME_MEDIA_TYPE, "application/pdf"),
        ));
        resp.add_printer_attribute(IppAttribute::new_single(
            "printer-uri-supported",
            IppValue::text(TAG_URI, "ipp://127.0.0.1:8631/ipp/print"),
        ));
        resp.add_printer_attribute(IppAttribute::new_single(
            "uri-security-supported",
            IppValue::text(TAG_KEYWORD, "none"),
        ));
        resp.add_printer_attribute(IppAttribute::new_single(
            "uri-authentication-supported",
            IppValue::text(TAG_KEYWORD, "none"),
        ));
        resp.add_printer_attribute(IppAttribute::new_single(
            "charset-configured",
            IppValue::text(TAG_CHARSET, "utf-8"),
        ));
        resp.add_printer_attribute(IppAttribute::new_single(
            "charset-supported",
            IppValue::text(TAG_CHARSET, "utf-8"),
        ));
        resp.add_printer_attribute(IppAttribute::new_single(
            "natural-language-configured",
            IppValue::text(TAG_NATURAL_LANGUAGE, "en-us"),
        ));
        resp.add_printer_attribute(IppAttribute::new_single(
            "generated-natural-language-supported",
            IppValue::text(TAG_NATURAL_LANGUAGE, "en-us"),
        ));
        resp.add_printer_attribute(IppAttribute::new_single(
            "pdl-override-supported",
            IppValue::text(TAG_KEYWORD, "attempted"),
        ));

        // 5. marker-names
        resp.add_printer_attribute(IppAttribute::new_set(
            "marker-names",
            vec![
                IppValue::text(TAG_NAME_WITHOUT_LANGUAGE, "Pigment Black (GI-790)"),
                IppValue::text(TAG_NAME_WITHOUT_LANGUAGE, "Cyan (GI-790)"),
                IppValue::text(TAG_NAME_WITHOUT_LANGUAGE, "Magenta (GI-790)"),
                IppValue::text(TAG_NAME_WITHOUT_LANGUAGE, "Yellow (GI-790)"),
            ],
        ));

        // 6. marker-colors
        resp.add_printer_attribute(IppAttribute::new_set(
            "marker-colors",
            vec![
                IppValue::text(TAG_NAME_WITHOUT_LANGUAGE, "#000000"),
                IppValue::text(TAG_NAME_WITHOUT_LANGUAGE, "#00FFFF"),
                IppValue::text(TAG_NAME_WITHOUT_LANGUAGE, "#FF00FF"),
                IppValue::text(TAG_NAME_WITHOUT_LANGUAGE, "#FFFF00"),
            ],
        ));

        // 7. marker-types
        resp.add_printer_attribute(IppAttribute::new_set(
            "marker-types",
            vec![
                IppValue::text(TAG_KEYWORD, "ink-cartridge"),
                IppValue::text(TAG_KEYWORD, "ink-cartridge"),
                IppValue::text(TAG_KEYWORD, "ink-cartridge"),
                IppValue::text(TAG_KEYWORD, "ink-cartridge"),
            ],
        ));

        // 8. marker-levels
        resp.add_printer_attribute(IppAttribute::new_set(
            "marker-levels",
            vec![
                IppValue::integer(status.ink_levels.pigment_black_percent as i32),
                IppValue::integer(status.ink_levels.cyan_percent as i32),
                IppValue::integer(status.ink_levels.magenta_percent as i32),
                IppValue::integer(status.ink_levels.yellow_percent as i32),
            ],
        ));

        // Supplemental standard IPP Everywhere attributes
        resp.add_printer_attribute(IppAttribute::new_set(
            "ipp-versions-supported",
            vec![
                IppValue::text(TAG_KEYWORD, "1.1"),
                IppValue::text(TAG_KEYWORD, "2.0"),
            ],
        ));
        resp.add_printer_attribute(IppAttribute::new_set(
            "operations-supported",
            vec![
                IppValue::enumeration(OP_PRINT_JOB as i32),
                IppValue::enumeration(OP_VALIDATE_JOB as i32),
                IppValue::enumeration(OP_CREATE_JOB as i32),
                IppValue::enumeration(OP_SEND_DOCUMENT as i32),
                IppValue::enumeration(OP_CANCEL_JOB as i32),
                IppValue::enumeration(OP_GET_JOB_ATTRIBUTES as i32),
                IppValue::enumeration(OP_GET_JOBS as i32),
                IppValue::enumeration(OP_GET_PRINTER_ATTRIBUTES as i32),
            ],
        ));
        resp.add_printer_attribute(IppAttribute::new_single(
            "printer-is-accepting-jobs",
            IppValue::boolean(true),
        ));
        resp.add_printer_attribute(IppAttribute::new_single(
            "queued-job-count",
            IppValue::integer(0),
        ));
        resp.add_printer_attribute(IppAttribute::new_single(
            "color-supported",
            IppValue::boolean(true),
        ));

        resp
    }

    /// Builds a `Create-Job` response with job-id, job-uri, and pending/processing job-state.
    pub fn build_create_job_response(
        version_major: u8,
        version_minor: u8,
        request_id: u32,
        job_id: i32,
    ) -> Self {
        let mut resp = Self::new(
            version_major,
            version_minor,
            STATUS_SUCCESSFUL_OK,
            request_id,
        );

        resp.add_job_attribute(IppAttribute::new_single(
            "job-id",
            IppValue::integer(job_id),
        ));
        resp.add_job_attribute(IppAttribute::new_single(
            "job-uri",
            IppValue::text(
                TAG_URI,
                &format!("ipp://127.0.0.1:8631/ipp/print/jobs/{job_id}"),
            ),
        ));
        resp.add_job_attribute(IppAttribute::new_single(
            "job-state",
            IppValue::enumeration(JOB_STATE_PROCESSING),
        ));
        resp.add_job_attribute(IppAttribute::new_single(
            "job-state-reasons",
            IppValue::text(TAG_KEYWORD, "job-incoming"),
        ));

        resp
    }

    /// Builds a `Print-Job` / `Send-Document` response with job-id, job-uri, and completed job-state.
    pub fn build_print_job_response(
        version_major: u8,
        version_minor: u8,
        request_id: u32,
        job_id: i32,
    ) -> Self {
        let mut resp = Self::new(
            version_major,
            version_minor,
            STATUS_SUCCESSFUL_OK,
            request_id,
        );

        resp.add_job_attribute(IppAttribute::new_single(
            "job-id",
            IppValue::integer(job_id),
        ));
        resp.add_job_attribute(IppAttribute::new_single(
            "job-uri",
            IppValue::text(
                TAG_URI,
                &format!("ipp://127.0.0.1:8631/ipp/print/jobs/{job_id}"),
            ),
        ));
        resp.add_job_attribute(IppAttribute::new_single(
            "job-state",
            IppValue::enumeration(JOB_STATE_COMPLETED),
        ));
        resp.add_job_attribute(IppAttribute::new_single(
            "job-state-reasons",
            IppValue::text(TAG_KEYWORD, "job-completed-successfully"),
        ));

        resp
    }

    /// Serializes the response into binary IPP wire format.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(512);

        // Header: version (2), status_code (2), request_id (4)
        out.push(self.version_major);
        out.push(self.version_minor);
        out.extend_from_slice(&self.status_code.to_be_bytes());
        out.extend_from_slice(&self.request_id.to_be_bytes());

        // Helper closure to serialize an attribute group
        let mut write_group = |tag: u8, attrs: &[IppAttribute]| {
            if !attrs.is_empty() {
                out.push(tag);
                for attr in attrs {
                    for (i, val) in attr.values.iter().enumerate() {
                        out.push(val.tag);
                        if i == 0 {
                            let name_bytes = attr.name.as_bytes();
                            out.extend_from_slice(&(name_bytes.len() as u16).to_be_bytes());
                            out.extend_from_slice(name_bytes);
                        } else {
                            // 1setOf: name_len = 0
                            out.extend_from_slice(&0u16.to_be_bytes());
                        }
                        out.extend_from_slice(&(val.data.len() as u16).to_be_bytes());
                        out.extend_from_slice(&val.data);
                    }
                }
            }
        };

        write_group(TAG_OPERATION_ATTRIBUTES, &self.operation_attributes);
        write_group(TAG_JOB_ATTRIBUTES, &self.job_attributes);
        write_group(TAG_PRINTER_ATTRIBUTES, &self.printer_attributes);

        // End of attributes delimiter
        out.push(TAG_END_OF_ATTRIBUTES);

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pixma_core::models::InkLevels;

    #[test]
    fn test_ipp_request_parse_and_get_printer_attributes() {
        let mut req_bytes = Vec::new();
        // Version 2.0
        req_bytes.push(2);
        req_bytes.push(0);
        // Operation: Get-Printer-Attributes (0x000B)
        req_bytes.extend_from_slice(&OP_GET_PRINTER_ATTRIBUTES.to_be_bytes());
        // Request ID 1
        req_bytes.extend_from_slice(&1u32.to_be_bytes());

        // Group 1: Operation attributes
        req_bytes.push(TAG_OPERATION_ATTRIBUTES);
        // attributes-charset = utf-8
        req_bytes.push(TAG_CHARSET);
        let name = "attributes-charset";
        req_bytes.extend_from_slice(&(name.len() as u16).to_be_bytes());
        req_bytes.extend_from_slice(name.as_bytes());
        let val = "utf-8";
        req_bytes.extend_from_slice(&(val.len() as u16).to_be_bytes());
        req_bytes.extend_from_slice(val.as_bytes());

        // End of attributes
        req_bytes.push(TAG_END_OF_ATTRIBUTES);

        let parsed = IppRequest::parse(&req_bytes).expect("Failed to parse IPP request");
        assert_eq!(parsed.version_major, 2);
        assert_eq!(parsed.version_minor, 0);
        assert_eq!(parsed.operation_id, OP_GET_PRINTER_ATTRIBUTES);
        assert_eq!(parsed.request_id, 1);
        assert_eq!(
            parsed
                .get_operation_attribute("attributes-charset")
                .and_then(|a| a.first_value())
                .and_then(|v| v.as_str()),
            Some("utf-8")
        );

        // Build response
        let status = DeviceStatus {
            ink_levels: InkLevels {
                pigment_black_percent: 100,
                cyan_percent: 80,
                magenta_percent: 75,
                yellow_percent: 90,
                low_ink_warning: false,
            },
            ..Default::default()
        };

        let resp = IppResponse::build_printer_attributes(2, 0, 1, &status);
        let resp_bytes = resp.to_bytes();

        // Reparse response to verify validity
        assert_eq!(resp_bytes[0], 2);
        assert_eq!(resp_bytes[1], 0);
        let status_code = u16::from_be_bytes([resp_bytes[2], resp_bytes[3]]);
        assert_eq!(status_code, STATUS_SUCCESSFUL_OK);

        let parsed_resp = IppRequest::parse(&resp_bytes).expect("Failed to parse response bytes");
        assert_eq!(parsed_resp.request_id, 1);

        let p_name = parsed_resp
            .printer_attributes
            .iter()
            .find(|a| a.name() == "printer-name")
            .unwrap();
        assert_eq!(
            p_name.first_value().unwrap().as_str(),
            Some("Canon PIXMA G2010")
        );

        let p_levels = parsed_resp
            .printer_attributes
            .iter()
            .find(|a| a.name() == "marker-levels")
            .unwrap();
        assert_eq!(p_levels.values.len(), 4);
        assert_eq!(p_levels.values[0].as_i32(), Some(100));
        assert_eq!(p_levels.values[1].as_i32(), Some(80));
        assert_eq!(p_levels.values[2].as_i32(), Some(75));
        assert_eq!(p_levels.values[3].as_i32(), Some(90));
    }

    #[test]
    fn test_ipp_print_job_data_payload_extraction() {
        let mut req_bytes = Vec::new();
        req_bytes.push(2);
        req_bytes.push(0);
        req_bytes.extend_from_slice(&OP_PRINT_JOB.to_be_bytes());
        req_bytes.extend_from_slice(&42u32.to_be_bytes());

        // Group 1
        req_bytes.push(TAG_OPERATION_ATTRIBUTES);
        req_bytes.push(TAG_MIME_MEDIA_TYPE);
        let name = "document-format";
        req_bytes.extend_from_slice(&(name.len() as u16).to_be_bytes());
        req_bytes.extend_from_slice(name.as_bytes());
        let val = "image/pwg-raster";
        req_bytes.extend_from_slice(&(val.len() as u16).to_be_bytes());
        req_bytes.extend_from_slice(val.as_bytes());

        // End tag
        req_bytes.push(TAG_END_OF_ATTRIBUTES);

        // Document payload
        let doc_payload = b"RaS2_DUMMY_PWG_RASTER_PAYLOAD";
        req_bytes.extend_from_slice(doc_payload);

        let parsed = IppRequest::parse(&req_bytes).expect("parse print job failed");
        assert_eq!(parsed.operation_id, OP_PRINT_JOB);
        assert_eq!(parsed.request_id, 42);
        assert_eq!(parsed.document_format(), Some("image/pwg-raster"));
        assert_eq!(parsed.data, doc_payload);

        let resp = IppResponse::build_print_job_response(2, 0, 42, 1);
        let resp_bytes = resp.to_bytes();
        let parsed_resp = IppRequest::parse(&resp_bytes).expect("parse resp failed");
        let j_id = parsed_resp
            .job_attributes
            .iter()
            .find(|a| a.name() == "job-id")
            .unwrap();
        assert_eq!(j_id.first_value().unwrap().as_i32(), Some(1));
    }
}

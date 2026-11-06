//! Driverless eSCL (AirScan) HTTP/XML microserver for Canon PIXMA G2010.
//!
//! Implements Apple AirScan / eSCL Version 2.63 and PWG Scan Service protocol specifications,
//! allowing native zero-driver scanning from macOS, iOS, Windows, and Linux Mopria/eSCL clients.

pub mod server;
pub mod xml_models;

pub use server::{create_escl_router, generate_job_uuid, EsclServerState, StoredScanJob};
pub use xml_models::{
    build_scanner_capabilities_xml, build_scanner_status_xml, parse_scan_settings_xml, EsclJobInfo,
    EsclScanRequest,
};

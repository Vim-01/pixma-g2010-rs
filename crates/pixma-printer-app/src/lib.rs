//! Driverless IPP Everywhere / CUPS 3.x Printer Application for Canon PIXMA G2010.

pub mod ipp;
pub mod router;

pub use ipp::{
    IppAttribute, IppError, IppRequest, IppResponse, IppValue, OP_GET_JOBS,
    OP_GET_PRINTER_ATTRIBUTES, OP_PRINT_JOB, OP_VALIDATE_JOB,
};
pub use router::{build_router, AppState, MaintenanceApiRequest};

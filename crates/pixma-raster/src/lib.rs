//! Rasterization, layout imposition, dithering, and job stream generation
//! for the Canon PIXMA G2010 (and compatible MegaTank models).
//!
//! # Architecture
//! - [`cups_reader`]: CUPS / PWG Raster v2 (`"RaS2"`) parser, image file decoder (PNG, JPEG, TIFF, WebP),
//!   and diagnostic nozzle check / test print page generator.
//! - [`layout`]: Canon MP Driver page imposition engine implementing `Normal`, `FitToPage`, `Scaled` (20..=400%),
//!   `NUp` (2, 4, 6, 9, 16), `Poster` ($R \times C$ tiling with alignment ticks), `Booklet` (saddle-stitch signatures),
//!   and `BorderlessExtension` overscan bleeding.
//! - [`halftone`]: Multi-level (2-bit: 0=Off, 1=Small 2pL, 2=Medium, 3=Large 5pL) and 1-bit binary halftoning
//!   using an isotropic $64 \times 64$ void-and-cluster blue-noise matrix via `rayon` or serpentine
//!   Sierra-3 and Jarvis-Judice-Ninke error diffusion.
//! - [`pipeline`]: High-level [`PrintRasterPipeline`] coordinating page layout, ICC/tone transforms,
//!   hardware safety bounds ([`pixma_core::safety::ValidatedRasterLine`]), MegaTank GI-790 ink separation rules,
//!   and compiling wire-ready IVEC + PWG Raster and IVEC + BJRaster3 data streams.

pub mod cups_reader;
pub mod errors;
pub mod halftone;
pub mod layout;
pub mod pipeline;

pub use cups_reader::{
    generate_test_print_page, parse_document_from_memory, parse_image_from_memory,
    parse_pwg_raster, RasterPageInput,
};
pub use errors::RasterError;
pub use halftone::{
    get_blue_noise_matrix_64x64, BlueNoiseDitherer, DitherAlgorithm, DotDepth,
    ErrorDiffusionDitherer,
};
pub use layout::{apply_borderless_extension, apply_page_layout, resize_bilinear};
pub use pipeline::PrintRasterPipeline;

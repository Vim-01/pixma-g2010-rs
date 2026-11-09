# Changelog

## [v0.2.1] - 2026-11-09

### CUPS IPP 2.0 Printing & USB Lock Concurrency Hotfix

#### Fixed
- **CUPS IPP 2.0 Multi-Part Job Pipeline (`Create-Job` + `Send-Document`)**:
  - Implemented IPP operations `0x0005` (`OP_CREATE_JOB`) and `0x0006` (`OP_SEND_DOCUMENT`) in `pixma-printer-app` router alongside `0x0002` (`OP_PRINT_JOB`).
  - Added required IPP response attributes (`job-uri`, `printer-uri-supported`, `uri-security-supported`, `uri-authentication-supported`, and `application/vnd.cups-pdf` MIME format) to satisfy CUPS 2.4's `/usr/lib/cups/backend/ipp`.
- **Axum 2 MB Request Body Limit Disabled**:
  - Attached `.layer(DefaultBodyLimit::disable())` to `pixma-printer-app` router, fixing silent `413 Payload Too Large` failures when printing PDFs larger than 2 MB (e.g. multi-page color or vector PDFs from OnlyOffice / Okular).
- **USB Lock Contention & Daemon Deadlock (`/tmp/pixma-g2010.lock`)**:
  - Converted `pixma-escl-server` to on-demand USB mode: the scanner server now connects to USB on-demand inside scan job handlers and drops the USB interface and `flock` immediately upon completion instead of holding it open permanently from daemon startup.
  - Made `Get-Printer-Attributes` (`0x000b`) in `pixma-printer-app` return printer status and capabilities lock-free without acquiring hardware USB, preventing status polling stalls during job creation.
- **End-to-End CUPS Queue Verification**:
  - Verified automatic processing and output of pending CUPS jobs (Jobs 5, 6, and 7: 1.14 MB, 3.01 MB, and 0.63 MB) without user intervention.

## [v0.2.0] - 2026-11-09

### Hardware-Verified Production Release (Canon PIXMA G2010 `04a9:183a`)

#### Highlights
- **Native BJRaster3 (`BJR520`) Print Pipeline**:
  - Complete IVEC XML + `BJL` + `BJRaster3` (`ESC ( q`, `ESC ( c`, `ESC ( t`, `ESC ( L`, PackBits `ESC ( A`, raster advance `ESC ( e`, and mandatory `ESC ( F = 0x01` Print-Data-End-of-Page commit) verified on physical Canon PIXMA G2010 hardware (Firmware `1.020`).
  - **Two-Level Smart Print (`PGBK` vs `CMY`)**:
    - **Per-Pixel Smart Separation**: Neutral black and gray pixels (`|R-G| <= 4 && |G-B| <= 4 && |R-B| <= 4`) on Plain Paper use 100% waterproof Pigment Black (`PGBK`) and 0% dye `CMY`.
    - **Per-Page Smart Mode Switching**: Neutral/text-only pages in multi-page jobs automatically switch hardware color mode to `ESC ( c = 0x31` (`ESC ( L = "K"`, 1 bit-plane), accelerating B&W pages and preventing color nozzle cycling.
  - **Isotropic 64×64 Blue Noise Halftoning & Normalized ITU-R BT.709 `SGray`**:
    - Eliminates periodic `8×8` Bayer harmonic beats against CIS scanner LED strobes and preserves full 25%–100% tonal gradation.
  - **Multi-Page PDF, PWG Raster, and Image Ingestion**:
    - Direct support for `%PDF` documents (via `pdftoppm`/`gs`), `RaS2` PWG/CUPS Raster streams, and PNG/JPEG/TIFF/WebP across `pixma-cli`, `pixma-printer-app` (IPP Everywhere), and `rastertopixma-g2010` (CUPS filter).
- **Exact ISO A4 Flatbed Scanning & CIS Strobe Chroma Demodulator**:
  - **Exact ISO A4 Geometry**: Trims the 6 mm US Letter right-hand platen overhang (`638 -> 620` px at 75 DPI), outputting exact ISO A4 (`1240×1754` at 150 DPI, `2480×3508` at 300 DPI) with `< 0.1 mm` dimensional accuracy (`100.00 mm` target measured `99.91 × 99.91 mm`).
  - **CIS Halftone Strobe Chroma Demodulator**: Compensates `(0, 1/3, 2/3)` sequential R/G/B LED flash phase shifts, applies a 3-tap vertical chrominance median filter, and uses 2D summed-area tables (`17×17` window) to eliminate rainbow/pink moiré on scanned B&W halftones (`ΔRGB = 0.0`) while preserving 100% of genuine printed colors.
- **Desktop & Network Integration**:
  - On-demand USB connection mode in `pixma-printer-app` (`127.0.0.1:8631/ipp/print`), allowing seamless printing from OnlyOffice, LibreOffice, and CUPS while keeping USB endpoints free when idle.

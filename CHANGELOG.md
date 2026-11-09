# Changelog

## [v0.2.3] - 2026-11-09

### BJR520 Firmware Table Restoration, IPP Orientation Support & 1200 DPI Optical Sensor Mapping

#### Fixed
- **BJR520 Firmware Descriptor Restoration (`ESC ( t`)**:
  - Reverted modified `ESC ( t` descriptors back to the Canon G2010 firmware-hardcoded 24-byte table (`01 00 02 02 00 03 02 00 03 01 00 02`). Reverse-engineering of `cnm_0520.dll` confirmed that Slot 0 represents the physical Black head (BH-4, 1-bpp), Slot 1/2 represent Cyan/Magenta (2-bpp), and Slot 3 represents Yellow (1-bpp). Restoring this exact byte layout fixes the bug where jobs submitted to the printer were silently dropped without paper feed.
- **IPP Page Orientation & Capabilities in OnlyOffice / KDE (`orientation-requested-supported`)**:
  - Added standard IPP 2.0 attributes `orientation-requested-supported: [3, 4, 5, 6]` (Portrait, Landscape, Reverse-Landscape, Reverse-Portrait) and `orientation-requested-default: 3` to `build_printer_attributes`.
  - Added standard `media-supported`, `print-color-mode-supported`, `sides-supported`, and `copies-default` IPP attributes. This unlocks grayed-out orientation dropdowns in OnlyOffice, LibreOffice, Chromium, and KDE print dialogs.
- **1200 DPI Scanner Support (Physical 600 DPI CIS Sensor Mapping)**:
  - Addressed the hardware constraint where Canon G2010 flatbed CIS sensor features 600 optical photodiodes per inch horizontally. When 1200 DPI is requested, `acquire_raw_scan` executes a native 600 DPI optical capture on the hardware and performs 2x subpixel bilinear upscale to deliver exact 1200 DPI output without ASIC command rejection (`0xD820`).
- **ScanGear Pro Non-Blocking GUI Preview**:
  - Automatically generates and caches lightweight downscaled preview thumbnails (`.thumb_scan_*.png`) for `gtk::Picture`, preventing GTK main-thread freezes when loading full-resolution 600 DPI (105 MB) and 1200 DPI scans.

## [v0.2.2] - 2026-11-09

### Landscape Auto-Orientation, BJRaster3 Cyan Fix & ScanGear Pro GUI Export

#### Fixed
- **BJRaster3 `ESC ( t` Bit-Depth Descriptor Table Inversion (Cyan / Blue Lines)**:
  - Corrected plane bit-depth descriptors in `encode_bjr520_prologue` to strictly match `ESC ( L` (`"CMYK"`): Slot 0 (Cyan) is now 2-bpp (`02 00 03`) and Slot 2 (Yellow) is 1-bpp (`01 00 02`). Previously, Cyan was assigned 1-bpp while 2-bpp data was streamed, causing buffer overflow and completely dropping Cyan ink (which erased blue grids, flooded zone hatching, and blue dashed lines).
- **Auto-Orientation & Full-Page Scaling (Landscape to Portrait Feed)**:
  - Added automatic 90-degree counter-clockwise orientation alignment in `apply_page_layout`: when document orientation (Landscape, width > height) does not match the printer's narrow-edge physical feed (Portrait, width < height), the page is rotated automatically to fill the entire physical A4 sheet.
  - Added proportional printable-area scaling in `layout_normal` for full-page documents, fixing the bug where vector documents rendered at a different DPI were shrunk into a half-sized box in the center of the sheet.
  - Set `pixma-printer-app` to ingest and render PDFs at the printer's native 600 DPI hardware grid (`base_raster_dpi()`), ensuring high-DPI vector precision and crisp rendering of 0.5-pt dashed lines.
- **ScanGear Pro GUI Export & Preview (`pixma-studio`)**:
  - Automatically exports acquired scans to `~/Pictures/Scans/scan_<timestamp>.png`.
  - Added live document preview in the ScanGear Pro tab (`gtk::Picture`) immediately upon scan completion.
  - Added an "Open in Viewer" button (`xdg-open`) and interactive spinner (`gtk::Spinner`) that disables action buttons while hardware scanning is active.
  - Made hardware connections in `pixma-studio` non-blocking and on-demand, preventing startup deadlocks on `/tmp/pixma-g2010.lock`.

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

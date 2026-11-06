# pixma-g2010-rs

[![License: AGPL v3](https://img.shields.io/badge/License-AGPL_v3-blue.svg)](https://www.gnu.org/licenses/agpl-3.0)
[![Platform: Arch Linux](https://img.shields.io/badge/Platform-Arch_Linux-1793d1.svg)](https://archlinux.org)
[![Rust: 1.78+](https://img.shields.io/badge/Rust-1.78%2B-orange.svg)](https://www.rust-lang.org)

**[Русская документация (README.ru.md)](./README.ru.md)**

Full-featured, memory-safe Linux driver, scanner backend, maintenance utility, and GTK4/Libadwaita control studio for the **Canon PIXMA G2010** (and compatible G1010 / G2000 / G2020 / G3000 / G3010 / G4000 / G4010 MegaTank series), written in Rust.

---

## Why This Exists

Canon's official Linux driver packages (`cnijfilter2` and `scangearmp2`) treat the G2010 as an afterthought:
- **Missing USB IDs & PPDs**: USB Product ID `04a9:183a` (`G2010 series`) and `04a9:183c` (`G1010 series`) are completely omitted from `80-canon_mfp2.rules`, `canon_mfp2.conf`, and `/usr/share/cups/model/`.
- **Locked Print Engine**: Even when forced to run with a G3010 PPD, `rastertocanonij` only passes 4 flags (`--papersize`, `--media`, `--grayscale`, `--duplex`), locking print resolution to a single 600 DPI preset and ignoring draft mode (`300x300`), fine photo passes, borderless extension sliders, paper gap control, ink drying times, and manual C/M/Y color balance.
- **65%+ of Hardware Maintenance Missing**: `cmdtocanonij3` hardcodes only three commands (`PrintSelfTestPage`, `Clean all`, `com.canon.autoalignment`). Crucial CISS operations supported by the printer's firmware — independent Black (`group1`) vs Color (`group2`) cleaning, Deep Cleaning (`deep`), **System Ink Flush (`choke`)** for purging air from CISS ink tubes, Roller Cleaning (`roller`), Bottom Plate Cleaning (`platen`), and Manual Head Alignment (`manual_registration` — the only alignment mode G2010/G3010 actually support) — are unreachable on Linux.
- **Discarded Telemetry**: `cnijlgmon3` queries `<ivec:operation>GetStatus</ivec:operation>` over USB, receives full XML telemetry (`<vcn:supportcode>`, `<vcn:statusdetail>`, `<vcn:ink>`, waste ink absorber counters), and throws almost all of it away.
- **Crippled Scanner**: `scangearmp2` caps flatbed resolution at 600 DPI and lacks CIS line-stagger compensation, auto-multi-crop, deskew, FFT moire descreening, dust removal, gutter shadow correction, and spline tone curves.

`pixma-g2010-rs` replaces the entire proprietary C stack with a clean-room, modular Rust workspace.

---

## Hardware Safety Guarantees

The Canon PIXMA G2010 uses thermal inkjet (FINE) printheads (**BH-4** Black with 320 nozzles, **CH-41** Color with 1,152 nozzles) where **liquid ink is the sole coolant** for thin-film tantalum-aluminum heaters pulsing past $300^\circ\text{C}$, and its gravity-fed CISS tanks have **no physical liquid level sensors**.

To protect the printheads, carriage mechanics, and mainboard:
1. **Compile-Time & Typestate Coordinate Bounds (`pixma-core::safety`)**:
   - `ValidatedRasterLine` and `PrintJobTypestate<State>` enforce physical carriage travel limits (`2550` dots at 300 DPI, `5100` dots at 600 DPI, `10200` dots at 1200 DPI) before any byte can be packed into PWG Raster or BJRaster3 frames.
2. **Thermal Duty-Cycle Throttling & Fast Abort**:
   - `DutyCycleMonitor` tracks consecutive high-density (>85% coverage) scanlines and injects $200\ \text{ms}$ inter-swath cooling pauses to prevent resistor thermal runaway.
   - Any hardware support code indicating head or thermal faults (`1401`, `1403`, `1470`, `1471`, `1472`, `5200` / `P08`) triggers an immediate `CancelJob` and paper eject.
3. **MegaTank Ink Separation (`pixma-color`)**:
   - **Plain Paper**: Uses Under Color Removal (UCR) and Gray Component Replacement (GCR) routing pure black text (`#000000`) to 100% Pigment Black (`GI-790 PGBK`).
   - **Glossy / Luster / Platinum Photo Paper**: Pigment Black particles cannot penetrate microporous silica coatings and will flake off. The separator forces `PGBK = 0%` on all glossy media and synthesizes deep blacks from composite dye `C + M + Y`.
4. **Waste Ink Absorber (`5B00` / `P07`) & Ink Flush Protection**:
   - A single System Ink Flush (`choke`) drains **~45 ml of ink** and advances the internal waste pad counter (`D-Counter`) by **~20%**.
   - `MaintenanceSupervisor` enforces a persistent **24-hour cooldown** on Ink Flush, blocks execution if `D-Counter >= 80%`, and requires explicit confirmation (`YES-DRAIN-INK` in CLI or a timed confirmation dialog in the GUI).
5. **No Raw EEPROM Factory Writes**:
   - All operations use documented `<ivec:operation>` XML envelopes and standard raster streams. Raw `@BJL` factory EEPROM write vectors (which can trigger permanent `Error 006` mainboard lockouts) are excluded from the driver.

---

## Workspace Architecture

| Crate | Description |
| :--- | :--- |
| [`crates/pixma-core`](./crates/pixma-core) | Hardware models, bounded coordinates (`Dots`, `Microns`, `ValidatedRasterLine`), typestate job machine, `MaintenanceSupervisor`, and LCD/Support code database (`E02`..`P10` / `1000`..`5B00`). |
| [`crates/pixma-protocol`](./crates/pixma-protocol) | BJL framing, IVEC XML builders/parsers, `CNCL` (`XOR 0x39`) PPD capability codec, PWG Raster (`RaS2` + PackBits RLE), BJRaster3 (`ESC (`), and binary MP2 scanner commands (`0xF320`, `0xD820`, `0xD920`, `0xD420`, `0xEF20`). |
| [`crates/pixma-transport`](./crates/pixma-transport) | Pure-Rust async USB transport via `nusb` (Interface 0 Scanner + Interface 1 Printer), automatic `usblp` kernel detach, `flock` fallback, and a full `MockG2010Device` hardware simulator. |
| [`crates/pixma-color`](./crates/pixma-color) | ICC/sRGB color management (`qcms`), LUT-accelerated C/M/Y balance, brightness/contrast/gamma adjustments, and media-aware PGBK vs composite CMY separation. |
| [`crates/pixma-raster`](./crates/pixma-raster) | CUPS/PWG raster reader, page layout engine (Fit-to-Page, Scaled, N-up, Poster, Booklet, Borderless Extension 0–3), Blue-Noise and Sierra/Jarvis-Judice-Ninke error diffusion dithering. |
| [`crates/pixma-imageproc`](./crates/pixma-imageproc) | CIS sensor RGB line-stagger alignment (`LineStaggerBuffer`), Otsu auto-multi-crop, deskew, 2D-FFT moire descreening, unsharp mask, morphological dust/scratch removal, book gutter shadow correction, histograms, cubic spline tone curves, and multi-page PDF/TIFF/PNG/JPEG exporters. |
| [`crates/pixma-daemon`](./crates/pixma-daemon) | `pixmad` background USB multiplexer (`/run/pixma/pixmad.sock`) arbitrating concurrent print jobs, scan sessions, and status polling without USB bus collisions. |
| [`crates/pixma-cups`](./crates/pixma-cups) | Native CUPS 2.x filters (`rastertopixma-g2010`, `cmdtopixma-g2010`), USB backend (`cnusb-g2010`), and full PPD (`assets/ppd/pixma-g2010.ppd`). |
| [`crates/pixma-printer-app`](./crates/pixma-printer-app) | Driverless IPP Everywhere HTTP/IPP server (`127.0.0.1:8631`) for CUPS 3.x compatibility. |
| [`crates/pixma-sane`](./crates/pixma-sane) | SANE Standard v1.04 C ABI shared library (`libsane-pixma_g2010.so.1`) with panic-safe FFI boundaries for Simple Scan, Skanpage, GIMP, and `scanimage`. |
| [`crates/pixma-escl`](./crates/pixma-escl) | Driverless eSCL (AirScan) HTTP/XML server (`127.0.0.1:8080`) for network and driverless scanning. |
| [`crates/pixma-cli`](./crates/pixma-cli) | `pixma-cli` headless tool for status inspection, cleaning, nozzle checks, alignment, scanning, and direct printing. |
| [`crates/pixma-gui`](./crates/pixma-gui) | **PIXMA G2010 Studio** (`pixma-studio`) — native Wayland/X11 desktop application built with Relm4 (GTK4 + Libadwaita). |

---

## Installation on Arch Linux

### Option 1: Interactive Modular Installer (Select Components)
```bash
./scripts/install.sh
```
Presents a checklist allowing you to build and install only the modules you want (`core`, `cups`, `sane`, `escl`, `printer-app`, `cli`, `gui`).

### Option 2: Arch Linux Split Packages (`makepkg`)
```bash
cd packaging/arch
makepkg -si
```
Builds modular split packages:
- `pixma-g2010-core` (`pixmad` + udev rules)
- `pixma-g2010-cups` (CUPS filters, backend, PPD)
- `pixma-g2010-sane` (`libsane-pixma_g2010.so.1`)
- `pixma-g2010-escl` (AirScan server)
- `pixma-g2010-printer-app` (IPP Everywhere server)
- `pixma-g2010-cli` (`pixma-cli`)
- `pixma-g2010-gui` (`pixma-studio`)
- `pixma-g2010-meta` (full desktop suite)

---

## CLI Usage (`pixma-cli`)

```bash
# Live hardware status, ink levels, waste absorber %, and error decoding
pixma-cli status

# Print nozzle diagnostic grid
pixma-cli nozzle-check

# Standard cleaning (all, black-only, or color-only)
pixma-cli clean --kind regular --target black

# Deep cleaning
pixma-cli clean --kind deep --target all

# System Ink Flush (requires explicit confirmation token and respects 24h cooldown)
pixma-cli clean --kind flush --target all --confirm YES-DRAIN-INK

# Clean paper feed rollers or bottom plate
pixma-cli roller-clean --target roller
pixma-cli roller-clean --target platen

# Print head alignment (print 2-sheet manual pattern or apply column values A..N)
pixma-cli align --action print-manual
pixma-cli align --action apply --values "0,1,-1,0,0,2,0,-1,0,0,1,0,0,0"

# Scan at 600 DPI with auto-multi-crop, deskew, and moire descreening to PDF
pixma-cli scan --dpi 600 --deskew --descreen --auto-crop --output scan.pdf

# Print an image or internal calibration test page with full media and color controls
pixma-cli print --test-page --media plain --paper a4 --quality 3
```

---

## License

Licensed under the **GNU Affero General Public License v3.0 or later (`AGPL-3.0-or-later`)**. See [`LICENSE`](./LICENSE) for the full text.

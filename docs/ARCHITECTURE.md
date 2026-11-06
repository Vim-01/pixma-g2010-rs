# `pixma-g2010-rs`: Architecture Specification
**License:** AGPL-3.0-or-later  
**Target Platform:** Arch Linux (Linux Kernel ≥ 6.6, systemd, Wayland / X11)  
**Target Hardware:** Canon PIXMA G2010 Multi-Function MegaTank (USB ID `04a9:183a`)

---

## Overview

The **Canon PIXMA G2010** is a continuous ink tank (MegaTank) multifunction printer/scanner. On Linux, the vendor-provided driver stack (`cnijfilter2` and `scangearmp2`) exhibits several technical limitations:
1. Omitted USB device IDs (`04a9:183a`, `04a9:183c`), missing PPDs, and hardcoded 4-option filter invocations (`--papersize`, `--media`, `--grayscale`, `--duplex`) that lock output to a single 600 DPI preset.
2. Uncorrected CIS sensor vertical channel displacement at 600 and 1200 DPI, producing chromatic fringing along horizontal edges.
3. Missing maintenance operations in `cmdtocanonij3` (which exposes only 3 of the 9+ IVEC XML maintenance commands supported by the printer's firmware) and discarded XML status telemetry in `cnijlgmon3`.
4. USB interface contention when scanning and querying printer status concurrently.

`pixma-g2010-rs` implements the complete driver stack in Rust across 13 modular workspace crates: dual-path printing (CUPS 2.x PPD/filter pipeline + CUPS 3.x IPP Everywhere Printer Application), dual-path scanning (SANE C ABI backend + driverless eSCL AirScan HTTP server), a media-aware color and halftoning pipeline, and a GTK4/Libadwaita desktop utility.

---

## 1. USB & Transport Layer

```
                        ┌──────────────────────────────────────────────┐
                        │             Hardware Device                  │
                        │      Canon PIXMA G2010 (USB 04a9:183a)       │
                        └──────────────────────┬───────────────────────┘
                                               │
             ┌─────────────────────────────────┴─────────────────────────────────┐
             │                                                                   │
┌────────────▼────────────────────────┐             ┌────────────────────────────▼──────────────┐
│ Interface 0: Vendor Specific (0xFF)  │             │ Interface 1: USB Printer Class (0x07)     │
│ SubClass: 0xCC, Protocol: 0x00       │             │ SubClass: 0x01, Protocol: 0x02            │
│ Endpoints: EP 0x01/0x02 OUT (Bulk)   │             │ Endpoints: EP 0x03 OUT, EP 0x84 IN (Bulk) │
│            EP 0x82/0x81 IN (Bulk)    │             │ Control: EP 0x00 (GET_DEVICE_ID 0xA1/0x00)│
│ Used for: Scanner, EEPROM, ASIC Regs │             │ Used for: Print stream & IEEE 1284 Status │
└─────────────────────┬────────────────┘             └───────────────────────────┬───────────────┘
                      │                                                          │
                      └────────────────────────┬─────────────────────────────────┘
                                               │
                     ┌─────────────────────────▼────────────────────────┐
                     │          Linux usbfs Kernel Boundary             │
                     │  /dev/bus/usb/BBB/DDD (USBDEVFS_DISCONNECT)      │
                     └─────────────────────────┬────────────────────────┘
                                               │
               ┌───────────────────────────────┴───────────────────────────────┐
               │              pixma-transport Engine (nusb)                    │
               │   • Zero C dependencies (libc/ioctl/netlink hotplug)          │
               │   • Async-first (Tokio / epoll / zero-copy memory slices)     │
               │   • Safe ownership model prevents use-after-free on unplug    │
               └───────────────────────────────┬───────────────────────────────┘
                                               │
        ┌──────────────────────────────────────┴──────────────────────────────────────┐
        │                                                                             │
┌───────▼───────────────────────────────┐                     ┌───────────────────────▼──────────────┐
│  Daemon Mode: pixmad Multiplexer      │                     │ Standalone Direct Mode (Failover)    │
│  • IPC: /run/pixma/pixmad.sock        │                     │ • Cooperative flock on               │
│  • Token-based Session Arbitrator     │                     │   /run/lock/pixma-g2010.lock         │
│  • High Priority: Print / Scan Pipes  │                     │ • Standalone CLI / emergency print   │
│  • Low Priority: Background Status    │                     │ • Graceful timeout & interface claim │
└───────────────────────────────────────┘                     └──────────────────────────────────────┘
```

### 1.1 `nusb` vs `rusb` Technical Evaluation

| Criterion | `nusb` (Recommended) | `rusb` (`libusb-1.0` FFI) | Architecture Impact |
| :--- | :--- | :--- | :--- |
| **C Dependencies** | **Zero**. Pure Rust directly interfacing with Linux `usbfs` (`/dev/bus/usb/*`) and netlink udev. | Requires C dynamic library `libusb-1.0.so` and development headers. | `nusb` builds with musl/glibc cleanly without cross-compilation toolchain headaches. |
| **Async Architecture** | **Native async/await** with epoll and kernel completion queues. | Callback-based event loop requiring manual OS thread spawning or `libusb_handle_events_timeout_completed`. | `nusb` fits natively into Tokio runtimes without worker-thread starvation or event pump threads. |
| **Zero-Copy & Memory Safety** | Safe slice passing into ioctl buffers with strict lifetime tracking. Zero C-FFI pointer gymnastics. | Passes raw `*mut u8` pointers to C code. Unsafe transmutes and potential pointer invalidation on device disconnect. | `nusb` completely eliminates use-after-free and double-free vulnerabilities during physical cable disconnection. |
| **Interface Detaching** | Direct, deterministic `USBDEVFS_DISCONNECT` and `USBDEVFS_RELEASEINTERFACE` ioctls. | Wraps `libusb_detach_kernel_driver` which can fail silently on varied Linux kernels. | Smooth automatic kernel detachment of `usblp`. |
| **Platform Target** | Linux-first (with ongoing macOS/Windows support). Perfect for Arch Linux. | Cross-platform (Linux, Windows, macOS, BSD). | Arch Linux is our primary target; Linux usbfs is native. |

> **Architectural Decision:** Adopt **`nusb`** as the primary asynchronous USB transport engine. Encapsulate all USB calls behind a clean `pixma_core::transport::UsbTransport` trait so a `rusb` fallback can be activated via Cargo feature flag if non-Linux compilation is ever desired.

### 1.2 Handling Dual USB Interfaces on PIXMA G2010

The Canon PIXMA G2010 features two distinct USB interfaces under VID `0x04A9`, PID `0x183A`:
1. **Interface 0 (Vendor-Specific `0xFF:0xCC:0x00`)**:
   - Proprietary Canon scanner protocol (generation `pixma_mp150`).
   - Endpoints: Bulk OUT (e.g. `0x01` / `0x02`), Bulk IN (e.g. `0x82` / `0x81`).
   - Also accepts vendor control transfers (`0x40` vendor requests) for hardware calibration, EEPROM dumps, and ink absorption counter query/reset.
2. **Interface 1 (USB Printer Class `0x07:0x01:0x02`)**:
   - Bidirectional printer interface (1284.4 compatible).
   - Endpoints: Bulk OUT (raster data stream) and Bulk IN (status return).
   - Control Endpoint 0 responds to standard USB Printer Class requests:
     - `GET_DEVICE_ID` (`0xA1`, `0x00`): Returns standard IEEE 1284 string:
       `MFG:Canon;MDL:G2010 series;CMD:BJL,BJRaster3;CLS:PRINTER;DES:Canon G2010 series;CID:CA_G2010;`
     - `GET_PORT_STATUS` (`0xA1`, `0x01`): Returns raw paper out, fault, and selection flags.
     - `SOFT_RESET` (`0x21`, `0x02`).

#### Kernel Driver Conflict Resolution (`usblp`)
When plugged in, the kernel module `usblp` automatically binds to Interface 1, locking the file descriptor.
* **Udev Rule (`/usr/lib/udev/rules.d/70-pixma-g2010.rules`):**
  ```udev
  # Canon PIXMA G2010 series USB permissions and usblp unbind
  ACTION=="add", SUBSYSTEM=="usb", ATTR{idVendor}=="04a9", ATTR{idProduct}=="183a", \
    TAG+="uaccess", GROUP="lp", MODE="0664"
  ```
* **Rust Driver Logic:** When opening the device via `nusb`, issue `claim_interface` with auto-detach enabled:
  ```rust
  let interface = device.claim_interface(1)?; // Automatically detaches usblp via usbfs ioctl
  ```

### 1.3 Concurrency Architecture & Multiplexing Broker (`pixmad`)

Even though the scanner (Interface 0) and printer (Interface 1) reside on separate USB interfaces, the G2010 hardware utilizes a **shared internal MCU/ASIC and motor chassis**.
* Initiating a scan carriage movement while print heads are wiping or ejecting paper causes physical contention, power supply spikes, or USB command timeouts (`0x06` NAK / STALL).
* Status polling queries issued over Interface 0 or 1 while high-speed raster bands are streaming to the print head cause buffer underruns, producing horizontal white banding lines.

#### The `pixmad` Daemon Architecture
`pixmad` runs as a lightweight, low-footprint background daemon (RAM < 8MB) managed by systemd:
1. **Local IPC:** Exposes a high-performance Unix Domain Socket at `/run/pixma/pixmad.sock`. Protocol is binary CBOR or typed Varlink over `tokio::net::UnixListener`.
2. **Resource Locking Model:**
   * **`JobToken::Printing(job_id)`**: Takes exclusive ownership of Interface 1 and holds a shared read-lock on mechanical state. Pauses periodic status polling.
   * **`JobToken::Scanning(job_id)`**: Takes exclusive ownership of Interface 0 and acquires an exclusive lock preventing mechanical carriage motion from printing.
   * **`JobToken::Maintenance(op)`**: Exclusive lock over both interfaces during head cleaning, bottom plate cleaning, or ink flushing.
   * **`Status Monitor (Telemetry)`**: When the device is `Idle`, polls ink tank status and error flags every 5 seconds. During active printing, status is parsed opportunistically from inbound packets on the print Bulk IN pipe without injecting extra polling packets.
3. **Cooperative Direct Fallback:**
   If `pixmad` is stopped (e.g. minimal container, recovery shell, or embedded test), `pixma-cups` and `pixma-sane` fall back to cooperative file locking using `nix::fcntl::flock` on `/run/lock/pixma-g2010.lock`.

---

## 2. Printing Stack: Dual-Path Architecture

```
                               ┌────────────────────────────────────────────────────────┐
                               │                    Print Job Sources                   │
                               │  Desktop Apps / CUPS Clients / Mobile AirPrint / CLI   │
                               └───────────────────────────┬────────────────────────────┘
                                                           │
                      ┌────────────────────────────────────┴───────────────────────────────────┐
                      │                                                                        │
┌─────────────────────▼──────────────────────────────┐              ┌──────────────────────────▼─────────────────────────┐
│ Path A: Native CUPS 2.x Integration                │              │ Path B: Driverless IPP Everywhere (CUPS 3.x PAPPL) │
│ • PPD: /usr/share/cups/model/pixma-g2010.ppd       │              │ • pixma-printer-app embedded daemon                │
│ • Filter: rastertopixma-g2010                      │              │ • Pure Rust IPP 2.0 / HTTP server on 127.0.0.1:8631│
│ • Cmd: cmdtopixma-g2010 (Clean, Align, Levels)     │              │ • Zero-conf mDNS/DNS-SD (mdns-sd) advertisement    │
│ • Backend: cnusb-g2010 (pixma-usb://04a9/183a)     │              │ • Ingests PWG-Raster, Apple Raster, PDF natively   │
└─────────────────────┬──────────────────────────────┘              └──────────────────────────┬─────────────────────────┘
                      │                                                                        │
                      └────────────────────────────────────┬───────────────────────────────────┘
                                                           │
                                ┌──────────────────────────▼──────────────────────────┐
                                │          pixma-raster: Pure-Rust Pipeline           │
                                ├─────────────────────────────────────────────────────┤
                                │ 1. Parser: CUPS Raster v2 / PWG Raster / PDF        │
                                │ 2. Color Mgmt: ICC Transforms (qcms / lcms2)        │
                                │ 3. Ink Separation: Dynamic GCR/UCR:                 │
                                │    • Plain Paper  -> Pigment Black (PGBK)           │
                                │    • Glossy Photo -> Composite CMY (No PGBK!)       │
                                │ 4. Halftoning / Dithering: Multi-Level SIMD:        │
                                │    • Blue Noise Void-and-Cluster (High Speed)       │
                                │    • Sierra / Jarvis-Judice-Ninke Error Diffusion   │
                                │ 5. Canon BJRaster3 / IVEC Command Formatter         │
                                └──────────────────────────┬──────────────────────────┘
                                                           │
                                ┌──────────────────────────▼──────────────────────────┐
                                │             pixmad IPC or nusb Transport            │
                                └─────────────────────────────────────────────────────┘
```

### 2.1 Path A: Native CUPS 2.x Integration

Native CUPS integration provides drop-in compatibility with existing Linux print queues.

#### 1. Filter: `rastertopixma-g2010`
* Takes standard CUPS Raster stream (`application/vnd.cups-raster` or `image/pwg-raster`) from `stdin`.
* Parses raster headers (`cups_header_t` / PWG Raster header): width, height, resolution (e.g. 600x600, 1200x1200), color space (sRGB, Grayscale, CMYK).
* Emits compressed Canon BJRaster3 data stream into `stdout`.

#### 2. Command Filter: `cmdtopixma-g2010`
* Handles `application/vnd.cups-command` files submitted via CUPS web interface or CLI (`lp -d G2010 -o raw clean.cmd`).
* Parses `#CUPS-COMMAND` directives and emits Canon IVEC XML maintenance payloads (`servicetype="maintenance"` and `servicetype="status"`):
  * `Clean`, `CleanBlack`, `CleanColor` -> `<ivec:operation>Cleaning</ivec:operation>` with `<ivec:type>regular</ivec:type>` and target `all`, `group1`, or `group2`.
  * `DeepClean`, `DeepCleanBlack`, `DeepCleanColor` -> `<ivec:operation>Cleaning</ivec:operation>` with `<ivec:type>deep</ivec:type>`.
  * `InkFlush YES-DRAIN-INK` -> `<ivec:operation>Cleaning</ivec:operation>` with `<ivec:type>choke</ivec:type>` (guarded by `MaintenanceSupervisor` 24h cooldown and `D-Counter < 80%`).
  * `RollerClean`, `BottomPlateClean` -> `<ivec:operation>RollerCleaning</ivec:operation>` (`roller` or `platen`).
  * `PrintSelfTestPage`, `ManualAlign`, `AutoAlign`, `AlignmentCheck` -> `<ivec:operation>TestPrint</ivec:operation>` (`nozzle_check`, `manual_registration`, `half_auto_registration`, `regi_check`).
  * `ReportLevels`, `ReportStatus` -> `<ivec:operation>GetStatus</ivec:operation>`, reporting `ATTR: marker-levels=...` and `STATE:` back to CUPS `stderr`.

#### 3. Backend: `cnusb-g2010`
* Supports CUPS device discovery mode (`/usr/lib/cups/backend/cnusb-g2010` called without arguments):
  ```text
  direct cnusb-g2010://Canon/G2010%20series?serial=KNDG00001 "Canon PIXMA G2010 series" "Canon PIXMA G2010 series USB" "MFG:Canon;MDL:G2010 series;CMD:BJL,BJRaster3,NCCe,IVEC,PWGRaster;CLS:PRINTER;DES:Canon G2010 series;" ""
  ```
* Supports job execution: Connects to `pixmad` or claims USB Interface 1 via `nusb`, pipes raster stream, and reports page progress and marker levels back to the CUPS scheduler.

#### 4. Comprehensive PPD Specification (`pixma-g2010.ppd`)
Exposes all hardware capabilities supported by the G2010 firmware:
* **`*OpenUI *MediaType/Media Type:`**
  * `plain/Plain Paper`: High-speed pigment black text mode (`stationery`).
  * `glossygold/Photo Paper Plus Glossy II (PP-201)`: Composite CMY black (`custom-media-type-canon-6`).
  * `proplat/Photo Paper Pro Platinum (PT-101)`: Composite CMY black (`custom-media-type-canon-3`).
  * `luster/Photo Paper Pro Luster (LU-101)`: Composite CMY black (`custom-media-type-canon-17`).
  * `semigloss/Photo Paper Plus Semi-gloss (SG-201)`: Composite CMY black (`custom-media-type-canon-9`).
  * `glossypaper/Glossy Photo Paper (GP-501)`: Composite CMY black (`photographic-glossy`).
  * `matte/Matte Photo Paper (MP-101)`: Matte coating profile (`custom-media-type-canon-11`).
  * `highres/High Resolution Paper (HR-101N)`, `envelope/Envelope`, `ijpostcard/Inkjet Hagaki`, `postcard/Hagaki`, `tshirt/T-Shirt Transfers`, `cardstock/Cardstock`.
* **`*OpenUI *Resolution/Print Quality & Resolution:`**
  * `1200dpi/Level 1 - Ultra Fine Photo (1200x1200 dpi)`
  * `601dpi/Level 2 - High Quality (600x600 Multi-Pass)`
  * `600dpi/Level 3 - Standard (600x600 dpi)`
  * `300dpi/Level 5 - Fast Draft (300x300 dpi)`
* **`*OpenUI *CNBorderlessExtension/Borderless Amount of Extension:`**
  * `Off/Standard Borders (Off)`
  * `Ext0/Level 0 - No Extension (Exact Size)`
  * `Ext1/Level 1 - Small Extension`
  * `Ext2/Level 2 - Medium Extension (Recommended)`
  * `Ext3/Level 3 - Large Extension (Zero White Edge)`
* **`*OpenUI *CNInkDryingTime/Ink Drying Wait Time:`**
  * `Normal/Standard`
  * `Long/Extended (Prevents smearing on saturated cardstock)`
  * `ExtraLong/Maximum Wait`
* **`*OpenUI *CNAbrasion/Prevent Paper Abrasion:`**
  * `False/Off`
  * `True/On (Widens head clearance for thick/textured paper)`
* **`*OpenUI *CNColorBalance/Manual Color & Intensity:`**
  * Fine slider calibration: Cyan (-50..50), Magenta (-50..50), Yellow (-50..50), Brightness (Dark/Normal/Light), Contrast (-50..50).
* **`*OpenUI *CNQuietMode/Quiet Mode:`**
  * `Off/Full Speed`
  * `On/Reduced Acoustic Noise`
  * `Scheduled/Time-window Controlled`

---

### 2.2 Path B: Driverless IPP Everywhere / Printer Application (CUPS 3.x Ready)

OpenPrinting CUPS 3.0 removes PPD support and traditional filter chains. `pixma-printer-app` provides a native, forward-compatible PAPPL-style daemon written in Rust.

* **Embedded Server:** Built with `axum` / `tokio`, listening on `127.0.0.1:8631`.
* **Zero-Conf Discovery:** Publishes DNS-SD / mDNS service via pure-Rust `mdns-sd`:
  * Service: `_ipp._tcp.local`, `_printer._tcp.local`.
  * TXT Records: `rp=ipp/print`, `ty=Canon PIXMA G2010`, `pdl=image/pwg-raster,image/urf,application/pdf`, `Color=T`, `Duplex=F`.
* **IPP Object Model:** Full implementation of IPP/2.0 attributes:
  * `marker-names`: `["Pigment Black (GI-790)", "Cyan (GI-790)", "Magenta (GI-790)", "Yellow (GI-790)"]`
  * `marker-colors`: `["#000000", "#00FFFF", "#FF00FF", "#FFFF00"]`
  * `marker-types`: `["ink-cartridge", "ink-cartridge", "ink-cartridge", "ink-cartridge"]`
  * `marker-levels`: Real-time percentage gauges dynamically updated via USB status.
  * `printer-state`: `idle`, `processing`, or `stopped` with descriptive `printer-state-reasons` (`media-empty-error`, `cover-open`).

---

### 2.3 Pure-Rust Color Pipeline & Rasterizer (`pixma-raster`)

#### 1. Input Processing
* **CUPS / PWG Raster:** Built-in decoder parsing PWG raster packbits chunks into scanline buffers.
* **PDF Rasterization:** Multi-backend strategy:
  * Default: `pdfium-render` (industrial standard rendering of complex vector graphics, transparency, and CMYK embeddings).
  * Pure Rust fallback: `hayro` / `lopdf` paired with `tiny-skia` for zero-C embedded deployments.
* **Images:** Uses the safe `image` crate (PNG, JPEG, WebP, TIFF).

#### 2. Color Management Architecture
* **Engine Evaluation:**
  * `qcms` (Mozilla): 100% pure Rust, zero-allocation SIMD transforms for sRGB -> RGB. Excellent for standard RGB images, but lacks full 4-channel CMYK A2B device link profile transforms.
  * `lcms2` (LittleCMS2 FFI): Industry gold standard for ICC profiles. Supports multidimensional LUTs (A2B0/A2B1/B2A0), device-link profiles, and full rendering intents (Perceptual, Relative Colorimetric, Saturation).
* **Architecture Choice:** Modular color engine with a default pure-Rust high-speed matrix-LUT transform pipeline (`pixma-color-core`), and an optional compile-time Cargo feature `--features lcms2` for professional prepress ICC calibration.

#### 3. CMYK Ink Separation & Black Generation (The MegaTank Dilemma)
The Canon PIXMA G2010 uses a 4-color MegaTank setup:
1. **GI-790 PGBK (Pigment Black):** Large pigment particles suspended in solvent. Exceptional optical density on uncoated cellulose paper, but **cannot adhere to the micro-porous silica coating of glossy photo paper** (it flakes off, puddles, and causes bronzing).
2. **GI-790 C, M, Y (Dye-based Colors):** Molecular dye solutions that penetrate cleanly into photo paper coatings.

```
       Plain Paper Mode                       Glossy Photo Paper Mode
  ┌────────────────────────┐                ┌────────────────────────┐
  │ sRGB Document Input    │                │ sRGB Document Input    │
  └───────────┬────────────┘                └───────────┬────────────┘
              │                                         │
              ▼                                         ▼
  ┌────────────────────────┐                ┌────────────────────────┐
  │ GCR / UCR Separation   │                │ Composite CMY Splitting│
  │ • Black -> PGBK (100%) │                │ • Black -> C + M + Y   │
  │ • CMY -> Graphics only │                │ • PGBK -> 0% (Forced!) │
  └───────────┬────────────┘                └───────────┬────────────┘
              │                                         │
              ▼                                         ▼
  ┌────────────────────────┐                ┌────────────────────────┐
  │ Nozzle Map:            │                │ Nozzle Map:            │
  │ PGBK: Active           │                │ PGBK: Shut off         │
  │ CMY:  Active           │                │ CMY:  Dense dye mix    │
  └────────────────────────┘                └────────────────────────┘
```

**Implementation Rules:**
* **On Plain Paper:** Apply Under Color Removal (UCR) and Gray Component Replacement (GCR):
  $$K_{pgbk} = \min(C, M, Y) \times \gamma_{gcr}$$
  $$C' = C - K_{pgbk}, \quad M' = M - K_{pgbk}, \quad Y' = Y - K_{pgbk}$$
  Pure black text ($R=G=B=0$) triggers 100% $K_{pgbk}$ and 0% CMY for sharp, waterproof typography.
* **On Glossy / Luster / Semi-Gloss Photo Paper:**
  $$K_{pgbk} \equiv 0$$
  Black tones are rendered purely by composite mixing of dye inks ($C_{dye} + M_{dye} + Y_{dye}$).

#### 4. Halftoning & Multi-Level Dithering
The G2010 print head produces multi-size micro-droplets (e.g. 2pl and 5pl), corresponding to 2-bit multi-level dot states (Off, Small, Medium, Large drop).

* **Blue Noise Void-and-Cluster Matrix:**
  * Pre-computed $128 \times 128$ isotropic blue-noise threshold tiles.
  * Fully parallelized across scanlines via `rayon` and AVX2/NEON SIMD vector comparisons.
  * 15x faster than error diffusion; zero directional worming artifacts.
* **Advanced Error Diffusion:**
  * Implemented with **Jarvis-Judice-Ninke** and **Sierra-3** filters with serpentine raster traversal (alternating left-to-right and right-to-left) to suppress directional grain.
  * SIMD-vectorized tiled processing with inter-tile halo error distribution.

---

## 3. Scanner Stack: SANE Backend, eSCL & Post-Processing

```
┌────────────────────────────────────────────────────────────────────────────────────────┐
│                                Scanner Clients                                         │
│ Simple Scan (GNOME) │ Skanpage (KDE) │ gscan2pdf │ scanimage CLI │ AirScan (macOS/iOS) │
└───────────────────────┬──────────────────────────────────┬─────────────────────────────┘
                        │                                  │
┌───────────────────────▼────────────────┐ ┌───────────────▼────────────────────────────┐
│ SANE C ABI Backend: libsane-pixma_g2010│ │ Driverless eSCL (AirScan) HTTP/XML Server   │
│ • Drop-in cdylib shared library        │ │ • Axum / Tokio REST daemon on port 8080     │
│ • Implements SANE API v1               │ │ • /eSCL/ScannerCapabilities & /ScanJobs     │
│ • Catch unwinding panics at FFI bounds │ │ • Advertises _uscan._tcp via mdns-sd        │
└───────────────────────┬────────────────┘ └───────────────┬────────────────────────────┘
                        │                                  │
                        └─────────────────┬────────────────┘
                                          │
                  ┌───────────────────────▼────────────────────────┐
                  │ pixma-scanner & pixma-imageproc Engine         │
                  ├────────────────────────────────────────────────┤
                  │ 1. Raw Scanner Protocol (pixma_mp150 commands) │
                  │ 2. CIS Sensor Line Stagger Realignment Buffer  │
                  │ 3. ScanGear Pro Image Post-Processing:         │
                  │    • Auto-Multi-Crop (Otsu + Contours)         │
                  │    • Deskew (Radon / Hough Angle Correction)   │
                  │    • Descreening (2D FFT Moire Removal)        │
                  │    • Dust & Scratch Removal (Morphology)       │
                  │    • Gutter Shadow Compensation                │
                  │    • Tone Curves / Auto-Levels Histogram       │
                  │ 4. Exporters: Searchable PDF, TIFF, PNG, JPEG  │
                  └───────────────────────┬────────────────────────┘
                                          │
                  ┌───────────────────────▼────────────────────────┐
                  │          pixmad IPC or nusb Transport          │
                  └────────────────────────────────────────────────┘
```

### 3.1 SANE C ABI Backend (`libsane-pixma_g2010.so`)

Written in 100% safe Rust as a `cdylib`, exporting the full SANE Standard v1.04 C ABI:
* `sane_init`, `sane_exit`
* `sane_get_devices`, `sane_open`, `sane_close`
* `sane_get_option_descriptor`, `sane_control_option`
* `sane_get_parameters`, `sane_start`, `sane_read`, `sane_cancel`

#### Safety Guarantees at the FFI Boundary
1. **Panic Protection:** Every `extern "C"` function wraps internal calls in `std::panic::catch_unwind`. Uncaught Rust panics never cross the C boundary (preventing undefined behavior and aborts in calling applications like `simple-scan`).
2. **Deterministic Pointer Lifetime Management:** Option descriptor arrays and string tables are allocated in pinned Rust heap structures owned by the device handle, preventing pointer invalidation.
3. **Safe Buffer Bounds:** `sane_read` slices input pointers into Rust safe slices `std::slice::from_raw_parts_mut`, strictly respecting `max_length`.

#### CIS Sensor Line-Stagger Realignment
The G2010 flatbed utilizes a Contact Image Sensor (CIS) where Red, Green, and Blue sensor rows are physically separated by fixed line offsets. Scanning without software compensation causes **chromatic fringe ghosting**:
* At 600 DPI: Green is offset by $+2$ lines, Blue by $+4$ lines relative to Red.
* At 1200 DPI: Green is offset by $+4$ lines, Blue by $+8$ lines.
* **Rust Implementation:** A zero-allocation circular ring buffer (`LineStaggerBuffer`) buffers scanlines in memory and realigns R, G, B channel components in a single cache-coherent pass before serving data to `sane_read`.

---

### 3.2 Driverless eSCL (AirScan) Server (`pixma-escl`)

An embedded HTTP/XML microserver allowing driverless network scanning from any Linux, Android, iOS, or macOS device on the LAN.
* **Endpoints:**
  * `GET /eSCL/ScannerCapabilities`: Returns XML manifest defining Flatbed source, 75–1200 DPI optical resolutions, color formats (`image/jpeg`, `image/pdf`, `image/png`), and scan dimensions (max 216x297mm).
  * `GET /eSCL/ScannerStatus`: Returns scanner operational state (`Idle`, `Processing`, `Testing`).
  * `POST /eSCL/ScanJobs`: Accepts XML settings payload specifying region, format, and resolution. Allocates scan session UUID.
  * `GET /eSCL/ScanJobs/{uuid}/NextDocument`: Streams multi-part chunked scanned image data.
  * `DELETE /eSCL/ScanJobs/{uuid}`: Cancels scan job and returns carriage to home park.
* **mDNS Zero-Conf:** Advertises `_uscan._tcp` and `_uscans._tcp` on port 8080. Automatically picked up by `sane-airscan`.

---

### 3.3 Pure-Rust Post-Processing Pipeline (Matching Windows ScanGear)

All image processing algorithms are implemented in pure Rust (`pixma-imageproc`) without external C dependencies (OpenCV/Leptonica):

1. **Auto-Multi-Crop (Multi-Scan):**
   * Automatically segments multiple photographs or receipts scattered on the flatbed glass.
   * Algorithm: Downscales preview scan, applies Otsu adaptive binarization, finds outer contours (`imageproc::contours`), filters out noise by bounding area, computes oriented minimum-area bounding boxes with padding, crops and outputs independent files.
2. **Deskew:**
   * Detects document rotation angle via Radon transform or horizontal edge gradient accumulation.
   * Performs high-fidelity bicubic rotation with border extrapolation.
3. **Descreening (Moire Removal):**
   * When scanning offset-printed magazines or newspapers, halftone print dots cause unsightly moire interference patterns.
   * Algorithm: 2D Fast Fourier Transform (`rustfft`), dynamic frequency-domain peak detection to locate periodic halftone carrier frequencies, applies a Gaussian notch filter, and performs inverse FFT.
4. **Unsharp Masking:**
   * Enhances text and photographic edge contrast:
     $$I_{sharp} = I + \lambda \cdot (I - \text{GaussianBlur}(I, \sigma))$$
   * AVX2/NEON SIMD accelerated convolution.
5. **Dust & Scratch Removal:**
   * Detects surface flecks and scratches via morphological Top-Hat and Black-Hat filtering.
   * Heals defective regions using modified Telea fast-marching inpainting.
6. **Gutter Shadow Correction:**
   * Compenses for the dark gradient shadow that occurs along the spine of thick books placed on the flatbed.
   * Detects the non-linear darkening curve and applies an exponential brightness gain function across the inner boundary.
7. **Histogram & Tone Curve Editor:**
   * Generates real-time 256-bin histograms.
   * Parametric cubic spline tone curves (Highlights, Midtones, Shadows, Black Point, White Point clipping).
8. **Document Exporters:**
   * **PDF:** Pure-Rust searchable PDF generation via `pdf-writer`.
   * **TIFF:** Lossless Deflate/LZW compression via `image` / `tiff`.
   * **PNG / JPEG:** Direct encoding via `png` and `jpeg-encoder`.

---

## 4. GUI & CLI Architecture & Modular Packaging

### 4.1 Linux GUI Toolkit Evaluation (Wayland / X11 on Arch Linux)

| Framework | Wayland / Arch Integration | Desktop Look & HIG | Memory & CPU Footprint | Recommendation |
| :--- | :--- | :--- | :--- | :--- |
| **Relm4 (GTK4 + libadwaita)** | **Flawless**. First-class Wayland fractional scaling, native system dark/light theme, system fonts, portal integration. | **Native GNOME/Arch HIG**. Standard `AdwPreferencesWindow`, `AdwActionRow`, `AdwStatusPage`, `AdwViewSwitcher`. | Low idle footprint (< 35MB RAM). Hardware-accelerated GTK4 rendering. | **Primary Choice for Desktop Studio ("PIXMA G2010 Studio")**. |
| **`iced`** | Excellent via `winit` / `smithay-client-toolkit`. Pure Rust (wgpu / tiny-skia). | Custom non-native widgets. Requires manual recreation of complex desktop preferences, file pickers, and tab bars. | Low memory, pure Rust, but feels like an embedded/gaming UI rather than a native Linux desktop utility. | Excellent for standalone diagnostic tools, but less HIG-compliant for daily desktop users. |
| **`Slint`** | Great Wayland/X11 support. Software/OpenGL renderers. | Polished, but requires learning proprietary Slint DSL (`.slint`). Dual licensed (GPLv3 / commercial). | Extremely low RAM (< 15MB). | Viable alternative, but DSL introduces build complexity. |
| **`egui`** | Immediate mode. Works on Wayland via `eframe`. | Completely non-native styling. Difficult desktop text rendering, IME handling, and continuous repaint overhead. | Higher idle CPU/GPU usage without manual reactive throttling. | Great for internal debug tools; unsuitable for end-user system settings app. |

> **Selection:** **Relm4 with Libadwaita** is selected for **PIXMA G2010 Studio** to provide an authentic, modern Linux desktop experience on Arch Linux with flawless Wayland fractional scaling and system theme synchronization.

---

### 4.2 Unified Desktop App: "PIXMA G2010 Studio" Architecture

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                       PIXMA G2010 Studio (Relm4)                            │
│  [ Print Settings ]   [ Maintenance ]   [ ScanGear Pro ]   [ Live Status ]  │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│  • Print Settings:                                                          │
│    - Media Type Selector (Plain, Glossy II, Luster, Matte, Envelope)        │
│    - Quality Level (1 to 5) & Borderless Overscan Sliders                   │
│    - Color Balance Preview & Ink Drying Wait Time Configuration             │
│                                                                             │
│  • Maintenance Assistant:                                                   │
│    - Head Cleaning (Normal & Deep Clean) with nozzle animation              │
│    - Interactive Nozzle Check Pattern Viewer                                │
│    - Roller Cleaning & Bottom Plate Cleaning Routines                       │
│    - Ink System Flush & Maintenance Pad Absorber Reset Wizard               │
│                                                                             │
│  • ScanGear Pro:                                                            │
│    - Interactive Live Scanner Preview with draggable crop handles           │
│    - Resolution (75 to 1200 DPI) & Color Mode (Color, Gray, Lineart)        │
│    - Auto-Multi-Crop Button (auto-segments photos on flatbed)               │
│    - Real-time Histogram & Tone Curve Spline Adjuster                       │
│    - Filters: Deskew, Descreen (Moire), Unsharp Mask, Gutter Shadow         │
│    - Multi-page PDF, TIFF, PNG, JPEG Exporter                               │
│                                                                             │
│  • Live Status Monitor:                                                     │
│    - Visual MegaTank Ink Level Gauges (GI-790 PGBK, C, M, Y)                │
│    - Real-time Hardware Alerts: Paper Jam, Cover Open, Ink Low              │
│    - Print Queue Inspector & Cancel Button                                  │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

### 4.3 Headless Maintenance CLI (`pixma-cli`)

For servers, headless setups, automation scripts, and sysadmins:
* `pixma-cli status`: Visual ASCII ink tank gauges, waste absorber counter, printer operational state, and support code lookup (`--json` supported).
* `pixma-cli clean --kind <regular|deep|flush> --target <all|black|color> [--confirm YES-DRAIN-INK]`: Triggers printhead cleaning with cooldown and waste absorber interlocks.
* `pixma-cli roller-clean --target <roller|platen>`: Triggers paper feed roller or bottom plate cleaning.
* `pixma-cli nozzle-check`: Prints the 1-sheet nozzle check diagnostic grid.
* `pixma-cli align --action <print-manual|half-auto|check|apply> [--values "0,1,-1,..."]`: Printhead alignment routines and 14-column offset registration.
* `pixma-cli scan --dpi 600 --output doc.pdf [--auto-crop] [--deskew] [--descreen] [--unsharp] [--remove-dust] [--gutter-shadow]`: Headless scanning with ScanGear Pro post-processing.
* `pixma-cli print --input document.png --media plain --paper a4 --quality 3`: Direct headless printing.

---

### 4.4 Cargo Workspace Modular Structure

```
pixma-g2010-rs/
├── Cargo.toml                    # Virtual Workspace Root
├── crates/
│   ├── pixma-core/               # Shared types, error definitions, USB VID/PID constants
│   ├── pixma-transport/          # nusb async transport, hotplug, interface claim, locking
│   ├── pixma-protocol/           # Canon BJRaster3, scanner mp150 packets, maintenance commands
│   ├── pixma-color/              # ICC transform, SIMD matrix-LUT, GCR/UCR ink separation
│   ├── pixma-raster/             # CUPS/PWG raster parser, Blue Noise & Sierra halftoning
│   ├── pixma-imageproc/          # Pure-Rust auto-crop, deskew, 2D-FFT descreen, unsharp mask
│   ├── pixma-daemon/             # pixmad background multiplexer & IPC socket server
│   ├── pixma-cups/               # CUPS filters (rastertopixma, cmdtopixma, cnusb backend, PPD gen)
│   ├── pixma-printer-app/        # Driverless IPP Everywhere server (CUPS 3.x PAPPL style)
│   ├── pixma-sane/               # cdylib -> libsane-pixma_g2010.so.1 (SANE C ABI)
│   ├── pixma-escl/               # Driverless eSCL (AirScan) HTTP/XML microserver
│   ├── pixma-cli/                # pixma-cli headless administration tool
│   └── pixma-gui/                # PIXMA G2010 Studio (Relm4 / libadwaita desktop GUI)
├── assets/
│   ├── ppd/                      # Template PPD files
│   ├── udev/                     # 70-pixma-g2010.rules
│   ├── systemd/                  # pixmad.service, pixma-escl.service, pixma-printer-app.service
│   └── desktop/                  # org.pixma.G2010Studio.desktop, application icons
├── packaging/
│   └── arch/                     # Modular Arch Linux PKGBUILD
└── scripts/
    └── install.sh                # Interactive TUI modular installer
```

---

### 4.5 Arch Linux Modular Packaging Strategy (`PKGBUILD`)

Split package structure allowing users on Arch Linux to install only what they need:

```bash
# Maintainer: Vim-01
pkgbase=pixma-g2010-rs
pkgname=(
  'pixma-g2010-core'         # Udev rules, pixmad multiplexer, base transport
  'pixma-g2010-cups'         # CUPS filters, backend, and PPD generator
  'pixma-g2010-sane'         # SANE backend (libsane-pixma_g2010.so)
  'pixma-g2010-escl'         # Standalone eSCL AirScan server
  'pixma-g2010-printer-app'  # Standalone CUPS 3.x IPP Everywhere server
  'pixma-g2010-cli'          # Maintenance CLI tool
  'pixma-g2010-gui'          # PIXMA G2010 Studio (Relm4 / libadwaita)
  'pixma-g2010-meta'         # Meta-package pulling the complete desktop suite
)
pkgver=0.1.0
pkgrel=1
arch=('x86_64' 'aarch64')
license=('AGPL-3.0-or-later')
makedepends=('cargo' 'git' 'pkgconf' 'gtk4' 'libadwaita')
```

#### Package Sub-allocations
1. `pixma-g2010-core`: Installs `/usr/lib/udev/rules.d/70-pixma-g2010.rules`, `/usr/bin/pixmad`, and `/usr/lib/systemd/system/pixmad.service`.
2. `pixma-g2010-cups`: Depends on `cups` and `pixma-g2010-core`. Installs `/usr/lib/cups/filter/rastertopixma-g2010`, `/usr/lib/cups/filter/cmdtopixma-g2010`, `/usr/lib/cups/backend/cnusb-g2010`, and `/usr/share/cups/model/pixma-g2010.ppd`.
3. `pixma-g2010-sane`: Depends on `sane` and `pixma-g2010-core`. Installs `/usr/lib/sane/libsane-pixma_g2010.so.1` and `/etc/sane.d/dll.d/pixma_g2010`.
4. `pixma-g2010-gui`: Depends on `gtk4`, `libadwaita`, and `pixma-g2010-core`. Installs `/usr/bin/pixma-studio` and desktop launcher.

---

### 4.6 Interactive Modular TUI Installer

For users compiling directly from source or running on non-AUR setups, `scripts/install.sh` provides an interactive terminal checklist:

```text
┌───────────────────────── Canon PIXMA G2010 Suite Installer ─────────────────────────┐
│ Select the modules you wish to build and install on this system:                    │
│                                                                                     │
│    [*] 1. System Udev Rules & pixmad Multiplexer (Required for USB access)          │
│    [*] 2. Classic CUPS Printing Pipeline (raster filter, backend, PPD)              │
│    [*] 3. SANE Scanner Driver (libsane-pixma_g2010.so for Simple Scan / Skanpage)   │
│    [ ] 4. CUPS 3.x Driverless IPP Everywhere Printer Application                    │
│    [*] 5. Driverless eSCL (AirScan) HTTP Server (LAN scan sharing)                  │
│    [*] 6. Headless Maintenance CLI Utility (pixma-cli)                              │
│    [*] 7. PIXMA G2010 Studio Desktop App (Relm4 / Libadwaita GUI)                  │
│                                                                                     │
│                       < Install Selected >      < Cancel >                          │
└─────────────────────────────────────────────────────────────────────────────────────┘
```

The script runs `cargo build --release -p <selected_crates>`, installs binaries to `/usr/bin`, installs udev rules and systemd units, registers the SANE backend in `/etc/sane.d/dll.d/`, reloads `systemd`, and restarts CUPS.

---

## 5. Architectural Quality Attributes & Verification Matrix

1. **Memory Safety:** 100% safe Rust across all USB transfers, raster calculations, and SANE bindings. Panics are trapped at FFI boundaries. Zero C-heap leaks.
2. **Speed & Throughput:**
   * Async I/O handles USB streaming without blocking OS threads.
   * Color separation and dithering are vectorized with AVX2/NEON SIMD and multi-threaded via `rayon`.
   * Able to stream full 4800x1200 DPI photo rasters at line rate with zero mechanical stutter.
3. **Resilience:** Sudden USB cable disconnection causes immediate, clean teardown without orphan threads, deadlocks, or kernel driver lockouts.
4. **Desktop Integration:** First-class Wayland support with crisp fractional scaling, dark/light theme switching, and native desktop notifications on ink depletion or paper jam.

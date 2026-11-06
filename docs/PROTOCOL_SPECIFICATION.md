# Clean-Room Hardware & Protocol Functional Specification: Canon PIXMA G2010 Series

**Document Status:** Approved Clean-Room Specification  
**License:** AGPL-3.0-or-later  
**Target Hardware:** Canon PIXMA G2010 (and compatible G1010 / G2000 / G3010 / G4010 series)

---

## 1. USB Hardware Enumeration & Endpoint Map

### 1.1 USB Vendor & Product Identifiers
- **Vendor ID (VID):** `0x04A9` (Canon Inc.)
- **Product IDs (PID):**
  - `0x183A`: Canon PIXMA G2010 / G2410 Series (USB Multifunction: Print + Scan)
  - `0x183B`: Canon PIXMA G3010 Series (USB + Network Multifunction: Print + Scan)
  - `0x183C`: Canon PIXMA G1010 Series (USB Printer Only)
  - `0x183D`: Canon PIXMA G4010 Series (USB + Network + ADF Multifunction)
  - `0x1794`: Canon PIXMA G3000 Series
  - `0x1795`: Canon PIXMA G2000 Series
  - `0x18BD`: Canon PIXMA G2020 Series

### 1.2 USB Composite Interfaces (PID `0x183A`)
1. **Interface 0 (`bInterfaceNumber = 0`) — Scanner & Vendor Control**
   - `bInterfaceClass = 0xFF` (Vendor Specific), `bInterfaceSubClass = 0xCC`, `bInterfaceProtocol = 0x00`
   - **Bulk OUT (`0x01` or `0x02`):** Host-to-device IVEC XML scan handshake and binary MP2 scanner commands.
   - **Bulk IN (`0x81` or `0x82`):** Device-to-host binary MP2 responses and scanline image chunks.
   - **Interrupt IN (`0x83`):** Hardware panel button events (Start Black, Start Color, Stop).
2. **Interface 1 (`bInterfaceNumber = 1`) — Bidirectional Printer**
   - `bInterfaceClass = 0x07` (Printer), `bInterfaceSubClass = 0x01`, `bInterfaceProtocol = 0x02` (Bi-directional)
   - **Bulk OUT (`0x02` or `0x03`):** Host-to-device BJL, IVEC XML, PWG Raster (`RaS2`), and BJRaster3 (`ESC (`) streams.
   - **Bulk IN (`0x82` or `0x84`):** Device-to-host IVEC XML status responses (`GetStatus`).
   - **Control Endpoint `0x00`:**
     - `GET_DEVICE_ID` (`bmRequestType = 0xA1`, `bRequest = 0x00`): Returns 2-byte big-endian length prefix followed by IEEE-1284 ID string:
       `MFG:Canon;CMD:BJRaster3,NCCe,IVEC;SOJ:CHMP;MDL:G2010 series;CLS:PRINTER;DES:Canon G2010 series;VER:1.020;STA:10;CID:CA_IVEC1TYPE4_IJP;`
     - `GET_PORT_STATUS` (`bmRequestType = 0xA1`, `bRequest = 0x01`): Returns 1-byte status register (`Bit 5` = Paper Empty, `Bit 4` = Select, `Bit 3` = Not Error).
     - `SOFT_RESET` (`bmRequestType = 0x21`, `bRequest = 0x02`).

---

## 2. Hardware Safety Boundaries & Interlocks (Mandatory)

### 2.1 Printhead Thermal Architecture
- **Black Printhead (BH-4 / BH-7):** 320 thermal inkjet nozzles (Pigment Black GI-790 PGBK), 600×1200 dpi.
- **Color Printhead (CH-41 / CH-7):** 1,152 thermal inkjet nozzles (384 Cyan, 384 Magenta, 384 Yellow Dye GI-790), up to 4800×1200 dpi, 2 pL minimum droplet.
- **Thermal Cooling Constraint:** Liquid ink is the sole physical coolant for the tantalum-aluminum thin-film heating resistors ($300^\circ\text{C}\text{--}350^\circ\text{C}$ firing pulse). There is no physical liquid level sensor in the CISS tanks.
- **Duty-Cycle Throttling Rule:**
  $$\text{DutyCycle} = \frac{\text{FiredDots}}{\text{TotalDotsInBand}}$$
  When consecutive passes exceed $85\%$ duty cycle, insert a $150\text{--}350\ \text{ms}$ inter-band cooling pause.
- **Thermal / Head Error Fast Abort:** If `GetStatus` returns support code `1401`, `1403`, `1405`, `1470`, `1471`, `1472`, or `5200` (`P08` Head Overheated), immediately abort raster emission and send `<ivec:operation>CancelJob</ivec:operation>`.

### 2.2 Mechanical Carriage & Paper Feed Bounds
- **Maximum Carriage Width:**
  - At 300 DPI: `2550` dots ($8.5\text{"}$)
  - At 600 DPI: `5100` dots ($8.5\text{"}$)
  - At 1200 DPI: `10200` dots ($8.5\text{"}$)
- **Maximum Printable Length:**
  - Standard sheet max: $676.0\ \text{mm}$ (`15968` dots at 600 DPI)
- **Standard Hardware Margins (Non-Borderless):**
  - Top margin: $3.0\ \text{mm}$ (`71` dots at 600 DPI)
  - Bottom margin: $5.0\ \text{mm}$ (`118` dots at 600 DPI)
  - Left/Right margin: $3.4\ \text{mm}$ (`80` dots at 600 DPI) for Letter, $3.0\ \text{mm}$ (`71` dots at 600 DPI) for A4 (`4960` printable dots out of `5100`).

### 2.3 Maintenance & Waste Ink Absorber (`D-Counter`) Interlocks
- **Standard Cleaning (`regular`):** Consumes $\sim 0.3\ \text{ml}$ ink, advances D-Counter by $+0.2\%$. Cooldown: 60 seconds.
- **Deep Cleaning (`deep`):** Consumes $\sim 2.0\ \text{ml}$ ink, advances D-Counter by $+1.2\%$. Cooldown: 10 minutes (`600` seconds).
- **System Cleaning / Ink Flush (`choke`):** Consumes **$35\text{--}55\ \text{ml}$ ink**, advances D-Counter by **$+20.0\%$**.
  - **Mandatory Cooldown:** 24 hours (`86400` seconds) persisted in state file.
  - **Absorber Capacity Block:** Hard-blocked if estimated or reported `D-Counter >= 80.0%` (preventing fatal `5B00` / `P07` lockout and physical ink overflow near the internal power supply).
  - **Explicit Confirmation:** Requires confirmation token `"YES-DRAIN-INK"` or explicit UI countdown confirmation.
- **EEPROM Factory Write Prohibition:** No raw `@BJL` factory EEPROM write commands may exist in the driver crate.

---

## 3. Printer & Maintenance Protocol Specification (Protocol Version 2: IVEC + BJL + PWG/BJRaster)

### 3.1 XML Envelope Format
All IVEC commands use UTF-8 XML with namespaces:
- `xmlns:ivec="http://www.canon.com/ns/cmd/2008/07/common/"`
- `xmlns:vcn="http://www.canon.com/ns/cmd/2008/07/canon/"`

### 3.2 Job Lifecycle Sequence (`servicetype="print"`)
1. **`SetJobConfiguration` (`VendorCmd`):**
   ```xml
   <?xml version="1.0" encoding="utf-8" ?>
   <cmd xmlns:ivec="http://www.canon.com/ns/cmd/2008/07/common/" xmlns:vcn="http://www.canon.com/ns/cmd/2008/07/canon/">
     <ivec:contents>
       <ivec:operation>VendorCmd</ivec:operation>
       <ivec:param_set servicetype="print">
         <vcn:ijoperation>SetJobConfiguration</vcn:ijoperation>
         <ivec:jobID>00000001</ivec:jobID>
         <vcn:hostenvironment>Linux</vcn:hostenvironment>
         <vcn:remotedriver>0</vcn:remotedriver>
         <vcn:driverinfo>
           <vcn:infoversion>1</vcn:infoversion>
           <vcn:os>Linux</vcn:os>
           <vcn:driverversion>5.00.1</vcn:driverversion>
           <vcn:language>en-US</vcn:language>
           <vcn:deviceid>1</vcn:deviceid>
           <vcn:deviceport>USB</vcn:deviceport>
         </vcn:driverinfo>
         <vcn:printstart>1</vcn:printstart>
         <vcn:user>user</vcn:user>
         <vcn:computer>host</vcn:computer>
         <vcn:document>document</vcn:document>
         <vcn:duplexprint>OFF</vcn:duplexprint>
         <vcn:jobtype>normal</vcn:jobtype>
         <vcn:externalid>00000001</vcn:externalid>
       </ivec:param_set>
     </ivec:contents>
   </cmd>
   ```
2. **`StartJob`:**
   ```xml
   <?xml version="1.0" encoding="utf-8" ?>
   <cmd xmlns:ivec="http://www.canon.com/ns/cmd/2008/07/common/">
     <ivec:contents>
       <ivec:operation>StartJob</ivec:operation>
       <ivec:param_set servicetype="print">
         <ivec:jobID>00000002</ivec:jobID>
         <ivec:bidi>1</ivec:bidi>
       </ivec:param_set>
     </ivec:contents>
   </cmd>
   ```
3. **BJL Time Synchronization (`SendData` with `RAW` 62-byte BJL payload):**
   Wrapped inside `<ivec:operation>SendData</ivec:operation>` with `<ivec:dataformat>RAW</ivec:dataformat><ivec:datasize>62</ivec:datasize>`:
   - 7-byte prefix: `\x1b[K\x02\x00\x00\x1f` (`1B 5B 4B 02 00 00 1F`)
   - Body (55 bytes):
     ```text
     BJLSTART\nControlMode=Common\nSetTime=YYYYMMDDHHMMSS\nBJLEND\n
     ```
4. **Per-Page Configuration (`SetPageConfiguration`):**
   ```xml
   <?xml version="1.0" encoding="utf-8" ?>
   <cmd xmlns:ivec="http://www.canon.com/ns/cmd/2008/07/common/" xmlns:vcn="http://www.canon.com/ns/cmd/2008/07/canon/">
     <ivec:contents>
       <ivec:operation>VendorCmd</ivec:operation>
       <ivec:param_set servicetype="print">
         <vcn:ijoperation>SetPageConfiguration</vcn:ijoperation>
         <ivec:jobID>00000002</ivec:jobID>
         <ivec:mediatype>stationery</ivec:mediatype>
         <ivec:papersize>iso_a4_210x297mm</ivec:papersize>
         <ivec:printquality>normal</ivec:printquality>
         <ivec:borderlessprint>OFF</ivec:borderlessprint>
         <ivec:colormode>color</ivec:colormode>
         <ivec:inputbin>auto</ivec:inputbin>
         <ivec:copies>1</ivec:copies>
         <vcn:totalpages>0</vcn:totalpages>
         <vcn:currentpage>1</vcn:currentpage>
         <vcn:Collate>OFF</vcn:Collate>
         <vcn:Pageorder>Ascending</vcn:Pageorder>
         <ivec:duplexprint>OFF</ivec:duplexprint>
         <ivec:duplextype>NONE</ivec:duplextype>
         <vcn:SpecifyMargin Type="millimeter">
           <vcn:PhysicalPaperWidth>210.0</vcn:PhysicalPaperWidth>
           <vcn:PhysicalPaperLength>297.0</vcn:PhysicalPaperLength>
           <vcn:TopMargin>3.0</vcn:TopMargin>
           <vcn:BottomMargin>5.0</vcn:BottomMargin>
           <vcn:LeftMargin>3.4</vcn:LeftMargin>
           <vcn:RightMargin>3.4</vcn:RightMargin>
         </vcn:SpecifyMargin>
         <vcn:EnableMargin>ON</vcn:EnableMargin>
         <vcn:orientation>0</vcn:orientation>
       </ivec:param_set>
     </ivec:contents>
   </cmd>
   ```
5. **Page Data Header (`SendData`):**
   ```xml
   <?xml version="1.0" encoding="utf-8" ?>
   <cmd xmlns:ivec="http://www.canon.com/ns/cmd/2008/07/common/">
     <ivec:contents>
       <ivec:operation>SendData</ivec:operation>
       <ivec:param_set servicetype="print">
         <ivec:jobID>00000002</ivec:jobID>
         <ivec:phase>start</ivec:phase>
         <ivec:PageNo>1</ivec:PageNo>
         <ivec:dataformat>PWGRaster</ivec:dataformat>
         <ivec:datasize>123456</ivec:datasize>
       </ivec:param_set>
     </ivec:contents>
   </cmd>
   ```
   Followed immediately by `datasize` bytes of `RaS2` PWG Raster data streamed in chunks up to 256 KB (`0x40000`).
6. **`EndJob` / `CancelJob`:**
   `<ivec:operation>EndJob</ivec:operation>` (or `CancelJob`) with `<ivec:jobID>00000002</ivec:jobID>`.

### 3.3 Supported Media Types & Quality Values in IVEC
- **Media Type mappings (`<ivec:mediatype>`):**
  - Plain Paper (`plain`) $\rightarrow$ `stationery`
  - Glossy Photo Paper (`glossypaper`) $\rightarrow$ `photographic-glossy`
  - Photo Paper Pro Platinum (`proplat`) $\rightarrow$ `custom-media-type-canon-3`
  - Photo Paper Plus Glossy II (`glossygold`) $\rightarrow$ `custom-media-type-canon-6`
  - Photo Paper Plus Semi-gloss (`semigloss`) $\rightarrow$ `custom-media-type-canon-9`
  - Matte Photo Paper (`matte`) $\rightarrow$ `custom-media-type-canon-11`
  - Envelope (`envelope`) $\rightarrow$ `custom-media-type-canon-12`
  - Inkjet Hagaki (`ijpostcard`) $\rightarrow$ `custom-media-type-canon-14`
  - Hagaki (`postcard`) $\rightarrow$ `custom-media-type-canon-15`
  - Photo Paper Pro Luster (`luster`) $\rightarrow$ `custom-media-type-canon-17`
  - High Resolution Paper (`highres`) $\rightarrow$ `custom-media-type-canon-18`
- **Print Quality (`<ivec:printquality>`):**
  - `fast_draft` (300×300 base DPI on `stationery`)
  - `normal` (600×600 base DPI)
  - `best` (600×600 / 1200×1200 high quality)
  - `auto`
- **Paper Gap / Abrasion Prevention (`<vcn:papergap>`):** `auto` or `wide`.

### 3.4 Maintenance Operations (`servicetype="maintenance"`)
1. **Print Head Cleaning (`<ivec:operation>Cleaning</ivec:operation>`):**
   - `<ivec:type>`:
     - `regular`: Standard cleaning
     - `deep`: Deep cleaning
     - `choke`: System Cleaning / Ink Flush (high ink consumption!)
   - `<ivec:target>`:
     - `all`: All ink groups (Black + Color)
     - `group1`: Black (`PGBK`) only
     - `group2`: Color (`C, M, Y`) only
   - `<ivec:bidirectional>OFF</ivec:bidirectional>`
2. **Mechanical Roller & Bottom Plate Cleaning (`<ivec:operation>RollerCleaning</ivec:operation>`):**
   - `<ivec:target>roller</ivec:target>` (Paper feed roller cleaning)
   - `<ivec:target>platen</ivec:target>` (Bottom plate cleaning)
3. **Diagnostic Test Prints & Head Alignment (`<ivec:operation>TestPrint</ivec:operation>`):**
   - `<ivec:type>nozzle_check</ivec:type>` (Nozzle Check Pattern, 1 sheet)
   - `<ivec:type>manual_registration</ivec:type>` (Manual Head Alignment Pattern, 2 sheets, supports column offset parameters)
   - `<ivec:type>half_auto_registration</ivec:type>` (Semi-automatic alignment, 1 sheet)
   - `<ivec:type>regi_check</ivec:type>` (Alignment verification pattern, 1 sheet)

### 3.5 Status Query & Telemetry (`<ivec:operation>GetStatus</ivec:operation>`)
- Request: `<ivec:operation>GetStatus</ivec:operation><ivec:param_set servicetype="status"/>`
- Response XML tags:
  - `<ivec:status>`: `idle`, `processing`, `stopped`, `support_error`, `operator_call`
  - `<vcn:statusdetail>`: `printing`, `cleaning`, `testing`, `waiting`, `coveropen`, `paperout`, `paperjam`
  - `<vcn:supportcode>`: 4-character code (`1000`, `1300`, `1401`, `1403`, `1470`, `1471`, `1472`, `1640`, `1686`, `1688`, `1700`, `1701`, `5100`, `5200`, `5B00`, `5B01`, `B202`..`B205`)
  - `<vcn:ink>`: Marker elements for `BK`, `C`, `M`, `Y` with status and fill state.

### 3.6 PPD Capability Obfuscation Cipher (`CNCL` Codec)
- **Encoding (`encode_cncl_string`):**
  For each byte $b \in \text{input}$:
  1. $x = b \oplus \text{0x39}$
  2. Emit two ASCII bytes: $\text{high} = \text{'a'} + ((x \gg 4) \ \& \ \text{0x0F})$, $\text{low} = \text{'a'} + (x \ \& \ \text{0x0F})$.
  3. Append `'='` (`0x3D`).
  4. Compute 32-bit unsigned sum: $S = \left(\sum b_i\right) \bmod 2^{32}$.
  5. Format $S$ as 4 big-endian bytes $[s_0, s_1, s_2, s_3]$, and for each byte $s_k$ (without XOR!) emit two ASCII bytes: $\text{'a'} + ((s_k \gg 4) \ \& \ \text{0x0F})$ and $\text{'a'} + (s_k \ \& \ \text{0x0F})$.
- **Decoding (`decode_cncl_string`):** Exact inverse, verifying the 32-bit checksum after `'='`.

### 3.7 PWG Raster (`RaS2`) & PackBits Scanline Compression
- Magic: 4 bytes `"RaS2"` (`52 61 53 32`).
- Per-page header: 1796 bytes (`0x704` bytes):
  - Bytes `0..64`: `"PwgRaster\0..."`
  - Big-endian `u32` fields at standard PWG Raster offsets:
    - `HwResolution[0..1]` at `276..284` (e.g. `600, 600` or `300, 300`)
    - `Width` (pixels) at `372..376`, `Height` (pixels) at `376..380`
    - `BitsPerColor` at `384..388` (`8`), `BitsPerPixel` at `388..392` (`24` for sRGB, `8` for SGray)
    - `BytesPerLine` at `392..396` (`Width * (BitsPerPixel / 8)`)
    - `ColorOrder` at `396..400` (`0` = chunky)
    - `ColorSpace` at `400..404` (`19` = sRGB, `18` = SGray)
    - `NumColors` at `420..424` (`3` or `1`)
- Scanline Compression:
  1. Line Repeat Count byte $R \in 0..=255$: means the following compressed line repeats $R + 1$ times ($0$ = 1 time).
  2. Pixel runs within the line (each pixel is 3 bytes for RGB or 1 byte for Gray):
     - Identical pixel run of length $N \in 1..=128$: emit control byte $(N - 1)$ followed by 1 pixel value ($3$ bytes or $1$ byte).
     - Literal distinct pixel run of length $K \in 2..=128$: emit control byte $(257 - K)$ followed by $K$ pixels.

---

## 4. Scanner Protocol Specification (MP2 over Interface 0)

### 4.1 Session Handshake
1. Send IVEC `StartJob` (`servicetype="scan"`, `jobID="00000001"`, `bidi="1"`), read XML response.
2. Send IVEC `VendorCmd` (`servicetype="scan"`, `<vcn:ijoperation>ModeShift</vcn:ijoperation>`, `<vcn:ijmode>1</vcn:ijmode>`), read XML response.
3. Interface 0 is now in binary MP2 command mode.

### 4.2 Binary MP2 Commands (Big-Endian 16-bit Opcodes)
All commands start with a 16-byte header (`[opcode_hi, opcode_lo, 0x00, param3, ...]`):
- **`0xF3 0x20` (`GetDataStatus`):** 16-byte command $\rightarrow$ 20-byte response (queries optical sensor base DPI and geometry).
- **`0xDA 0x20` (`GetCalibrationStatus`):** 16-byte command $\rightarrow$ 12-byte response (`0x06 0x06` status OK).
- **`0xDC 0x20` (`GetOPUMode`):** 16-byte command $\rightarrow$ 16-byte response (platen/cover status).
- **`0xD5 0x20` (`SetLamp`):** 16-byte command (`byte[3] = 0x01` ON, `0x00` OFF) $\rightarrow$ 8-byte response.
- **`0xD8 0x20` (`SetScanParameter`):** **72-byte (`0x48`) command** $\rightarrow$ 8-byte response:
  - `0x00..0x02`: `0xD8, 0x20`
  - `0x10..0x14`: Scan mode (`[0x02, 0x01, 0x03, 0x01]` for RGB Color, `[0x02, 0x01, 0x01, 0x01]` for Grayscale, `[0x01, 0x01, 0x01, 0x00]` for Lineart)
  - `0x18..0x1A`: `u16` big-endian `x_dpi | 0x8000` (supports `75, 100, 150, 200, 300, 400, 600, 1200` DPI)
  - `0x1A..0x1C`: `u16` big-endian `y_dpi | 0x8000`
  - `0x1C..0x20`: `u32` big-endian `x_offset` (in pixels at target DPI)
  - `0x20..0x24`: `u32` big-endian `y_offset` (in pixels at target DPI)
  - `0x24..0x28`: `u32` big-endian `width` (in pixels; max flatbed $8.5\text{"}$ = `5100` px at 600 DPI)
  - `0x28..0x2C`: `u32` big-endian `height` (in pixels; max flatbed $11.7\text{"}$ = `7016` px at 600 DPI)
  - `0x2C`: Channel bit depth (`0x08` for 8-bit per channel, `0x04` for grayscale mode flag)
  - `0x2D`: Total bits per pixel (`0x18` = 24-bit RGB, `0x08` = 8-bit Gray)
  - `0x31..0x33`: Gamma code (`0x81, 0x00` standard sRGB gamma 2.2)
- **`0xD9 0x20` (`StartScan`):** 16-byte command (`byte[3] = 0x01`) $\rightarrow$ 8-byte response.
- **`0xD4 0x20` (`ReadScanChunk`):** 16-byte command $\rightarrow$ variable response:
  - Bytes `0x00..0x02`: `0x06, 0x06` (ACK)
  - Status flags: `0x20` = page end, `0x28` or `0x38` = scan complete
  - Bytes `0x0C..0x10`: `u32` big-endian payload byte length $L$
  - Bytes `0x10..0x10+L`: Raw image scanline bytes.
- **`0xEF 0x20` (`CancelOrFinishScan`):** 16-byte command $\rightarrow$ 8-byte response, followed by IVEC `<ivec:operation>EndJob</ivec:operation>` (`servicetype="scan"`).

### 4.3 CIS Sensor Line-Stagger Compensation
On the G2010 Contact Image Sensor (CIS), the R, G, and B photodiode arrays are physically spaced apart along the Y scan axis:
- At 75/150/300 DPI: offset is 0 lines (corrected in ASIC).
- At 600 DPI: Green is delayed by $+2$ lines, Blue by $+4$ lines relative to Red.
- At 1200 DPI: Green is delayed by $+4$ lines, Blue by $+8$ lines relative to Red.
The host `LineStaggerBuffer` ring buffer aligns `(R[y], G[y + dy_g], B[y + dy_b])` before outputting RGB pixels.

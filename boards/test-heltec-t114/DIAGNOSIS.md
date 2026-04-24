# Heltec T114 USB Flash + Display Diagnostics

## Scope

This document captures what was tested for `boards/test-heltec-t114`, what was observed, and what that means.

## Environment and board facts

- Board identifies as:
  - `Model: HT-n5262`
  - `Board-ID: HT-n5262`
  - `UF2 Bootloader 0.9.0 ...`
- `INFO_UF2.TXT` reports:
  - `SoftDevice: not found`
- Device `CURRENT.UF2` metadata:
  - `Family ID: 0x239a0071`
  - `Target Address: 0x00001000`

## Key findings (confirmed)

1. Earlier UF2 images were using the wrong family/address.
   - Old files targeted `0xADA52840` / `0x26000`.
   - This did not produce reliable runtime changes on this device.

2. Correct UF2 profile for this board is:
   - Family: `0x239A0071`
   - App base: `0x1000`
   - Linker updated to:
     - `FLASH ORIGIN = 0x00001000`
     - `RAM ORIGIN = 0x20000000`

3. USB flashing is now confirmed working.
   - Verified by comparing `CURRENT.UF2` hashes before/after flashing.
   - Example observed by user:
     - `current_before`: `8fc346...`
     - `current_after`: `6bf68e...`
     - target test UF2 hash differed, confirming actual write/update activity.

4. Runtime issue remains in display bring-up path.
   - User still sees persistent board LED patterns and no display content.
   - This is no longer a UF2 conversion/addressing problem.

5. Marker-based proof confirms flashed image content on device.
   - Added unique markers in firmware images and verified they appear in `CURRENT.UF2`:
     - `T114_MARKER_2026_04_18_A` (main display test binary)
     - `T114_STAGE0_MARKER_2026_04_18` (minimal stage0 runtime binary)
   - This confirms image replacement on flash is happening.

6. Stage0 runtime LED behavior does not match expected timing pattern.
   - Stage0 expected pattern on `P1.03`:
     - `ON 3s -> OFF 1s -> ON 200ms -> OFF 200ms -> ON 200ms -> OFF 3s`
   - Observed by user:
     - brief red/green pulse, about ~1s off, repeating.
   - Interpretation:
     - Either the visually observed LED path is not `P1.03` on this board revision, or
     - firmware is not reaching/remaining in the intended delay loop (reset/restart loop).

7. Busy-wait runtime stage confirms execution on-device.
   - Added `stage0_busy` marker:
     - `T114_STAGE0_BUSY_MARKER_2026_04_18`
   - Programmed busy-wait pattern on `P1.03`:
     - `ON 2.5s -> OFF 2.5s -> ON 150ms -> OFF 150ms -> ON 150ms -> OFF 1.5s`
   - User observed:
     - `long OFF -> long ON -> short OFF -> short ON -> short OFF -> long ON -> long OFF`
   - Interpretation:
     - This matches the programmed sequence with inverted visible polarity.
     - Conclusion: firmware is executing; observed green LED path behaves active-low.

## Methods used

## A) Artifact validation

- ELF section inspection:
  - `llvm-objdump -h ...`
- HEX conversion:
  - `cargo objcopy ... -O ihex`
- UF2 conversion:
  - `python3 scripts/uf2conv.py ... --family ... --convert`
- UF2 metadata check:
  - `python3 scripts/uf2conv.py <file.uf2> --info`

## B) On-device verification

- Capture device image:
  - `cp /Volumes/HT-n5262/CURRENT.UF2 /tmp/current_*.uf2`
- Hash compare:
  - `shasum -a 256 /tmp/current_before.uf2 /tmp/current_after.uf2 /tmp/test-...uf2`
- Write method:
  - `dd if=... of=/Volumes/HT-n5262/firmware.uf2 bs=4096 conv=fsync`
  - `sync`
- Marker check:
  - `strings -a /tmp/current_after.uf2 | rg T114_...MARKER...`

## C) Firmware instrumentation

- Added minimal proof binary (`proof`) with distinct LED pattern.
- Added minimal stage0 binary (`stage0`) with unique marker and long/short timing pattern.
- Added minimal busy-wait stage (`stage0_busy`) with unique marker and timer-independent delays.
- Added staged checkpoints and error patterns in `main.rs`.
- Added display init error coding and reduced SPI speed.
- Switched panic strategy from `panic-probe` to `panic-halt` to avoid confusing panic signaling.
- Tested display on multiple SPI peripherals:
  - `SPI2`, `SPI3`, and `TWISPI1 (SPIM1)`
- Added backlight probe sequence on `P0.15` (active-low off/on cycles).

## Current state in repository

- Board test crate exists at:
  - `boards/test-heltec-t114`
- Current build/UF2 output path:
  - `/tmp/test-heltec-t114-main-1000.uf2`
- Current UF2 format:
  - family `0x239A0071`
  - target `0x1000`

## Working hypothesis now

- Flashing works.
- Flashed image replacement is confirmed by `CURRENT.UF2` hashes and embedded marker strings.
- Runtime execution is confirmed by stage0 busy-wait pattern correspondence.
- Remaining fault is in hardware-specific display control path:
  - panel control pins/polarity/timing and/or actual SPI instance for this exact board revision.
- LED observability is resolved enough for diagnostics:
  - visible green LED is controllable and appears active-low.

## Next recommended steps (deterministic)

1. Proceed with TFT-only troubleshooting from confirmed runtime baseline.
2. If no visible backlight modulation:
   - sweep `BL` and `VTFT_CTRL` pin/polarity combinations with LED-coded case IDs.
3. If backlight modulation works but no pixels:
   - sweep SPI instance + ST7789 init profile variants (MADCTL/COLMOD/init table) with LED-coded case IDs.

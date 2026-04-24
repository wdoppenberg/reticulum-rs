# Deploying reticulum-node on the Heltec T114

This guide covers everything needed to build and flash the `heltec-t114`
firmware on the **Heltec T114** (nRF52840 + SX1262 LoRa).

The primary flashing method is **UF2 over USB-C** — no debug probe required.
probe-rs over SWD is documented separately as an advanced option.

---

## Hardware required

| Item         | Notes                                                         |
|--------------|---------------------------------------------------------------|
| Heltec T114  | nRF52840 + SX1262, integrated LoRa antenna connector          |
| USB-C cable  | Data cable required (charge-only cables will not work)        |
| LoRa antenna | 868 MHz (EU) or 915 MHz (US/AUS) — **do not TX without one** |

---

## 1. Install toolchain prerequisites

### Rust target

```sh
rustup target add thumbv7em-none-eabihf
```

The nRF52840 is a Cortex-M4F.  The `thumbv7em-none-eabihf` target produces
Thumb-2 code with hardware floating-point, which is what `embassy-nrf` expects.

### cargo-binutils + llvm-tools

Used to convert the ELF to Intel HEX for UF2 packaging.  Uses the LLVM tools
already bundled with the Rust toolchain — no separate ARM toolchain needed.

```sh
rustup component add llvm-tools
cargo install cargo-binutils
```

### uf2conv.py

`uf2conv.py` is a single-file Python 3 script from the Microsoft UF2
repository.  Download it once and keep it somewhere on your `PATH`:

```sh
curl -O https://raw.githubusercontent.com/microsoft/uf2/master/utils/uf2conv.py
chmod +x uf2conv.py
# optional: mv uf2conv.py /usr/local/bin/uf2conv.py
```

### flip-link (required)

`flip-link` is set as the linker in `.cargo/config.toml` and must be installed:

```sh
cargo install flip-link
```

---

## 2. Enter DFU mode

**Double-tap the RESET button within ~0.5 seconds.**  A USB mass-storage
device named **`HT-n5262`** appears on your computer.  If you miss the timing
the board boots normally — just try again.

> **If the board previously had Meshtastic firmware:** the Meshtastic partition
> layout can prevent a custom UF2 from booting correctly.  Run a factory erase
> first by downloading
> [`nrf_erase2.uf2`](https://github.com/meshtastic/nrf52_factory_erase) and
> dragging it onto the `HT-n5262` drive.  The board reboots; double-tap RESET
> again before continuing.

---

## 3. Build the binary

The default `memory.x` is configured for UF2 flashing — the firmware starts
at `0x26000`, matching the Adafruit UF2 bootloader's hardcoded application
entry point.

Build from within the board directory (`.cargo/config.toml` sets the target
automatically, so `--target` can be omitted):

```sh
cd boards/heltec-t114

# Release (recommended for flashing)
cargo build --release

# Debug (larger binary, richer logs)
cargo build
```

ELF lands at `target/thumbv7em-none-eabihf/release/heltec-t114` (or `debug/`)
relative to the workspace root.

---

## 4. Flash via UF2

### Step 1 — Convert ELF → HEX → UF2

Run these from the **workspace root** (`reticulum-rs/`).  Using Intel HEX
format means the flash addresses are read directly from the ELF — no `--base`
flag needed and no risk of an address mismatch with `memory.x`.

```sh
cd boards/heltec-t114
cargo objcopy --release -- -O ihex /tmp/heltec-t114.hex

cd ../..
python3 uf2conv.py /tmp/heltec-t114.hex \
  --family 0xADA52840 \
  --convert \
  --output /tmp/heltec-t114.uf2
```

### Step 2 — Copy the UF2 to the device

Make sure the board is in DFU mode (section 2) before copying.

```sh
# macOS — use cat to avoid extended-attribute errors from cp
cat heltec-t114.uf2 > /Volumes/HT-n5262/heltec-t114.uf2

# Linux
cp heltec-t114.uf2 /media/$USER/HT-n5262/
```

The board reboots automatically once it receives the complete file and the
`HT-n5262` drive disappears.

> **macOS — do not use Finder drag-and-drop or `cp`:** both try to write
> extended attributes and resource forks to the FAT volume, causing
> "Error code -36" or "could not copy extended attributes" errors that may
> result in an incomplete flash.  Use `cat >` as shown above.
>
> If `HT-n5262` does not appear in `/Volumes/`, check Finder's sidebar under
> Locations, or run `diskutil list` to find the mount point.
>
> After the board reboots the volume disappears and macOS shows a
> **"Disk Not Ejected Properly"** notification — this is expected and harmless.

---

## 5. Erase flash (reset identity)

To reset the stored node identity so a new one is generated on the next boot,
erase only the identity page via DFU mode:

1. Enter DFU mode (double-tap RESET).
2. Download [`nrf_erase2.uf2`](https://github.com/meshtastic/nrf52_factory_erase)
   and copy it to `HT-n5262`.  The board reboots with a clean flash.
3. Re-flash the firmware (section 4).

---

## 6. LoRa frequency / radio configuration

The binary uses `LoraConfig::eu_868()` by default (868 MHz, SF7, BW125,
CR4/5, 14 dBm TX).  To change region, edit `src/main.rs`:

```rust
// EU 868 MHz (default)
let lora_config = LoraConfig::eu_868();

// Australia / New Zealand / US 915 MHz
let lora_config = LoraConfig::aus_nz();

// Asia 433 MHz
let lora_config = LoraConfig::asia_433();

// Custom — override individual fields
let lora_config = LoraConfig {
    frequency_hz: 869_525_000,
    tx_power_dbm: 20,
    ..LoraConfig::eu_868()
};
```

**Always attach a matching-frequency LoRa antenna before enabling TX.**
Operating the SX1262 without an antenna risks damaging the RF front-end.

---

## 7. Pin assignments

| Signal     | nRF52840 pin | Direction | Notes                     |
|------------|--------------|-----------|---------------------------|
| SPI SCK    | P0.19        | Output    | SX1262 SPI clock          |
| SPI MOSI   | P0.22        | Output    | SX1262 SPI data in        |
| SPI MISO   | P0.23        | Input     | SX1262 SPI data out       |
| NSS (CS)   | P0.24        | Output    | Active-low chip select    |
| RESET      | P0.25        | Output    | Active-low hardware reset |
| BUSY       | P0.17        | Input     | High while radio is busy  |
| DIO1       | P0.20        | Input     | IRQ / RxDone / TxDone     |
| ANT_RX_SW  | P0.13        | Output    | RF switch RX path (high)  |
| ANT_TX_SW  | P0.14        | Output    | RF switch TX path (high)  |

To change a pin, edit the type aliases at the top of `src/main.rs`:

```rust
type PinDio1 = peripherals::P0_20;  // ← change to your pin
```

---

## 8. Flash layout

| Region      | Start address  | Size    | Contents                                         |
|-------------|----------------|---------|--------------------------------------------------|
| MBR         | `0x0000_0000`  | 4 KB    | Nordic Master Boot Record                        |
| SD reserved | `0x0000_1000`  | ~148 KB | Reserved for S140 v6 SoftDevice (unused/empty)   |
| Firmware    | `0x0002_6000`  | 820 KB  | Binary (grows upward)                            |
| Identity    | `0x000F_3000`  | 4 KB    | Node identity record (stored by firmware)        |
| Bootloader  | `0x000F_4000`  | ~40 KB  | Adafruit UF2 bootloader                          |
| Settings    | `0x000F_F000`  | 4 KB    | Bootloader settings (do not write from firmware) |

The T114's Adafruit UF2 bootloader is compiled against S140 v6.1.1 and
unconditionally jumps to `0x26000` as the application entry point, even when
no SoftDevice is present.  The identity page shares the last 4 KB with the
bootloader settings region and is never touched by a normal firmware flash.

---

## 9. Troubleshooting

| Symptom | Likely cause | Fix |
|---------|-------------|-----|
| `HT-n5262` drive does not appear | Double-tap timing too slow/fast | Try again; the window is ~0.5 s |
| Board reboots but firmware does not start | Meshtastic partition layout conflict | Run factory erase (section 5) then re-flash |
| `LoRa init` panic at startup | SPI wiring issue or SX1262 not powered | Check P0.17 BUSY line; verify SPI connections |
| No packets received / transmitted | Wrong frequency or missing antenna | Verify `LoraConfig` region preset and attach a resonant antenna |

---

## Quick reference

```sh
# 1. Install prerequisites (once)
rustup target add thumbv7em-none-eabihf
rustup component add llvm-tools
cargo install flip-link cargo-binutils
curl -O https://raw.githubusercontent.com/microsoft/uf2/master/utils/uf2conv.py

# 2. Build (from the board directory)
cd boards/heltec-t114
cargo build --release

# 3. Convert to UF2 (from workspace root)
cd ../..
cd boards/heltec-t114
cargo objcopy --release -- -O ihex /tmp/heltec-t114.hex
cd ../..
python3 uf2conv.py /tmp/heltec-t114.hex \
  --family 0xADA52840 --convert --output /tmp/heltec-t114.uf2

# 4. Flash (double-tap RESET first, then copy when HT-n5262 appears)
cat /tmp/heltec-t114.uf2 > /Volumes/HT-n5262/heltec-t114.uf2   # macOS
```

---

## Advanced: flashing with probe-rs (SWD debug probe)

probe-rs gives you flash + live RTT logs in one step, but requires a hardware
debug probe wired to the T114's SWD header.

> **Important:** probe-rs flashes from address `0x0`, overwriting the Nordic
> MBR and disabling the UF2 bootloader.  After using probe-rs you can no longer
> use the USB DFU method unless you restore the bootloader.  Only use this path
> if you have a probe permanently available.

### Additional hardware

| Item        | Notes                                                                |
|-------------|----------------------------------------------------------------------|
| Debug probe | J-Link, CMSIS-DAP, or compatible (e.g. nRF52840-DK acts as a J-Link)|
| SWD cable   | 10-pin or 6-pin TagConnect / dupont depending on your probe          |

### Install probe-rs

```sh
cargo install probe-rs-tools --locked
```

> **Linux udev rules** — if `probe-rs list` shows no probes:
> ```sh
> probe-rs complete install-udev-rules
> ```

### Update memory.x for probe-rs

Change `boards/heltec-t114/memory.x` to start flash at `0x0` (probe-rs flashes
the full address space directly, bypassing the bootloader entirely):

```
MEMORY
{
  FLASH : ORIGIN = 0x00000000, LENGTH = 1024K
  RAM   : ORIGIN = 0x20000000, LENGTH = 256K
}
```

### Connect the debug probe

| Probe pin | T114 pad |
|-----------|----------|
| SWDIO     | SWDIO    |
| SWDCLK    | SWDCLK   |
| GND       | GND      |
| VCC (opt) | 3V3      |

Confirm probe-rs can see the chip:

```sh
probe-rs list
probe-rs chip info nRF52840_xxAA
```

### Flash + attach RTT logs

```sh
cd boards/heltec-t114
cargo build --release

probe-rs run \
  --chip nRF52840_xxAA \
  ../../target/thumbv7em-none-eabihf/release/heltec-t114
```

### Flash only (no RTT)

```sh
probe-rs download \
  --chip nRF52840_xxAA \
  --verify \
  target/thumbv7em-none-eabihf/release/heltec-t114

probe-rs reset --chip nRF52840_xxAA
```

### Log levels

```toml
# .cargo/config.toml
[env]
DEFMT_LOG = "debug"
```

Override at build time:

```sh
cd boards/heltec-t114
DEFMT_LOG=info cargo build --release
```

Valid levels: `error`, `warn`, `info`, `debug`, `trace`.

### Erase flash via probe-rs

```sh
# Erase everything (also wipes bootloader and identity)
probe-rs erase --chip nRF52840_xxAA --chip-erase

# Erase only the identity page
probe-rs erase --chip nRF52840_xxAA --sector 0x000FF000
```

# Deploying reticulum-node on the Heltec T114

This guide covers everything needed to build, flash, and monitor a
`reticulum-node` RNode binary on the **Heltec T114** (nRF52840 + SX1262 LoRa)
using `probe-rs`.

---

## Hardware required

| Item         | Notes                                                                |
|--------------|----------------------------------------------------------------------|
| Heltec T114  | nRF52840 + SX1262, integrated LoRa antenna connector                 |
| Debug probe  | J-Link, CMSIS-DAP, or compatible (e.g. nRF52840-DK acts as a J-Link) |
| SWD cable    | 10-pin or 6-pin TagConnect / dupont depending on your probe          |
| LoRa antenna | 868 MHz (EU) or 915 MHz (US/AUS) — **do not TX without one**         |
| USB-C cable  | Power only; not used for flashing                                    |

The T114 exposes a 4-pin SWD header (SWDIO, SWDCLK, GND, VCC) near the
USB-C connector.  Consult the Heltec schematic for the exact pad locations.

---

## 1. Install toolchain prerequisites

### Rust target

```sh
rustup target add thumbv7em-none-eabihf
```

The nRF52840 is a Cortex-M4F.  The `thumbv7em-none-eabihf` target produces
Thumb-2 code with hardware floating-point, which is what `embassy-nrf` expects.

### probe-rs

```sh
cargo install probe-rs-tools --locked
```

Verify the installation:

```sh
probe-rs --version
# probe-rs 0.31.0 (or later)
```

> **Linux udev rules** — if `probe-rs list` shows no probes, install the udev
> rules bundled with probe-rs:
>
> ```sh
> probe-rs complete install-udev-rules
> ```
>
> Then re-plug the debug probe and run `probe-rs list` again.

### flip-link (stack overflow protection — recommended)

```sh
cargo install flip-link
```

Add to `.cargo/config.toml` if you want overflow detection:

```toml
[target.thumbv7em-none-eabihf]
linker = "flip-link"
```

---

## 2. Connect the debug probe

Attach the probe to the T114 SWD header:

| Probe pin | T114 pad |
|-----------|----------|
| SWDIO     | SWDIO    |
| SWDCLK    | SWDCLK   |
| GND       | GND      |
| VCC (opt) | 3V3      |

Confirm probe-rs can see the chip:

```sh
probe-rs list
# [0]: J-Link (...)

probe-rs chip info nRF52840_xxAA
# nRF52840_xxAA
# Cores (1):
#     - main (Armv7em)
# NVM: 0x00000000..0x00100000 (1.0 MiB)
# RAM: 0x20000000..0x20040000 (256.0 KiB)
```

---

## 3. Build the binary

All commands below are run from the **workspace root**
(`reticulum-rs/`) unless stated otherwise.

The `.cargo/config.toml` inside `crates/reticulum-node/` sets the default
target to `thumbv7em-none-eabihf`, so `--target` can be omitted when you
`cd` into the crate first.

### Release build (recommended for flashing)

```sh
cargo build \
  --release \
  --package reticulum-node \
  --bin nrf52840 \
  --features reticulum-node/target-nrf52840 \
  --target thumbv7em-none-eabihf
```

The ELF lands at:

```
target/thumbv7em-none-eabihf/release/nrf52840
```

### Debug build (for development, larger binary, richer logs)

```sh
cargo build \
  --package reticulum-node \
  --bin nrf52840 \
  --features reticulum-node/target-nrf52840 \
  --target thumbv7em-none-eabihf
```

ELF at `target/thumbv7em-none-eabihf/debug/nrf52840`.

> **Tip:** If you `cd crates/reticulum-node` first, the `.cargo/config.toml`
> sets the default target automatically, so you can drop `--target` and shorten
> `--package`:
>
> ```sh
> cd crates/reticulum-node
> cargo build --release --bin nrf52840 --features target-nrf52840
> ```

---

## 4. Flash the binary

### Option A — `probe-rs run` (flash + attach RTT log in one step)

This is the recommended workflow during development.  `probe-rs run` flashes
the binary, resets the chip, and immediately streams defmt logs over RTT.

```sh
probe-rs run \
  --chip nRF52840_xxAA \
  target/thumbv7em-none-eabihf/release/nrf52840
```

You should see output like:

```
INFO  reticulum-node starting on nRF52840 / Heltec T114
INFO  storage: no identity in flash — generating new one
INFO  storage: identity stored to flash @ 0x000FF000
INFO  node address: AddressHash(ab:cd:ef:...)
DEBUG iface <addr>: driver started
```

Subsequent boots load the stored identity from flash:

```
INFO  reticulum-node starting on nRF52840 / Heltec T114
DEBUG storage: loaded identity from flash @ 0x000FF000
INFO  node address: AddressHash(ab:cd:ef:...)   ← same address as before
DEBUG iface <addr>: driver started
```

Press `Ctrl-C` to detach from RTT without resetting the device.

### Option B — `probe-rs download` (flash only, no RTT)

Useful in CI or when you want to flash and walk away.

```sh
probe-rs download \
  --chip nRF52840_xxAA \
  --verify \
  target/thumbv7em-none-eabihf/release/nrf52840
```

`--verify` re-reads the flash after writing and compares it to the ELF
contents.  Omit it to save a few seconds on large binaries.

Reset the chip after download:

```sh
probe-rs reset --chip nRF52840_xxAA
```

### Option C — `cargo run` (shortcut via `.cargo/config.toml`)

From inside `crates/reticulum-node/`, the `runner` key in `.cargo/config.toml`
wires `cargo run` directly to `probe-rs run`:

```sh
cd crates/reticulum-node
cargo run --release --bin nrf52840 --features target-nrf52840
```

This is equivalent to Option A but saves typing.

---

## 5. Erase flash (reset identity or recover a bricked board)

To erase all flash — including the stored identity — so the node generates a
new address on the next boot:

```sh
probe-rs erase --chip nRF52840_xxAA --chip-erase
```

> **Warning:** this also wipes the firmware.  Re-flash after erasing.

To erase only the identity page without touching the firmware, erase the
4 KB sector at `0x000FF000`:

```sh
probe-rs erase \
  --chip nRF52840_xxAA \
  --sector 0x000FF000
```

---

## 6. Attach to a running node (RTT only)

If the node is already running and you want to read its logs without
interrupting it:

```sh
probe-rs attach --chip nRF52840_xxAA
```

---

## 7. Log levels

The default log level is set in `.cargo/config.toml`:

```toml
[env]
DEFMT_LOG = "debug"
```

Override at build time:

```sh
DEFMT_LOG=info cargo run --release --bin nrf52840 --features target-nrf52840
```

Valid levels: `error`, `warn`, `info`, `debug`, `trace`.

---

## 8. LoRa frequency / radio configuration

The binary uses `LoraConfig::eu_868()` by default (868 MHz, SF7, BW125,
CR4/5, 14 dBm TX).  To change region, edit `src/bin/nrf52840.rs`:

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

## 9. Pin assignments

The binary configures these nRF52840 pins by default.  Verify against the
Heltec T114 schematic before flashing; pin silk-screen labels may differ from
the nRF pad numbers.

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

To change a pin, edit the type aliases at the top of `src/bin/nrf52840.rs`:

```rust
type PinDio1 = peripherals::P0_20;  // ← change to your pin
```

---

## 10. Flash layout

| Region | Start address | Size | Contents |
|--------|---------------|------|----------|
| Firmware | `0x0000_0000` | ~1 MB | Binary (grows upward) |
| Identity | `0x000F_F000` | 4 KB (1 page) | 68-byte identity record + `0xFF` padding |

The identity page is the **last 4 KB page** of the 1 MB internal flash.
It is never touched by a normal firmware flash (the linker script places
code from address 0 upward, well below 0xFF000 for a typical Reticulum
binary).

If you move `IDENTITY_FLASH_OFFSET` in `src/bin/nrf52840.rs`, keep it
aligned to a 4 KB boundary (the nRF52840's minimum erase granularity).

---

## 11. Troubleshooting

| Symptom | Likely cause | Fix |
|---------|-------------|-----|
| `probe-rs list` shows no probes | USB or driver issue | Check cable; install udev rules (Linux); try a different USB port |
| `Error: The debug probe is not supported` | Probe firmware too old | Update J-Link firmware or use a CMSIS-DAP probe |
| `Error: Failed to write to address 0x000FF000` | Identity page not erased before write | The driver calls `erase()` before `write()`; check that `ERASE_SIZE` alignment is correct |
| Node generates a new address on every boot | Identity page being erased by `probe-rs erase --chip-erase` | Use `--sector` erase if you only want to wipe the firmware |
| `LoRa init` panic at startup | SPI wiring issue or SX1262 not powered | Check P0.17 BUSY line; verify SPI connections with a logic analyser |
| No packets received / transmitted | Wrong frequency or missing antenna | Verify `LoraConfig` region preset and attach a resonant antenna |
| defmt output garbled | `DEFMT_LOG` mismatch between build and probe-rs version | Rebuild with the same probe-rs version used at runtime |

---

## Quick reference

```sh
# 1. Install prerequisites (once)
rustup target add thumbv7em-none-eabihf
cargo install probe-rs-tools --locked

# 2. Build
cargo build --release \
  --package reticulum-node \
  --bin nrf52840 \
  --features reticulum-node/target-nrf52840 \
  --target thumbv7em-none-eabihf

# 3. Flash + attach logs
probe-rs run \
  --chip nRF52840_xxAA \
  target/thumbv7em-none-eabihf/release/nrf52840

# 4. Flash only (no logs)
probe-rs download --chip nRF52840_xxAA --verify \
  target/thumbv7em-none-eabihf/release/nrf52840

# 5. Erase all flash (resets identity)
probe-rs erase --chip nRF52840_xxAA --chip-erase

# 6. Attach to running node logs
probe-rs attach --chip nRF52840_xxAA
```

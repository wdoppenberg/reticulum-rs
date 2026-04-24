# test-heltec-t114

Minimal Embassy USB-flash validation firmware for Heltec T114:

- LED heartbeat on `GPIO35` = `P1.03`
- ST7789 screen debug text rendered via `mousefood + ratatui`

Display wiring used (from `boards/heltec-t114/src/ui.rs`):

- `SCK  -> P1.08`
- `MOSI -> P1.09`
- `CS   -> P0.11`
- `DC   -> P0.12`
- `RST  -> P0.02`
- `VDD  -> P0.03` (panel power-enable)
- `BL   -> P0.15`

## Build

From workspace root:

```sh
cargo build -p test-heltec-t114 --release --target thumbv7em-none-eabihf
```

## Convert to UF2

```sh
cargo objcopy -p test-heltec-t114 --release --target thumbv7em-none-eabihf -- -O ihex /tmp/test-heltec-t114.hex
python3 scripts/uf2conv.py /tmp/test-heltec-t114.hex --family 0xADA52840 --convert --output /tmp/test-heltec-t114.uf2
```

## Flash over USB

1. Double-tap reset so the board appears as `HT-n5262`.
2. Copy `/tmp/test-heltec-t114.uf2` onto that drive.

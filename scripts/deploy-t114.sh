#!/bin/bash
set -e

# cargo clean
cargo build --release --target thumbv7em-none-eabihf -p heltec-t114

LLVM_OBJCOPY=$(find "$(rustc --print sysroot)" -name 'llvm-objcopy' -type f | head -n1)

"$LLVM_OBJCOPY" -O binary \
    target/thumbv7em-none-eabihf/release/heltec-t114 \
    target/firmware.bin

uv run scripts/uf2conv.py target/firmware.bin \
    --convert --base 0x1000 --family 0xADA52840 \
    --output target/firmware.uf2

# Sanity check before flashing
xxd target/firmware.bin | head -1
# Expect: 0000 0420 xxxx 0100  (SP = 0x20040000, reset vec in 0x0001xxxx range)

dd if=./target/firmware.uf2  of=/Volumes/HT-n5262/firmware.uf2 bs=4096 conv=fsync && sync

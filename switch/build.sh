#!/bin/sh
# usage: switch/build.sh <cargo subcommand> [args]   (e.g. build --profile play -p launcher)
set -e
export DEVKITPRO=$HOME/devkitpro PATH=$HOME/devkitpro/devkitA64/bin:$HOME/devkitpro/tools/bin:$HOME/.cargo/bin:$PATH
ROOT=$(cd "$(dirname "$0")/.." && pwd)
export CARGO_TARGET_DIR=$ROOT/target/nx
export RUSTFLAGS="-Clink-arg=-specs=$DEVKITPRO/libnx/switch.specs --cfg getrandom_backend=\"custom\""
# C deps built by the cc crate (blake3, ...): devkitA64 gcc, libnx headers.
export CC_aarch64_nintendo_switch_newlib=aarch64-none-elf-gcc CXX_aarch64_nintendo_switch_newlib=aarch64-none-elf-g++ AR_aarch64_nintendo_switch_newlib=aarch64-none-elf-gcc-ar
export CFLAGS_aarch64_nintendo_switch_newlib="-march=armv8-a+crc+crypto -mtune=cortex-a57 -mtp=soft -fPIE -D__SWITCH__ -I$DEVKITPRO/libnx/include -I$DEVKITPRO/portlibs/switch/include"
export CXXFLAGS_aarch64_nintendo_switch_newlib="$CFLAGS_aarch64_nintendo_switch_newlib"

# build-std resolves std from library/Cargo.lock and ignores the workspace
# [patch], so std would get upstream libc (3DS stat layout). Build std from a
# copy of library/ whose own [patch] points libc at the fork. Re-made per
# nightly commit.
LIBC_GIT=https://github.com/kyokoxdev/libc
LIBC_REV=90345107c821d02f17face49295cc4b362176338
SYSLIB=$(rustc +nightly --print sysroot)/lib/rustlib/src/rust/library
STD=$ROOT/target/nx-std-src/library
STAMP="$(rustc +nightly -vV | sed -n 's/^commit-hash: //p') $LIBC_REV"
if [ "$(cat "$STD/.iw4l-stamp" 2>/dev/null)" != "$STAMP" ]; then
    rm -rf "$STD" && mkdir -p "$(dirname "$STD")" && cp -r "$SYSLIB" "$STD"
    sed -i "/^\[patch.crates-io\]/a libc = { git = '$LIBC_GIT', rev = '$LIBC_REV' }" "$STD/Cargo.toml"
    # Lock entry: registry source + checksum -> git source.
    sed -i "/^name = \"libc\"$/{n;n;s|^source = .*|source = \"git+$LIBC_GIT?rev=$LIBC_REV#$LIBC_REV\"|;n;/^checksum/d}" "$STD/Cargo.lock"
    echo "$STAMP" > "$STD/.iw4l-stamp"
fi
export __CARGO_TESTS_ONLY_SRC_ROOT=$STD

cd "$ROOT"
exec cargo +nightly "$@" -Zjson-target-spec -Zbuild-std=std,panic_abort --target $ROOT/targets/aarch64-nintendo-switch-newlib.json

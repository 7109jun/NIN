# Source me: toolchain env for NIN Shell (Rust 1.84 from apt + patched std for windows-gnu)
export RUSTC=/usr/bin/rustc-1.84
export RUSTC_BOOTSTRAP=1
[ -x /usr/bin/rustdoc-1.84 ] && export RUSTDOC=/usr/bin/rustdoc-1.84
export __CARGO_TESTS_ONLY_SRC_ROOT=/home/claude/nin-toolchain/rustsrc/library
export WINEDEBUG=-all
export WINEPREFIX=${WINEPREFIX:-/tmp/claude-0/wineprefix}
export WINEARCH=win64
export WINE=/usr/lib/wine/wine64
NIN_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# rsbegin.o/rsend.o stubs must sit in the cwd of the linker (workspace root)
[ -f "$NIN_ROOT/rsbegin.o" ] || cp /home/claude/nin-toolchain/rt/*.o "$NIN_ROOT/"
# host (Linux) tests:   cargo test -p <crate>
# Windows build:        scripts/build-win.sh [cargo args]

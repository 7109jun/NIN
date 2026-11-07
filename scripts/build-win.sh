#!/usr/bin/env bash
# Cross-compile for Windows x64 (GNU). Usage: scripts/build-win.sh [-p crate] [extra cargo args]
set -e
source "$(dirname "$0")/env.sh"
cd "$NIN_ROOT"
exec cargo build --release --target x86_64-pc-windows-gnu -Zbuild-std=std,panic_abort "$@"

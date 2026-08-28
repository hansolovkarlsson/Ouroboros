#!/bin/sh
# Rebuild third_party/picolibc-prebuilt (the committed static libc.a + headers the
# picolibc port links against). You do NOT need to run this to build Ouroboros -
# the prebuilt is committed. Run it only to regenerate/upgrade picolibc.
#
# Requires meson + ninja (`brew install meson ninja`) and clang. The build uses
# the Rust toolchain's LLVM binutils (llvm-ar/as/nm) via libc/picolibc-cross.txt,
# so no separate cross-toolchain is needed. picolibc is built:
#   -fPIC                 -> self-relocating (R_AARCH64_RELATIVE only, no ABS64),
#                            so it loads under our PIE loader unchanged.
#   -Dposix-console=true  -> stdin/stdout/stderr wired to fd 0/1/2, i.e. to the
#                            write()/read() stubs in libc/src/file.c (our porting
#                            layer). picolibc's stdio bottoms out at those.
#   -Dnewlib-global-errno -> a single global errno (no TLS; we have no threads).
#   -Dthread-local-storage=false, -Dpicocrt=false (we use our own crt0),
#   -Dsemihost=false, -Dmultilib=false, -Dtests=false, -Dspecsdir=none.
# See docs/processes.md "Writing a program in C" and the libc-arc postmortem.
set -e

VERSION="${PICOLIBC_VERSION:-1.8.9}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TP="$ROOT/third_party"
SRC="$TP/picolibc"
BUILD="$TP/pico-build"
INSTALL="$TP/pico-install"
PREBUILT="$TP/picolibc-prebuilt"
CROSS_TMPL="$ROOT/libc/picolibc-cross.txt"
LLVM_BIN="$(rustc --print sysroot)/lib/rustlib/$(rustc -vV | sed -n 's/host: //p')/bin"
STRIP="$LLVM_BIN/llvm-strip"

# Resolve the committed cross-file TEMPLATE (@LLVM_BIN@ -> this host's toolchain)
# into a real file meson can read; kept in the gitignored build area.
mkdir -p "$TP"
CROSS="$TP/picolibc-cross.resolved.txt"
sed "s#@LLVM_BIN@#$LLVM_BIN#g" "$CROSS_TMPL" > "$CROSS"

if [ ! -d "$SRC" ]; then
    echo "cloning picolibc $VERSION ..."
    git clone --depth 1 --branch "$VERSION" https://github.com/picolibc/picolibc.git "$SRC"
fi

rm -rf "$BUILD" "$INSTALL"
( cd "$SRC" && meson setup "$BUILD" --cross-file "$CROSS" \
    -Dmultilib=false -Dpicocrt=false -Dsemihost=false -Dtests=false \
    -Dthread-local-storage=false -Dnewlib-global-errno=true -Dspecsdir=none \
    -Dposix-console=true -Dprefix="$INSTALL" )
ninja -C "$BUILD"
ninja -C "$BUILD" install

# Commit only the stripped lib + headers (the 4.7MB debug lib strips to ~1.2MB).
rm -rf "$PREBUILT"
mkdir -p "$PREBUILT/lib"
cp "$INSTALL/lib/libc.a" "$PREBUILT/lib/libc.a"
"$STRIP" --strip-debug "$PREBUILT/lib/libc.a"
cp -R "$INSTALL/include" "$PREBUILT/include"
echo "$VERSION" > "$PREBUILT/VERSION"
echo "picolibc-prebuilt regenerated ($VERSION), $(du -sh "$PREBUILT" | cut -f1)"

#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2026 Jeremy Gubler
#
# Baut ein .rpm aus einem fertigen Release-Binary.
#
# Aufruf:
#     cargo build --release -p ferrite-ctl
#     packaging/build-rpm.sh target/release/ferrite [ZIELVERZEICHNIS]
#
# Das .deb ist das Paket, das auf einer echten Maschine ausprobiert wird; das
# .rpm wird gebaut und sein Inhalt geprueft, aber nicht installiert — dafuer
# fehlt der CI eine Distribution, die es annimmt. Das steht so im README, damit
# niemand mehr Zusage darin liest, als gedeckt ist.

set -eu

BINARY="${1:?Aufruf: build-rpm.sh <BINARY> [ZIELVERZEICHNIS]}"
OUTDIR="${2:-target/packages}"
HERE="$(cd "$(dirname "$0")" && pwd)"

command -v rpmbuild >/dev/null || { echo "rpmbuild fehlt" >&2; exit 1; }
[ -x "$BINARY" ] || { echo "$BINARY ist nicht ausfuehrbar" >&2; exit 1; }

VERSION="$("$BINARY" version | awk '{print $2}')"
case "$VERSION" in
    [0-9]*.[0-9]*.[0-9]*) ;;
    *) echo "unerwartete Version: $VERSION" >&2; exit 1 ;;
esac

TOP="$(mktemp -d)"
trap 'rm -rf "$TOP"' EXIT
mkdir -p "$TOP/SOURCES" "$TOP/SPECS"

cp "$BINARY"                              "$TOP/SOURCES/ferrite"
cp "$HERE/systemd/ferrite.service"        "$TOP/SOURCES/ferrite.service"
cp "$HERE/systemd/ferrite-scrub.service"  "$TOP/SOURCES/ferrite-scrub.service"
cp "$HERE/systemd/ferrite-scrub.timer"    "$TOP/SOURCES/ferrite-scrub.timer"
cp "$HERE/ferrite.conf.example"           "$TOP/SOURCES/ferrite.conf.example"
cp "$HERE/ferrite.8"                      "$TOP/SOURCES/ferrite.8"
cp "$HERE/modules-load.d/ferrite.conf"    "$TOP/SOURCES/modules-load.conf"
cp "$HERE/../LICENSE"                     "$TOP/SOURCES/COPYING"
cp "$HERE/ferrite.spec"                   "$TOP/SPECS/ferrite.spec"

# `_unitdir` definiert sonst nur systemd-rpm-macros, und die gibt es auf einem
# Debian-Bauwirt nicht. Fest eingetragen statt geraten: Weicht der Pfad auf
# einer Zieldistribution ab, ist das eine Aenderung, die jemand entscheidet.
rpmbuild -bb \
    --define "_topdir $TOP" \
    --define "ferrite_version $VERSION" \
    --define "_unitdir /usr/lib/systemd/system" \
    --define "_build_id_links none" \
    "$TOP/SPECS/ferrite.spec" >"$TOP/log" 2>&1 || { cat "$TOP/log" >&2; exit 1; }

mkdir -p "$OUTDIR"
RPM="$(find "$TOP/RPMS" -name '*.rpm' -type f | head -1)"
[ -n "$RPM" ] || { echo "rpmbuild hat kein Paket erzeugt" >&2; cat "$TOP/log" >&2; exit 1; }
cp "$RPM" "$OUTDIR/"
echo "$OUTDIR/$(basename "$RPM")"

#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2026 Jeremy Gubler
#
# Baut ein .deb aus einem fertigen Release-Binary.
#
# # Warum von Hand und nicht mit cargo-deb
#
# Dieselbe Ueberlegung wie bei ublk und FUSE. Ein .deb ist ein ar-Archiv aus
# drei Dateien, und was hineingehoert, steht in der Debian Policy und nicht in
# den Voreinstellungen eines fremden Werkzeugs. Von Hand sind es achtzig
# Zeilen, die man lesen kann; mit einem Werkzeug waeren es drei Zeilen
# Konfiguration und eine Abhaengigkeit, deren naechste Version das Layout
# anders findet.
#
# Aufruf:
#     cargo build --release -p ferrite-ctl
#     packaging/build-deb.sh target/release/ferrite [ZIELVERZEICHNIS]

set -eu

BINARY="${1:?Aufruf: build-deb.sh <BINARY> [ZIELVERZEICHNIS]}"
OUTDIR="${2:-target/packages}"
HERE="$(cd "$(dirname "$0")" && pwd)"

for tool in dpkg-deb dpkg objdump strip gzip; do
    command -v "$tool" >/dev/null || { echo "$tool fehlt" >&2; exit 1; }
done
[ -x "$BINARY" ] || { echo "$BINARY ist nicht ausfuehrbar" >&2; exit 1; }

# Die Version kommt aus dem Binary und nicht aus der Cargo.toml daneben: Was
# `ferrite version` sagt, ist das, was der Betreiber sieht — und genau das
# gehoert auf das Paket. Ein zweiter Ort fuer dieselbe Zahl weicht eines Tages ab.
VERSION="$("$BINARY" version | awk '{print $2}')"
case "$VERSION" in
    [0-9]*.[0-9]*.[0-9]*) ;;
    *) echo "unerwartete Version: $VERSION" >&2; exit 1 ;;
esac
ARCH="$(dpkg --print-architecture)"

# Welches glibc das Binary wirklich braucht, steht in seinen Symbolen. Eine
# geratene Untergrenze waere entweder zu streng (das Paket laesst sich nicht
# installieren) oder zu lasch (es installiert sich und startet nicht).
GLIBC="$(objdump -T "$BINARY" \
    | sed -n 's/.*GLIBC_\([0-9][0-9.]*\).*/\1/p' \
    | sort -V | tail -1)"
[ -n "$GLIBC" ] || { echo "keine GLIBC-Symbole gefunden" >&2; exit 1; }

STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
# `mktemp` legt 0700 an. Das Wurzelverzeichnis des Pakets traegt seine Rechte
# aber ins Archiv, und ein 0700 auf `./` sieht in `dpkg -c` aus wie ein Fehler.
chmod 0755 "$STAGE"

install -D -m 0755 "$BINARY"                       "$STAGE/usr/bin/ferrite"
# Vollstaendig, wie es sich fuer ein Paket gehoert: Die Debug-Abschnitte machen
# zwei Drittel der Datei aus. Wer einen Backtrace von einer echten Maschine
# aufloesen will, nimmt das ungestrippte Binary aus demselben Bau — CI legt es
# als Artefakt neben das Paket, und `addr2line` bringt beides zusammen.
strip "$STAGE/usr/bin/ferrite"
install -D -m 0644 "$HERE/systemd/ferrite.service" "$STAGE/lib/systemd/system/ferrite.service"
install -D -m 0644 "$HERE/systemd/ferrite-scrub.service" \
                                                   "$STAGE/lib/systemd/system/ferrite-scrub.service"
install -D -m 0644 "$HERE/systemd/ferrite-scrub.timer" \
                                                   "$STAGE/lib/systemd/system/ferrite-scrub.timer"
install -D -m 0644 "$HERE/systemd/ferrite-check.service" \
                                                   "$STAGE/lib/systemd/system/ferrite-check.service"
install -D -m 0644 "$HERE/systemd/ferrite-check.timer" \
                                                   "$STAGE/lib/systemd/system/ferrite-check.timer"
install -D -m 0644 "$HERE/modules-load.d/ferrite.conf" \
                                                   "$STAGE/usr/lib/modules-load.d/ferrite.conf"
install -D -m 0644 "$HERE/ferrite.conf.example"    "$STAGE/etc/ferrite/ferrite.conf"
install -D -m 0644 "$HERE/ferrite.conf.example"    "$STAGE/usr/share/doc/ferrite/ferrite.conf.example"
install -D -m 0644 "$HERE/copyright"               "$STAGE/usr/share/doc/ferrite/copyright"

# Das Cockpit-Modul. Es liegt im selben Paket und nicht in einem eigenen:
# fuenfzehn Kilobyte statische Dateien, die ohne Cockpit einfach herumliegen.
# Ein zweites Paket waere eine zweite Versionsnummer, die auseinanderlaufen
# kann, fuer nichts.
install -D -m 0644 "$HERE/cockpit/manifest.json" "$STAGE/usr/share/cockpit/ferrite/manifest.json"
install -D -m 0644 "$HERE/cockpit/index.html"   "$STAGE/usr/share/cockpit/ferrite/index.html"
install -D -m 0644 "$HERE/cockpit/ferrite.js"   "$STAGE/usr/share/cockpit/ferrite/ferrite.js"
install -D -m 0644 "$HERE/cockpit/ferrite.css"  "$STAGE/usr/share/cockpit/ferrite/ferrite.css"

# `-n` laesst den Zeitstempel weg. Sonst enthielte dasselbe Paket, zweimal
# gebaut, verschiedene Bytes — und dann laesst sich nicht mehr zeigen, dass
# eine Datei aus genau diesem Quelltext stammt.
# `changelog.gz` und nicht `changelog.Debian.gz`: Die Version traegt keine
# Debian-Revision, das Paket ist also nativ, und dann ist der andere Name der
# falsche.
gzip -9nc "$HERE/changelog" > "$STAGE/usr/share/doc/ferrite/changelog.gz"
chmod 0644 "$STAGE/usr/share/doc/ferrite/changelog.gz"
install -d -m 0755 "$STAGE/usr/share/man/man8"
gzip -9nc "$HERE/ferrite.8" > "$STAGE/usr/share/man/man8/ferrite.8.gz"
chmod 0644 "$STAGE/usr/share/man/man8/ferrite.8.gz"

# Wohin das Betriebstagebuch geht, sagt die Konfiguration; das Verzeichnis
# muss es aber schon geben, sonst faellt die erste Zeile ins Leere.
install -d -m 0755 "$STAGE/var/lib/ferrite"

install -d -m 0755 "$STAGE/DEBIAN"
for script in postinst prerm postrm; do
    install -m 0755 "$HERE/deb/$script" "$STAGE/DEBIAN/$script"
done

# Alle Dateien unter /etc sind Konffiles. Ohne diese Liste ueberschriebe die
# naechste Installation die Konfiguration des Betreibers wortlos.
printf '/etc/ferrite/ferrite.conf\n' > "$STAGE/DEBIAN/conffiles"
chmod 0644 "$STAGE/DEBIAN/conffiles"

# md5sums, damit `dpkg -V` etwas zu pruefen hat. Konffiles bleiben draussen:
# Sie duerfen sich aendern, und eine Warnung fuer eine erwuenschte Aenderung
# ist eine Warnung, die man abzuschalten lernt.
( cd "$STAGE" && find . -type f \
    ! -path './DEBIAN/*' ! -path './etc/*' \
    -printf '%P\0' | sort -z | xargs -0 md5sum > DEBIAN/md5sums )
chmod 0644 "$STAGE/DEBIAN/md5sums"

SIZE="$(du -sk --exclude=DEBIAN "$STAGE" | cut -f1)"

cat > "$STAGE/DEBIAN/control" <<CONTROL
Package: ferrite
Version: $VERSION
Architecture: $ARCH
Maintainer: Jeremy Gubler <gubler.jeremy@gmail.com>
Installed-Size: $SIZE
Depends: libc6 (>= $GLIBC)
Recommends: btrfs-progs
Suggests: cockpit
Section: admin
Priority: optional
Homepage: https://github.com/jeremygubler/ferrite
Description: Paritaets-Array mit gemischten Plattengroessen
 Ferrite bildet Paritaet ueber gleiche Offsets statt ueber Streifen. Jede
 Datenplatte traegt ein eigenes Dateisystem und bleibt einzeln lesbar; faellt
 mehr aus, als die Paritaet abdeckt, sind die uebrigen Platten vollstaendig
 und einzeln montierbar.
 .
 Meldet btrfs einen Block als korrupt, rekonstruiert Ferrite ihn aus der
 Paritaet und schreibt ihn zurueck. Eigene Pruefsummen ueber Nutzdaten gibt es
 nicht - die liegen bei btrfs.
 .
 Das Paket schaltet keinen Dienst ein. Wer ferrite.service startet, uebergibt
 Blockgeraete an ublk und haengt Dateisysteme ein; das gehoert entschieden und
 nicht mitinstalliert.
CONTROL
chmod 0644 "$STAGE/DEBIAN/control"

mkdir -p "$OUTDIR"
DEB="$OUTDIR/ferrite_${VERSION}_${ARCH}.deb"
dpkg-deb --root-owner-group --build "$STAGE" "$DEB" >/dev/null
echo "$DEB"

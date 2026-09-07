# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2026 Jeremy Gubler
#
# Gebaut wird das hier von packaging/build-rpm.sh, das die Version hereingibt
# und die Dateien in SOURCES legt. Kein %build und kein %prep: Das Binary ist
# schon fertig, und ein zweites Uebersetzen im Paketbau waere ein zweiter
# Compiler-Aufruf mit anderen Flags als der, den die Tests gesehen haben.

Name:           ferrite
Version:        %{ferrite_version}
Release:        1
Summary:        Paritaets-Array mit gemischten Plattengroessen
License:        GPL-3.0-or-later
URL:            https://github.com/jeremygubler/ferrite

Source0:        ferrite
Source1:        ferrite.service
Source2:        ferrite-scrub.service
Source3:        ferrite-scrub.timer
Source4:        ferrite.conf.example
Source5:        ferrite.8
Source6:        modules-load.conf
Source7:        COPYING
Source8:        manifest.json
Source9:        index.html
Source10:       ferrite.js
Source11:       ferrite.css
Source12:       ferrite-check.service
Source13:       ferrite-check.timer

Recommends:     btrfs-progs
Suggests:       cockpit
%{?systemd_requires}

%description
Ferrite bildet Paritaet ueber gleiche Offsets statt ueber Streifen. Jede
Datenplatte traegt ein eigenes Dateisystem und bleibt einzeln lesbar; faellt
mehr aus, als die Paritaet abdeckt, sind die uebrigen Platten vollstaendig und
einzeln montierbar.

Meldet btrfs einen Block als korrupt, rekonstruiert Ferrite ihn aus der
Paritaet und schreibt ihn zurueck. Eigene Pruefsummen ueber Nutzdaten gibt es
nicht - die liegen bei btrfs.

Das Paket schaltet keinen Dienst ein. Wer ferrite.service startet, uebergibt
Blockgeraete an ublk und haengt Dateisysteme ein; das gehoert entschieden und
nicht mitinstalliert.

%install
install -D -m 0755 %{SOURCE0} %{buildroot}%{_bindir}/ferrite
install -D -m 0644 %{SOURCE1} %{buildroot}%{_unitdir}/ferrite.service
install -D -m 0644 %{SOURCE2} %{buildroot}%{_unitdir}/ferrite-scrub.service
install -D -m 0644 %{SOURCE3} %{buildroot}%{_unitdir}/ferrite-scrub.timer
%{_unitdir}/ferrite-check.service
%{_unitdir}/ferrite-check.timer
install -D -m 0644 %{SOURCE12} %{buildroot}%{_unitdir}/ferrite-check.service
install -D -m 0644 %{SOURCE13} %{buildroot}%{_unitdir}/ferrite-check.timer
install -D -m 0644 %{SOURCE4} %{buildroot}%{_sysconfdir}/ferrite/ferrite.conf
install -D -m 0644 %{SOURCE4} %{buildroot}%{_docdir}/ferrite/ferrite.conf.example
install -D -m 0644 %{SOURCE5} %{buildroot}%{_mandir}/man8/ferrite.8
install -D -m 0644 %{SOURCE6} %{buildroot}%{_prefix}/lib/modules-load.d/ferrite.conf
install -D -m 0644 %{SOURCE7} %{buildroot}%{_docdir}/ferrite/COPYING
install -D -m 0644 %{SOURCE8} %{buildroot}%{_datadir}/cockpit/ferrite/manifest.json
install -D -m 0644 %{SOURCE9} %{buildroot}%{_datadir}/cockpit/ferrite/index.html
install -D -m 0644 %{SOURCE10} %{buildroot}%{_datadir}/cockpit/ferrite/ferrite.js
install -D -m 0644 %{SOURCE11} %{buildroot}%{_datadir}/cockpit/ferrite/ferrite.css
install -d -m 0755 %{buildroot}/var/lib/ferrite

%post
# Nur neu einlesen. Kein enable, kein start - siehe %%description.
if [ -d /run/systemd/system ]; then
    systemctl daemon-reload >/dev/null 2>&1 || true
fi

%preun
# $1 == 0 heisst: Entfernen, kein Upgrade. Anhalten, solange das Programm noch
# da ist - `ferrite run` baut auf SIGTERM den Pool ab, dann die Members, dann
# die Blockgeraete.
if [ "$1" -eq 0 ] && [ -d /run/systemd/system ]; then
    systemctl stop ferrite-scrub.timer ferrite-check.timer >/dev/null 2>&1 || true
    systemctl stop ferrite.service >/dev/null 2>&1 || true
    systemctl disable ferrite-scrub.timer ferrite-check.timer >/dev/null 2>&1 || true
    systemctl disable ferrite.service >/dev/null 2>&1 || true
fi

%postun
if [ -d /run/systemd/system ]; then
    systemctl daemon-reload >/dev/null 2>&1 || true
fi

%files
%{_bindir}/ferrite
%{_unitdir}/ferrite.service
%{_unitdir}/ferrite-scrub.service
%{_unitdir}/ferrite-scrub.timer
%{_prefix}/lib/modules-load.d/ferrite.conf
%config(noreplace) %{_sysconfdir}/ferrite/ferrite.conf
%dir %{_sysconfdir}/ferrite
%dir /var/lib/ferrite
%doc %{_docdir}/ferrite/ferrite.conf.example
%license %{_docdir}/ferrite/COPYING
%{_mandir}/man8/ferrite.8*
%{_datadir}/cockpit/ferrite/

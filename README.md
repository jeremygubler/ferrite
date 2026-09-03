# Ferrite

Ein NAS-Betriebssystem für Linux, dessen Speicher-Layer neu gebaut ist statt
neu verpackt.

Gemischte Plattengrössen, jede Platte einzeln lesbar, Selbstheilung bei
Bit-Rot, atomare Updates. Läuft bare metal wie virtualisiert.

> **Status: früh.** Das *On-Disk-Format* steht bei Version 1.0 und ist
> eingefroren. **Ferrite selbst ist es nicht** — die beiden Versionen haben
> nichts miteinander zu tun. Es gibt die Metadatenschicht und die
> Paritätsrechnung, aber kein Blockgerät: Ferrite fasst bislang keine Platte an
> und kann keine Daten speichern. Lege nichts darauf ab, wovon du nur eine
> Kopie hast — auch später nicht, bevor das Crash-Harness aus Meilenstein 3
> grün in CI läuft.

## Warum

Unraids Array-Modell ist richtig: unabhängige Dateisysteme pro Platte, Parität
darüber, gemischte Grössen erlaubt, und beim Totalausfall bleiben die übrigen
Platten einzeln lesbar. Kein RAID5/6 kann das.

Vier Dinge daran sind es nicht:

| | Unraid | Ferrite |
|---|---|---|
| Bit-Rot im Array | wird still mitgeparitet | wird erkannt und repariert |
| Schreibpfad | Read-Modify-Write pro Write | Write-Log, Parität gebündelt |
| Kernel | gepatchter `md`-Treiber | Stock-Kernel, Engine im Userspace |
| Zustand | Config im USB-Flash | Superblöcke + deklarative Config |

**Selbstheilung ohne Mirror.** Jeder Data-Member trägt btrfs mit Prüfsummen.
Meldet btrfs einen korrupten Block, rekonstruiert der Repair-Broker ihn aus der
Parität und schreibt ihn zurück. Prüfsummen ohne Redundanz können nur melden,
Parität ohne Prüfsummen merkt nichts — erst die Kopplung repariert.

## Was Ferrite selbst baut

Ein NAS-OS ist zu grossen Teilen Integration. Samba, NFS, Podman, libvirt,
smartmontools und der Kernel werden übernommen, nicht nachgebaut. Eigenanteil
sind die Schichten, in denen Ferrite sich unterscheidet:

| Schicht | Herkunft |
|---|---|
| Web-UI und CLI | eigen, reine API-Clients |
| Control plane, deklarativ | eigen |
| Dienste (Samba, NFS, Podman, libvirt) | übernommen |
| Pool-Namespace (FUSE-Passthrough) | eigen |
| Paritäts-Engine und Repair-Broker | eigen |
| Basis-OS (Kernel, systemd, A/B-Updates) | übernommen, image-basiert |

Die Basis ist ein image-basiertes System im bootc/ostree-Stil: Das OS ist ein
Container-Image, Updates sind atomar, ein fehlgeschlagenes Update bootet in die
vorige Version zurück. Kein Bootmedium, das Zustand trägt.

## Der Weg dorthin

Die Engine kommt vor dem OS, und das ist keine Verkleinerung des Ziels, sondern
der einzige Weg dahin: Ein NAS-OS mit unbewiesener Speicherschicht bekommt keine
Nutzer. Eine Engine, die als Paket neben einem bestehenden Setup läuft, kann
jeder testen — und die ersten Nutzer finden die Crash-Bugs, die ein einzelner
Entwickler nie findet.

1. ~~**Format einfrieren.**~~ **Erledigt.** `docs/FORMAT.md` steht bei 1.0.
   `format/` und `parity/` sind fertig, beide Decode-Pfade und der Recovery-Pfad
   sind gefuzzt, Golden Vectors sichern das Byte-Layout, und `integration/` hat
   das Format einmal vollständig durchgespielt — ohne Blockgerät. Ab hier darf
   Code Bytes auf eine echte Platte schreiben.
2. **Paritäts-Engine.** Reed-Solomon P+Q ist fertig; offen sind ublk-Target,
   Write-Log-Anbindung und Rebuild. **Braucht Linux** mit geladenem `ublk_drv`.
3. **Crash-Harness.** `dm-flakey` und `dm-dust` für Lesefehler und stille
   Korruption, Power-Fail per `SIGKILL` an zufälligen Punkten im Schreibpfad,
   danach Replay und vollständige Paritätsverifikation. Ab hier in CI,
   blockiert Merges.
   Ein Fall wartet hier schon: **Absturz im degradierten Betrieb.** Neurechnen
   der Parität scheitert am fehlenden Member, Fortschreiben am nach dem Absturz
   unzuverlässigen alten Inhalt. `engine` gibt dafür bewusst einen Fehler
   (`CannotUpdateParity`) statt einer geratenen Antwort — was stattdessen
   passieren soll, entscheidet dieses Harness.
4. **Repair-Broker.** btrfs-EIO abfangen, rekonstruieren, zurückschreiben.
5. **Pool-Namespace.** FUSE-Passthrough, Share-Policies.
6. **Control plane und UI.**
7. **OS-Image.** Erst jetzt. Bis hierhin läuft Ferrite als Paket auf
   bestehenden Distributionen.

Meilenstein 3 steht bewusst vor den Features. Ein Storage-Projekt gewinnt
Vertrauen nicht über Funktionsumfang, sondern darüber, dass es beim
Stromausfall nichts verliert — und das lässt sich nur zeigen, wenn der Nachweis
von Anfang an mitläuft.

## Stand

| Komponente | |
|---|---|
| `docs/FORMAT.md` | **Version 1.0 — eingefroren** |
| `format/` | Superblock samt Member-Zustand, Assemble, Write-Log mit Ringpuffer und Recovery, Golden Vectors, 6 Fuzz-Targets — 103 Tests grün |
| `parity/` | GF(2^8), P+Q, Rekonstruktion aller Ein- und Zwei-Slot-Fälle — 32 Tests grün |
| `integration/` | In-Memory-Generalprobe, wiederaufsetzbarer Rebuild — 9 Tests grün |
| `engine/` | Planung von Schreibpfad und Rebuild, plattformunabhängig — 47 Tests grün. ublk-Target offen, braucht Linux |
| `broker/` | offen |
| `pool/` | offen |
| `ctl/` | offen |

```
cargo test
```

Die Fuzz-Targets liegen in `format/fuzz/` und brauchen eine Nightly-Toolchain
plus `cargo-fuzz`. Bei jedem Push läuft eine 60-Sekunden-Rauchprobe pro Target,
sonntags ein 30-Minuten-Lauf mit aufgehobenem Korpus. Von Hand:

```
cargo install cargo-fuzz
cd format && cargo +nightly fuzz run log_ring_replay -- -max_total_time=300
```

Jeder Fund wird zuerst als Regressionstest unter `format/tests/`
festgehalten und dann behoben — nicht umgekehrt.

Der Durchsatz der Paritätsrechnung lässt sich messen, ohne eine Platte zu
haben. Ein Kern, 64-KiB-Blöcke, 32 Data-Slots: rund 24 GB/s für P und 12 GB/s
für Q. Damit ist die Rechnung für realistische Arrays kein Engpass — der
Nachweis dafür gehört ins Repo, nicht in eine Behauptung:

```
cargo bench -p ferrite-parity
```

## Mitarbeit

Ferrite ist auf Dauer kein Ein-Personen-Projekt. Ein System, dem Leute 40 TB
anvertrauen, braucht mehr als einen Maintainer — sonst ist der Bus-Faktor
selbst das grösste Datenrisiko. `docs/FORMAT.md` ist deshalb normativ und
vollständig genug, um eine unabhängige Implementierung zu schreiben.

Wie man mitmacht, steht in [`CONTRIBUTING.md`](CONTRIBUTING.md) — inklusive der
Antwort auf die Frage, was man ohne sechs Festplatten im Keller beitragen kann.
Kurzfassung: fast alles. Die Invarianten selbst stehen in
[`CLAUDE.md`](CLAUDE.md).

## Lizenz

GPL-3.0-or-later. Die Engine läuft im Userspace, es gibt also keine
Kernelmodul-Lizenzfragen. Copyleft ist Absicht: Format und Engine sollen nicht
in einem geschlossenen Produkt wieder verschwinden.

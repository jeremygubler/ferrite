# Mitarbeit an Ferrite

Danke, dass du hier bist. Ferrite soll auf Dauer kein Ein-Personen-Projekt sein:
Ein System, dem Leute 40 TB anvertrauen, braucht mehr als einen Maintainer, sonst
ist der Bus-Faktor selbst das grösste Datenrisiko.

Lies zuerst [`README.md`](README.md) für das Warum und
[`docs/FORMAT.md`](docs/FORMAT.md) für das On-Disk-Format. Die Invarianten stehen
in [`CLAUDE.md`](CLAUDE.md) — die Datei ist an ein Werkzeug adressiert, gilt aber
für alle. Bei Widersprüchen zwischen dieser Datei hier und `CLAUDE.md` gewinnt
`CLAUDE.md`.

**Ferrite kann noch keine Daten speichern.** Es gibt kein Blockgerät. Lege nichts
darauf ab, wovon du nur eine Kopie hast.

## Wo du anfangen kannst

**Ohne jede Hardware.** Der grösste Teil des Projekts ist reine Rechnung und
läuft auf jedem Rechner: `format/` beschreibt Bytes, `parity/` rechnet, `engine/`
plant, `integration/` spielt beides gegeneinander durch. Zusammen 191 Tests und
sechs Fuzz-Targets, alle ohne Platte.

Das ist Absicht. Ein Speicherprojekt, an dem man nur mit sechs Festplatten im
Keller mitarbeiten kann, bekommt keine Mitarbeiter.

**Mit Linux.** Meilenstein 2 braucht Kernel ≥ 6.0 mit geladenem `ublk_drv`, das
Crash-Harness aus Meilenstein 3 zusätzlich `dm-flakey` und Root. Der Auftrag
dafür steht ausformuliert in
[`docs/CLAUDE-CODE-KICKOFF.md`](docs/CLAUDE-CODE-KICKOFF.md). Eine VM reicht —
und für vieles sogar ein CI-Runner: `ublk_drv` liegt dort in
`linux-modules-extra-$(uname -r)`, `dm-flakey` ist ohnehin geladen. Details im
Kickoff.

**Am wertvollsten wäre eine zweite Implementierung.** `docs/FORMAT.md` ist
normativ und soll vollständig genug sein, um Ferrite unabhängig nachzubauen. Wer
das versucht und dabei über eine Lücke stolpert, findet etwas, das intern niemand
mehr sieht.

## Bauen und prüfen

Rust ≥ 1.75, Edition 2021, keine Dependencies. Ein `cargo`, sonst nichts.

```
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --all --check
```

Alle drei müssen grün sein, bevor du einen Patch schickst. CI prüft dasselbe und
hält zusätzlich die MSRV 1.75.

Den Durchsatz der Paritätsrechnung misst:

```
cargo bench -p ferrite-parity
```

## Fuzzing

`format/fuzz/` ist ein eigener Workspace mit sechs Targets. Nightly und
`cargo-fuzz` nötig:

```
cargo install cargo-fuzz
cd format && cargo +nightly fuzz run log_ring_replay -- -max_total_time=300
```

Bei jedem Push läuft eine 60-Sekunden-Rauchprobe pro Target, sonntags 30 Minuten
mit aufgehobenem Korpus.

Auf Windows-MSVC braucht libFuzzer die x64-AddressSanitizer-Runtime aus Visual
Studio (Komponente `Microsoft.VisualStudio.Component.VC.ASAN`). Ohne sie
scheitert das Linken mit `clang_rt.asan_dynamic_runtime_thunk-x86_64.lib` nicht
gefunden. Auf Linux funktioniert es out of the box.

## Die Regeln, die nicht verhandelbar sind

**`docs/FORMAT.md` ist normativ, und das Format ist eingefroren.** Der Code folgt
dem Dokument, nicht umgekehrt. Ab Version 1.0 ändert sich kein Offset, keine
Feldbedeutung und keine Gültigkeitsregel. Erweitert wird über die Feature-Bits
aus Abschnitt 4.1 und über die reservierten Bereiche, deren Nullwert das
bisherige Verhalten bedeuten muss.

`format/tests/golden.rs` hält das Byte-Layout als Literale fest. **Schlägt einer
dieser Tests fehl, ist die Zusage von 1.0 gebrochen** — dann nimmst du den Code
zurück, nicht die Erwartung.

**`format/` und `parity/` bleiben rein.** Keine Dependencies, kein I/O, keine
Konfiguration, keine Uhrzeit, kein Zufall aus der Umgebung. Zufall und Zeit
kommen als Parameter herein. Das ist der Grund, warum sich beide vollständig
fuzzen lassen, ohne eine Platte anzufassen — und warum die Tests einen festen
LCG statt einer Zufallsbibliothek benutzen.

**Fehler werden zurückgegeben, nicht geloggt und geschluckt.** Kein `unwrap()`
oder `expect()` ausserhalb von Tests. In einem Speicherprojekt ist ein
verschluckter Fehler ein stiller Datenverlust.

**Keine Mocks für fehlende Hardware.** Wenn etwas Kernelzugriff oder Root
braucht, sag es, statt eine Attrappe zu bauen. Ein Mock, der etwas anderes testet
als die Realität, ist schlimmer als kein Test — er erzeugt Vertrauen, das nicht
gedeckt ist. Loop-Geräte über `losetup` sind dagegen kein Mock: Die Blockschicht
des Kernels ist echt.

**Erst korrekt, dann schnell.** Optimierungen gehören in einen eigenen Commit,
mit Messung davor und danach. `cargo bench -p ferrite-parity` ist dafür da.

## Was ein guter Patch mitbringt

- `cargo test` grün, inklusive Doc-Tests
- `cargo clippy --all-targets -- -D warnings` sauber
- `cargo fmt --all --check` sauber
- **Zu jeder neuen Gültigkeitsregel ein Test, der ihre Verletzung nachweist.**
  Nicht nur der Happy Path. Eine Fehlervariante ohne zugehörigen Test ist
  unfertig.
- Bei neuen On-Disk-Strukturen zusätzlich: Roundtrip-Test, Bitflip-Test über den
  gesamten prüfsummengeschützten Bereich, Test gegen zu kurzen Puffer, Test gegen
  Zufallsmüll ohne Panik
- Bei einem Bugfix im Recovery-Pfad: der Test, der den Fehler vorher rot zeigt.
  Ohne ihn ist es kein Bugfix.

Kommentare erklären **warum**, nicht was. Ein Kommentar, der die darüberstehende
Zeile nacherzählt, wird gelöscht.

## Sprache und Commits

Deutsch in Kommentaren, Doc-Comments und Commit-Messages. Bezeichner auf
Englisch. Umlaute in Rust-Kommentaren ausgeschrieben (`ue`, `ae`, `oe`), im
Markdown normal.

Kleine Commits entlang der Meilensteine, nicht ein grosser Wurf. Die Message
sagt, **warum** die Änderung nötig war — was sie tut, steht im Diff.

Für grössere Änderungen mach vorher ein Issue auf. Kleine Korrekturen können
direkt als Pull Request kommen.

## Was nicht angenommen wird

- Änderungen am Byte-Layout ohne Feature-Bit
- Dependencies in `format/` oder `parity/`
- Attrappen für Hardware, die nicht da ist
- Optimierungen ohne Messung davor und danach
- Ein Fuzz-Fund, der behoben wurde, ohne dass vorher ein Regressionstest ihn
  festgehalten hat

## Wenn dir im Format etwas unklar ist

Melde es, statt zu raten. Ein falsch geratenes On-Disk-Detail kostet später eine
Formatversion, und ab 1.0 gibt es die nicht mehr umsonst.

Das ist ausdrücklich eine willkommene Art von Beitrag: Mehrere Regeln in
`docs/FORMAT.md` stehen erst dort, weil beim Implementieren aufgefallen ist, dass
sie fehlten — die Nullbyte-Regel beim Label, das Ende der Sequenzkette, die
Grenzen beim Anwenden eines Write-Records. Alle drei hätten stillen Datenverlust
verursacht.

## Lizenz

GPL-3.0-or-later. Mit einem Beitrag stellst du ihn unter dieselbe Lizenz. Jede
Quelldatei trägt den SPDX-Header:

```rust
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) <Jahr> <Name>
```

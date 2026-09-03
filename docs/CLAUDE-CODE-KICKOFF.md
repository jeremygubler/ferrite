# Startprompt für Claude Code

`CLAUDE.md` liegt im Repo-Root und wird automatisch gelesen. Dieser Prompt gibt
nur den Auftrag für die nächste Sitzung. Alles ab `---` kopieren.

---

Lies zuerst `CLAUDE.md`, `README.md` und `docs/FORMAT.md`. Sie sind normativ —
wenn dein Code ihnen widerspricht, ist der Code der Fehler.

Zwei Aufgaben, in dieser Reihenfolge.

## Aufgabe 1 — Fuzz-Targets für `format/`

Lege `format/fuzz/` mit `cargo-fuzz` an, zwei Targets:

- `superblock_decode` — beliebige Bytes in `Superblock::decode`
- `log_header_decode` — beliebige Bytes in `LogRecordHeader::decode`

Beide dürfen unter keinen Umständen paniken, endlos laufen oder allozieren, was
die Eingabe nicht deckt. Ergänze ein drittes Target `superblock_roundtrip`, das
aus dem Fuzz-Input einen gültigen `Superblock` konstruiert, kodiert, dekodiert
und auf Gleichheit prüft.

Ein viertes Target `chain_replay`: `ChainValidator` gegen eine aus dem
Fuzz-Input abgeleitete Folge von Records. Invariante: Sobald einmal
`StopReplay` kam, darf nie wieder `Accept` kommen — egal wie gültig ein
späterer Record für sich aussieht. An dieser Regel hängt, ob ein Replay nach
Absturz alte Daten über neue schreibt.

Lass jedes Target mindestens fünf Minuten laufen und berichte, was gefunden
wurde. Jeder Crash wird als Regressionstest in `format/tests/roundtrip.rs`
festgehalten, bevor du ihn behebst.

Dann ein GitHub-Actions-Workflow `.github/workflows/ci.yml`: `cargo test`,
`cargo clippy -- -D warnings`, `cargo fmt --check`, plus ein kurzer Fuzz-Lauf
pro Target auf jedem Push.

## Aufgabe 2 — Crate `parity/`

Reine Rechnung, keine I/O, keine Dependencies. Vollständig testbar ohne
Hardware, deshalb kommt es vor der Engine.

Inhalt:

- GF(2⁸) mit Polynom `0x11D`, Log-/Antilog-Tabellen zur Compile-Zeit
- `compute_p(slots: &[&[u8]], out: &mut [u8])` — XOR über alle Slots
- `compute_q(slots: &[&[u8]], out: &mut [u8])` — `⨁ⱼ gʲ · Dⱼ`, `g = 0x02`,
  `j` ist der `slot_index`, nicht die Position im übergebenen Slice
- Rekonstruktion: ein fehlender Data-Slot aus P; ein fehlender aus Q; zwei
  fehlende Data-Slots aus P und Q; ein Data-Slot plus P; ein Data-Slot plus Q
- **Zero-Extension:** Slots dürfen unterschiedlich lang sein. Ein kürzerer Slot
  liest jenseits seines Endes als Nullbytes. Das ist keine Bequemlichkeit,
  sondern die Regel, die gemischte Plattengrössen erlaubt.

Tests, die ich sehen will:

- Bekannte Vektoren für die GF-Arithmetik (Assoziativität, Inverse, `g` ist
  Generator der multiplikativen Gruppe)
- Round-Trip über zufällige Daten mit festem LCG wie in `format/tests`: P und Q
  berechnen, ein bis zwei Slots löschen, rekonstruieren, auf Byte-Gleichheit
  prüfen — über alle Kombinationen bei 4 bis 8 Slots
- Derselbe Test mit **ungleich langen** Slots, inklusive eines Slots der Länge
  null
- Rekonstruktion eines Slots, der selbst nur Nullen enthält (fängt Fehler, die
  ein zufälliger Test übersieht)

Kein SIMD in dieser Runde. Erst korrekt, dann schnell — die Optimierung braucht
die Tests als Netz und gehört in einen eigenen Commit mit Benchmark davor und
danach.

## Was ich nicht will

Keine Engine, kein ublk, kein FUSE, nichts, das ein Blockgerät öffnet. Das
Format ist noch nicht auf 1.0 eingefroren; bis dahin schreibt kein Code auf
echte Platten.

Keine Mocks für fehlende Hardware. Wenn etwas Kernelzugriff oder Root braucht,
sag es, statt eine Attrappe zu bauen. Ein Mock, der etwas anderes testet als
die Realität, ist in einem Speicherprojekt schlimmer als kein Test — er erzeugt
Vertrauen, das nicht gedeckt ist.

Melde dich, wenn `docs/FORMAT.md` an einer Stelle unklar oder widersprüchlich
ist. Ein falsch geratenes Detail kostet später eine Formatversion.

## Abschluss

Kurze Zusammenfassung: was geändert wurde, was die Fuzzer gefunden haben, und
ob `docs/FORMAT.md` angefasst werden musste.

---

Meilenstein 2 (`engine/`, das ublk-Target) und Meilenstein 3 (Crash-Harness)
brauchen Kernel ≥ 6.0 mit geladenem `ublk_drv`, `dm-flakey`/`dm-dust` und Root.
Dafür ist eine VM im Homelab der richtige Ort, nicht eine Umgebung ohne
Kernelzugriff.

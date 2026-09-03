# Ferrite — Projektkontext für Claude Code

Speicher-Betriebssystem für Linux. Paritätsbasierter Pool mit gemischten
Plattengrössen, jede Platte einzeln lesbar, Selbstheilung bei Bit-Rot.
Rust, GPL-3.0-or-later.

Lies vor der ersten Änderung `README.md` und `docs/FORMAT.md`.

## Die Regeln, die nicht verhandelbar sind

**1. `docs/FORMAT.md` ist normativ.** Der Code folgt dem Dokument, nicht
umgekehrt. Wenn eine Änderung das On-Disk-Layout berührt, ändere zuerst das
Dokument, dann den Code, und trage die Änderung in die Versionshistorie ein.
Weicht der Code vom Dokument ab, ist der Code der Fehler.

**2. `format/` bleibt dependency-frei und I/O-frei.** Keine Crates, kein
`std::fs`, kein `unsafe`, keine Allokation ausser für Strings. Das ist der Grund,
warum sich das Format vollständig fuzzen lässt, ohne eine Platte anzufassen. Wer
hier eine Dependency einträgt, macht das Format von einer fremden Version
abhängig.

**3. Das Format ist eingefroren.** `FORMAT.md` steht bei 1.0. Das Layout ändert
sich nicht mehr — weder ein Offset noch die Bedeutung eines Feldes noch eine
Gültigkeitsregel. Erweitert wird nur über Feature-Bits (Regel 6) und über die
reservierten Bereiche, deren Nullwert das bisherige Verhalten bedeuten muss.
`format/tests/golden.rs` hält das Byte-Layout als Literale fest; schlägt es
fehl, ist die Zusage gebrochen, und die Antwort ist, den Code zurückzunehmen,
nicht die Erwartung anzupassen. Ab hier darf Code auf echte Platten schreiben.

**4. Jede Gültigkeitsregel im Dokument braucht einen Test, der ihre Verletzung
nachweist.** Nicht nur den Happy Path. Ein `FormatError`-Variant ohne
zugehörigen Test ist unfertig.

**5. Fehler werden zurückgegeben, nicht geloggt und geschluckt.** In einem
Speicherprojekt ist ein verschluckter Fehler ein stiller Datenverlust.
Kein `unwrap()` oder `expect()` ausserhalb von Tests.

**6. Keine Feature-Erweiterung des Formats ohne Feature-Bit.** `feature_compat`,
`feature_ro_compat`, `feature_incompat` sind da, damit alte Implementierungen
neue Arrays erkennen und sich korrekt verweigern.

**7. Keine eigenen Prüfsummen über Nutzdaten.** Die liegen bei btrfs auf dem
jeweiligen Data-Member. Ferrite liefert die Redundanz, aus der ein als korrupt
gemeldeter Block rekonstruiert wird. Zwei konkurrierende Prüfsummenschichten
wären nicht sicherer, nur teurer und im Fehlerfall mehrdeutig.

**8. Reine Crates bleiben rein.** Regel 2 gilt sinngemäss auch für `parity/`:
kein I/O, keine Konfiguration, keine Uhrzeit, kein Zufall aus der Umgebung.
Zufall und Zeit kommen als Parameter herein.

**9. Ein Bugfix im Recovery-Pfad ohne Test, der ihn vorher rot zeigt, ist kein
Bugfix.**

## Kerninvariante

Ferrite ist **kein Striping-Layout**. Parität wird über gleiche Offsets gebildet:

```
P[i] = D₀[i] ⊕ D₁[i] ⊕ … ⊕ Dₙ₋₁[i]
Q[i] = ⨁ⱼ gʲ · Dⱼ[i]        (GF(2⁸), g = 0x02, j = slot_index)
```

Ist ein Data-Member kürzer als der Parity-Member, liest er jenseits seines Endes
als **Nullbytes**. Das erlaubt gemischte Plattengrössen und ist zwingend — eine
Implementierung, die stattdessen abbricht, produziert falsche Parität.

Daraus folgt die Eigenschaft, die das Projekt definiert: Fallen mehr Members aus,
als Parität abdeckt, bleiben die verbleibenden Data-Members vollständig und
einzeln montierbar. Diese Eigenschaft wird durch kein späteres Feature
aufgegeben.

## Crate-Struktur

```
format/   Superblock, Log-Records, Ringpuffer, Assemble — kein I/O    [fertig]
parity/   GF(2^8), Reed-Solomon P+Q, Rekonstruktion                 [fertig]
integration/ In-Memory-Generalprobe beider Crates, keine Produktion  [fertig]
engine/   Planung von Schreibpfad und Rebuild — kein I/O            [fertig]
          Geraetezugriff, Array, Flush-Test nach 5.3              [fertig]
          ublk-Target, Write-Log-Anbindung — braucht Linux          [offen]
broker/   btrfs-EIO abfangen, rekonstruieren, zurückschreiben       [offen]
pool/     FUSE-Namespace mit Passthrough, Share-Policies            [offen]
ctl/      gRPC-Daemon und CLI                                       [offen]
```

Reihenfolge der Meilensteine steht im README. Sie ist bewusst so gewählt: Das
Crash-Harness (Meilenstein 3) kommt vor den Features.

## Arbeitsweise

- Deutsch in Kommentaren, Doc-Comments und Commit-Messages. Bezeichner auf
  Englisch.
- Umlaute in Rust-Kommentaren ausschreiben (`ue`, `ae`, `oe`), im Markdown normal.
- `cargo test` muss nach jeder Änderung grün sein. `cargo clippy -- -D warnings`
  ebenfalls, sobald clippy verfügbar ist.
- Kommentare erklären **warum**, nicht was. Ein Kommentar, der die darüberstehende
  Zeile nacherzählt, wird gelöscht.
- Kleine Commits entlang der Meilensteine, nicht ein grosser Wurf.
- Bei Unklarheit im Format: nachfragen, nicht raten. Ein falsch geratenes
  On-Disk-Detail kostet später eine Formatversion.
- Tests, die Zufall brauchen, benutzen den festen LCG aus
  `format/tests/roundtrip.rs` mit fixem Seed. Keine Test-Dependency,
  reproduzierbar, in Millisekunden durch. `cargo-fuzz` kommt zusätzlich, nicht
  stattdessen.
- Byte-Layout-Konstanten stehen an genau einer Stelle pro Struktur, als
  `const OFF_*`. Keine magischen Zahlen im Parser.

## Definition of done

- `cargo test` grün, inklusive Doc-Tests
- `cargo clippy --all-targets -- -D warnings` sauber
- `cargo fmt --check` sauber
- Neue On-Disk-Strukturen haben: Roundtrip-Test, Bitflip-Test über den
  gesamten prüfsummengeschützten Bereich, Test gegen zu kurzen Puffer, Test
  gegen Zufallsmüll ohne Panic
- Bei Formatänderungen: `docs/FORMAT.md` samt Versionshistorie mitgeführt

## Umgebung

Rust ≥ 1.75, Edition 2021. Für `engine/` später Kernel ≥ 6.0 mit geladenem
`ublk_drv`. Zielumgebung ist bare metal und VM gleichermassen; bei
virtualisierten Log-Geräten gilt Abschnitt 5.3 des Formatdokuments
(Flush-Verifikation, sonst Write-Through).

`format/`, `parity/`, Fuzzing und CI brauchen weder speziellen Kernel noch
Platten und laufen überall. `pool/` braucht FUSE, für Passthrough Kernel ≥ 6.9.
Das Crash-Harness braucht `dm-flakey`/`dm-dust` und Root.

Fehlt eine dieser Voraussetzungen: melden, nicht mocken. Ein Mock, der etwas
anderes testet als die Realität, ist in einem Speicherprojekt schlimmer als gar
kein Test — er erzeugt Vertrauen, das nicht gedeckt ist.

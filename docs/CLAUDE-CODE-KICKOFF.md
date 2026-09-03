# Startprompt für Claude Code — Meilenstein 2

`CLAUDE.md` liegt im Repo-Root und wird automatisch gelesen. Dieser Prompt gibt
nur den Auftrag für die nächste Sitzung. Alles ab `---` kopieren.

> **Diese Sitzung braucht Linux.** Kernel ≥ 6.0 mit geladenem `ublk_drv`, Root
> für das Anlegen der Geräte. Auf einer Maschine ohne Kernelzugriff ist der
> Auftrag nicht ausführbar — das ist kein Grund für eine Attrappe, sondern ein
> Grund, die Maschine zu wechseln.
>
> **Es muss aber keine eigene Maschine sein.** Auf den GitHub-Actions-Runnern
> (`ubuntu-latest`, Kernel 6.17-azure) steht alles Nötige zur Verfügung —
> nachgeprüft, nicht vermutet:
>
> ```
> sudo apt-get install -y linux-modules-extra-$(uname -r)
> sudo modprobe ublk_drv        # danach existiert /dev/ublk-control
> sudo modprobe dm_flakey       # ohne Nachinstallation bereits da
> ```
>
> `sudo` ohne Passwort, Loop-Geräte funktionieren, `dmsetup` kennt `flakey` und
> `error`. **Nicht** verfügbar ist `dm_dust`, auch nicht nachinstallierbar; die
> stille Korruption aus Meilenstein 3 lässt sich stattdessen über
> `dm-flakey`s `corrupt_bio_byte` erzeugen, Lesefehler über `dm-error`.
>
> Geprüft ist damit, dass das Modul lädt und das Kontrollgerät existiert —
> **nicht**, dass sich ein ublk-Gerät tatsächlich anlegen und betreiben lässt.
> Das ist deine erste Aufgabe.
>
> Zwei Dinge kann CI nicht: interaktives Entwickeln, und den Flush-Test aus
> Abschnitt 5.3 ehrlich beantworten. Ein Loop-Gerät auf dem Dateisystem eines
> Runners misst das Dateisystem, nicht die Platte — genau der Fall, vor dem 5.3
> warnt.
>
> **Auf Windows geht es über WSL2, aber nicht ohne eigenen Kernel.** Microsofts
> WSL-Kernel hat `CONFIG_BLK_DEV_UBLK` und `CONFIG_DM_DUST` nicht gesetzt;
> `CONFIG_DM_FLAKEY=m` ist dagegen da. Der Umbau dauert etwa 15 Minuten Bauzeit:
>
> ```bash
> apt-get install -y build-essential flex bison libssl-dev libelf-dev bc dwarves
> git clone --depth 1 --branch linux-msft-wsl-6.18.y \
>   https://github.com/microsoft/WSL2-Linux-Kernel /usr/src/wsl-kernel
> cd /usr/src/wsl-kernel
> cp Microsoft/config-wsl .config
> ./scripts/config --file .config \
>   --enable CONFIG_BLK_DEV_UBLK --module CONFIG_DM_DUST --module CONFIG_DM_FLAKEY
> make olddefconfig && make -j"$(nproc)" && make modules_install
> ```
>
> Dann `arch/x86/boot/bzImage` nach Windows kopieren und in `%USERPROFILE%\.wslconfig`
> eintragen:
>
> ```ini
> [wsl2]
> kernel=C:\\Pfad\\zu\\bzImage
> ```
>
> Nach `wsl --shutdown` existiert `/dev/ublk-control`, und `dmsetup targets`
> kennt `dust`, `flakey` und `error`. Ausgangspunkt ist Microsofts eigene
> Konfiguration — wer stattdessen `defconfig` nimmt, verliert Dateizugriff und
> Interop. Zurück zum Standardkernel führt das Löschen der `.wslconfig`.
>
> Der Flush-Test ist auch hier nicht ehrlich zu beantworten: Darunter liegt eine
> VHDX auf NTFS.

---

Lies zuerst `CLAUDE.md`, `README.md` und `docs/FORMAT.md`. Sie sind normativ —
wenn dein Code ihnen widerspricht, ist der Code der Fehler.

## Wo das Projekt steht

Meilenstein 1 ist abgeschlossen. `docs/FORMAT.md` steht bei **Version 1.0 und
ist eingefroren**: kein Offset, keine Feldbedeutung und keine Gültigkeitsregel
ändert sich noch. Erweitert wird nur über die Feature-Bits aus Abschnitt 4.1.

`format/tests/golden.rs` hält das Byte-Layout als Literale fest. **Schlägt einer
dieser Tests fehl, ist die Zusage von 1.0 gebrochen** — dann nimmst du den Code
zurück, nicht die Erwartung.

Vier Crates sind fertig und brauchen kein Gerät:

| | |
|---|---|
| `format/` | Superblock samt Member-Zustand, `assemble`, Write-Log mit Ringpuffer und Recovery |
| `parity/` | GF(2⁸), P+Q, alle Ein- und Zwei-Slot-Rekonstruktionen |
| `engine/` | Geometrie, dreckige Blöcke, Rebuild-Plan, Schreibpfad — reine Rechnung |
| `integration/` | In-Memory-Generalprobe, wiederaufsetzbarer Rebuild |

191 Tests, sechs Fuzz-Targets, CI grün — darunter ein Job, der die MSRV 1.75
hält, und ein Wochenlauf, der jedes Fuzz-Target 30 Minuten mit aufgehobenem
Korpus fährt.

**Was `engine/` schon kann, baust du nicht neu.** Sieh dir vorher an:
`BlockGeometry`, `dirty_blocks`, `RebuildPlan`, `WriteBatch`,
`required_parity_update`, `data_is_valid_at`.

## Deine Aufgaben, in dieser Reihenfolge

### 1 — Blockgeräte öffnen und Superblöcke schreiben

Das erste Mal, dass Ferrite eine echte Platte anfasst. Nur das, nichts weiter:

- Ein Gerät öffnen, seine Grösse ermitteln, `Superblock::fits_on_device` prüfen
- Primären und Backup-Superblock lesen, über `Superblock::select` entscheiden
- Beide schreiben — **primär zuerst, Backup nach einem Flush** (Abschnitt 3).
  Die Reihenfolge ist der Grund, warum es zwei Kopien gibt; wer sie umdreht,
  verliert bei einem Absturz beide auf einmal.
- Ein Array anlegen: Superblöcke für Data-, Parity- und Log-Member schreiben,
  danach über `assemble` wieder einlesen und vergleichen

Der I/O-Pfad gehört in `engine/`. Der Rechenkern darunter bleibt
plattformunabhängig, damit `cargo test` überall läuft. Diese Trennung ist
Absicht und der Grund, warum Meilenstein 1 ohne Hardware fertig wurde.

Offsets sind 4096-aligned, `O_DIRECT` ist damit möglich. Ob du es nimmst,
entscheidest du — aber begründe es im Code, nicht im Commit.

> **Erledigt** in `engine/src/device.rs` und `engine/src/array.rs`.
>
> Abweichung vom ursprünglichen Plan, der `#[cfg(target_os = "linux")]` um den
> ganzen I/O-Pfad vorsah: Der Grund für diese Grenze war, dass `cargo test`
> überall laufen soll. Das ist auch ohne sie erfüllt — positioniertes Lesen und
> Schreiben gibt es in `std` auf beiden Plattformen, und eine Datei verhält sich
> für diesen Code wie ein Blockgerät. Die Grenze wandert damit dorthin, wo sie
> hingehört: an das ublk-Target aus Aufgabe 3.
>
> Was nur ein echtes Blockgerät zeigt — Grösse über `seek` statt über Metadaten,
> `sync_data` auf ein Gerät statt auf eine Datei — steht in
> `engine/tests/loop_device.rs`, ist `#[ignore]`, braucht Linux und Root und
> läuft im CI-Job „Blockgeräte (Loop)".

### 2 — Flush-Test, Abschnitt 5.3

Bisher steht die Regel nur im Dokument und ist von nichts abgedeckt. Vor der
ersten Nutzung MUSS geprüft werden, ob das Log-Gerät `FLUSH` ehrlich beantwortet.
Fällt der Test negativ aus oder ist er nicht durchführbar, läuft das Array im
**Write-Through-Modus**: Der Write wird erst bestätigt, wenn Data-Member und
Parität aktualisiert sind.

Sag im Bericht ausdrücklich, wie du den Test gebaut hast und was er auf deiner
Hardware ergibt. Ein Flush-Test, der immer „ehrlich" sagt, ist schlimmer als
keiner.

> **Erledigt** in `engine/src/flush.rs`.
>
> **Die tragende Einsicht: Der Test ist asymmetrisch.** Ob ein `FLUSH` ehrlich
> beantwortet wird, entscheidet sich erst bei einem Stromausfall — aus dem
> Userspace eines laufenden Systems ist das nicht nachweisbar. Der Test kann
> Ehrlichkeit also **widerlegen, aber kaum belegen**. Entsprechend ist
> `Undecidable` die Vorgabe und nicht der Ausnahmefall, und Abschnitt 5.3 macht
> beides ohnehin gleich: Write-Through.
>
> Die Entscheidung fällt die reine Funktion `judge(&DeviceFacts)`. Das Sammeln
> der Fakten steht getrennt davon, weil es plattformabhängig ist. Damit lässt
> sich jede Kombination prüfen — der Test `exactly_one_combination_of_facts_yields_honest`
> geht alle 216 durch und hält fest, dass genau ein Satz Fakten zu „ehrlich"
> führt und alles andere zu Write-Through.
>
> **Verworfen: Zeitmessung.** Ein `FLUSH`, das nach 4 MiB in 20 µs zurückkommt,
> ist auf einer drehenden Platte unmöglich und auf einer NVMe mit
> Power-Loss-Protection normal. Die Messung trennt schnelle Geräte von langsamen,
> nicht Ehrlichkeit von Lüge.
>
> **Verworfen: erfolgreiches `sync_data` als Beleg.** Genau das ist die Lüge,
> um die es in 5.3 geht.
>
> **Was der Test auf dieser Maschine sagt** (WSL2, Kernel 6.18.40.1):
>
> ```
> Gerät /dev/sda   Schreibcache WriteThrough   virtualisiert   → Undecidable
> Gerät /dev/sdb   Schreibcache WriteBack      virtualisiert   → Undecidable
> ```
>
> Der erste Fall ist der Grund, warum die Virtualisierungsprüfung nötig ist:
> **`/dev/sda` meldet „write through", ist aber eine virtuelle Platte.** Wer nur
> auf `/sys/.../queue/write_cache` hört, bekommt hier „ehrlich" für ein Gerät,
> das genau die Lüge erzählt, gegen die Abschnitt 5.3 geschrieben ist. Der Test
> `no_real_block_device_on_a_virtual_machine_is_honest` läuft über alle
> Blockgeräte der Maschine und hält das fest — 27 geprüft, keines ehrlich.
>
> Nachzuvollziehen mit `cargo run -p ferrite-engine --example flush-report --
> /dev/sda`. Der Bericht gibt die Fakten aus, nicht nur das Ergebnis.
>
> **Offen und ehrlich benannt:** Auf dieser Maschine kann der Test nie „ehrlich"
> sagen, weil sie virtualisiert ist. Das ist die richtige Antwort und keine
> Lücke — aber es heisst auch, dass der positive Zweig hier nur durch die
> Faktentabelle abgedeckt ist und nicht durch eine Messung. Den harten Nachweis
> liefert erst das Crash-Harness aus Meilenstein 3.

### 3 — ublk-Target pro Data-Member

Ein ublk-Gerät je Data-Member, das seine Payload-Region 1:1 abbildet. btrfs
schreibt dorthin, Ferrite sitzt dazwischen und kommt so an jeden Write, ohne
den Kernel zu patchen — das ist die Zeile „Stock-Kernel, Engine im Userspace"
aus dem README.

Diese Zuordnung steht nirgends geschrieben, sie folgt aus der Kerninvariante:
Ein Gerät für den ganzen Pool wäre ein Striping-Layout, und dann wäre keine
Platte mehr einzeln montierbar. Auf der rohen Platte liegt das Dateisystem
entsprechend ab `payload_offset`, also 1 MiB — beim direkten Mounten braucht es
diesen Offset. **Halte die Zuordnung im Code fest, sobald sie steht**; sie
gehört zur Engine, nicht ins Formatdokument, aber sie darf nicht ungeschrieben
bleiben.

- **Read** im Normalfall durchreichen. Ist der Member an diesem Block nicht
  brauchbar (`data_is_valid_at`), aus der Parität rekonstruieren.
- **Write** zuerst als Record ins Log. Bestätigt wird, sobald der Record durable
  ist — nicht früher und nicht später.

### 4 — Schreibpfad verdrahten

`engine::WriteBatch` gibt die Reihenfolge vor, halte dich daran:

```
Logged → [OldDataRead] → DataWritten → ParityWritten → Checkpointed
```

`OldDataRead` entfällt beim Neurechnen. **Kein Checkpoint vor durabler
Parität** — der Checkpoint gibt Log-Platz frei, und wenn die Parität dann nicht
passt, rekonstruiert das Array nach dem nächsten Plattenausfall Müll.

Welches Verfahren erlaubt ist, sagt `required_parity_update`. Rate es nicht neu.

### 5 — Rebuild

`RebuildPlan` aus dem Superblock fortsetzen, Stapel rekonstruieren, **erst die
Blöcke durable schreiben, dann den Fortschritt** in den Superblock. Andersherum
meldest du nach einem Absturz Blöcke als fertig, die nie geschrieben wurden.

`integration/tests/rebuild_resume.rs` spielt genau das im Speicher durch. Der
Test auf echten Geräten muss dasselbe Ergebnis liefern.

## Was offen ist und nicht geraten wird

**Absturz im degradierten Betrieb.** Neurechnen der Parität scheitert am
fehlenden Member, Fortschreiben am nach dem Absturz unzuverlässigen alten
Inhalt. `required_parity_update` gibt dort `EngineError::CannotUpdateParity`
zurück — das ist Absicht und kein Versehen.

Wenn dir beim Bauen eine Lösung einfällt, schreib sie in den Bericht, aber
implementiere sie nicht auf Verdacht. Der Fall gehört ins Crash-Harness aus
Meilenstein 3, wo er sich nachweisen lässt statt begründen.

## Was ich nicht will

Kein FUSE, kein Pool-Namespace, keine Control plane. Das ist Meilenstein 4
aufwärts.

**Keine Mocks für fehlende Hardware.** Wenn etwas Kernelzugriff oder Root
braucht, sag es, statt eine Attrappe zu bauen. Ein Mock, der etwas anderes
testet als die Realität, ist in einem Speicherprojekt schlimmer als kein Test —
er erzeugt Vertrauen, das nicht gedeckt ist.

Loop-Geräte über `losetup` auf Sparse-Dateien sind **kein** Mock: Die
Blockschicht des Kernels ist echt, und für die Entwicklung reichen sie. Für den
Flush-Test aus Aufgabe 2 taugen sie nicht — dort misst du das Dateisystem
darunter, nicht die Platte.

Keine Formatänderung. Fällt dir eine Lücke auf, melde sie und beschreibe, welches
Feature-Bit sie bräuchte. Ab 1.0 ist der reservierte Bereich der einzige Weg,
und sein Nullwert muss das bisherige Verhalten bedeuten.

Kein SIMD in `parity/`. Gemessen ist es bereits — `cargo bench -p
ferrite-parity` liefert auf einem Kern rund 24 GB/s für P und 12 GB/s für Q,
nach dem Horner-Umbau und den Konstantentabellen. Die Paritätsrechnung ist
damit für realistische Arrays kein Engpass. Wenn die Engine etwas anderes
zeigt, bring die Messung mit; ohne sie bleibt es dabei.

## Abschluss

Kurze Zusammenfassung: was gebaut wurde, was der Flush-Test auf deiner Hardware
ergeben hat, was auf echten Geräten anders war als in der In-Memory-Generalprobe,
und ob `docs/FORMAT.md` an einer Stelle nicht ausreicht — ohne sie zu ändern.

---

Danach kommt Meilenstein 3, das Crash-Harness: `dm-flakey` und `dm-dust` für
Lesefehler und stille Korruption, Power-Fail per `SIGKILL` an zufälligen Punkten
im Schreibpfad, danach Replay und vollständige Paritätsverifikation. Ab dort in
CI, und es blockiert Merges. Meilenstein 3 steht bewusst vor den Features.

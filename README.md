# Ferrite

Ein NAS-Betriebssystem für Linux, dessen Speicher-Layer neu gebaut ist statt
neu verpackt.

Gemischte Plattengrössen, jede Platte einzeln lesbar, Selbstheilung bei
Bit-Rot, atomare Updates. Läuft bare metal wie virtualisiert.

> **Status: früh.** Das *On-Disk-Format* steht bei Version 1.0 und ist
> eingefroren. **Ferrite selbst ist es nicht** — die beiden Versionen haben
> nichts miteinander zu tun. Seit Meilenstein 2 stellt Ferrite je Data-Member
> ein Blockgerät bereit, btrfs läuft darauf, und jeder Write geht durch Log und
> Parität. Der Absturz an jedem einzelnen I/O-Punkt des Schreibpfads läuft seit
> Meilenstein 3 in CI, ebenso Lesefehler, verschluckte Writes und Bit-Rot über
> `dm-dust` und `dm-flakey`. Seit Meilenstein 4 findet ein btrfs-Scrub den
> Rost und Ferrite holt ihn aus der Parität zurück — in CI, mit echtem btrfs
> auf einem echten Ferrite-Blockgerät. **Trotzdem: Lege nichts darauf ab, wovon du nur
> eine Kopie hast.** Was fehlt, ist Betrieb auf echter Hardware über längere
> Zeit — und ein Speichersystem, das noch niemand im Alltag benutzt hat, hat
> seine unangenehmen Überraschungen noch vor sich.

## Warum

Unraids Array-Modell ist richtig: unabhängige Dateisysteme pro Platte, Parität
darüber, gemischte Grössen erlaubt, und beim Totalausfall bleiben die übrigen
Platten einzeln lesbar. Kein RAID5/6 kann das.

Vier Dinge daran sind es nicht:

| | Unraid | Ferrite |
|---|---|---|
| Bit-Rot im Array | wird still mitgeparitet | wird erkannt und repariert |
| Absturz beim Schreiben | Parität kann veralten | Write-Log, Recovery nach 5.2 |
| Kernel | gepatchter `md`-Treiber | Stock-Kernel, Engine im Userspace |
| Zustand | Config im USB-Flash | Superblöcke + deklarative Config |

**Zum Durchsatz sagt diese Tabelle bewusst nichts.** Ferrite schreibt heute im
Write-Through: Read-Modify-Write der Parität wie bei Unraid, plus einen
Log-Record. Das ist pro Write *mehr* I/O, nicht weniger. Der Vorteil käme erst
mit gebündelter Parität im Write-Back-Modus — und der ist gesperrt, bis ein
Gerät nachweislich ehrlich flusht (Abschnitt 5.3). Solange das gilt, wäre eine
Zeile über Geschwindigkeit eine Behauptung ohne Deckung.

**Selbstheilung ohne Mirror.** Jeder Data-Member trägt btrfs mit Prüfsummen.
Meldet btrfs einen korrupten Block, rekonstruiert der Repair-Broker ihn aus der
Parität und schreibt ihn zurück. Prüfsummen ohne Redundanz können nur melden,
Parität ohne Prüfsummen merkt nichts — erst die Kopplung repariert. Das steht
seit Meilenstein 4 nicht mehr nur hier, sondern läuft bei jedem Push einmal
durch: echtes btrfs, echter Rost, echter Scrub, echte Reparatur.

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
2. **Paritäts-Engine.** Reed-Solomon P+Q, Gerätezugriff und das Write-Log auf
   Platte, das ublk-Target, der Schreibpfad und der Rebuild sind fertig.
   Auch der **Doppelausfall**: Fallen zwei Datenplatten gleichzeitig aus, löst
   die Engine das Gleichungssystem aus P und Q nach beiden auf — der Gast liest
   weiter, und der Rebuild holt beide zurück. Q auf die Platte zu schreiben,
   ohne sie im Ernstfall benutzen zu können, hieße den Aufwand zu bezahlen und
   den Schutz nicht zu bekommen.
   Offen bleibt der Write-Back-Modus — er braucht ein Gerät, dessen Flush
   nachweislich ehrlich ist.
   **Braucht Linux** mit geladenem `ublk_drv`.
3. **Crash-Harness.** Der Power-Fail-Teil steht und **läuft in CI**: Der
   Schreibpfad wird an *jedem einzelnen* I/O-Punkt per `SIGKILL` abgebrochen —
   nicht an zufälligen, sondern durchgezählt, damit keine Lücke bleibt und jeder
   Fehlschlag exakt wiederholbar ist. Danach werden drei Zusagen geprüft: das
   Array lässt sich öffnen, die Parität passt nach dem Recovery zum Inhalt der
   Data-Members, und kein bestätigter Write fehlt. Ein Selbsttest weist nach,
   dass das Harness einen bekannten Fehler wirklich bemerkt — ein Harness, das
   immer grün ist, wäre eine Behauptung.
   Dazu kommt, was ein `SIGKILL` zwischen zwei Operationen nicht abdeckt:
   `dm-dust` erzeugt Lesefehler, `dm-flakey` verschluckt Writes oder verfälscht
   Bytes. Ein Lesefehler wird aus der Parität beantwortet, ein Gerät, das seinen
   Flush belogen hat, hinterlässt eine veraltete Parität — und der Scrub findet
   sie. Bit-Rot wird gefunden und aus der Parität repariert.
   **Absturz im degradierten Betrieb** ist entschieden und gemessen:
   Neurechnen scheitert am fehlenden Member, Fortschreiben an der nach dem
   Absturz unbestimmbaren Parität — der Inhalt des fehlenden Members ist für
   diese Bereiche verloren. Ferrite gibt deshalb nicht das ganze Array auf,
   sondern grenzt den Verlust ein: Die Parität wird neu gebildet, der fehlende
   Slot zählt als Nullbytes, und `recover` meldet die betroffenen Bereiche. Das
   Array bleibt offen, die übrigen Members voll nutzbar. Gemessen an allen 80
   Abbruchpunkten: 64 mit Recovery, 64 davon mit gemeldetem Verlust.
4. **Repair-Broker.** Der Teil, der Selbstheilung erst zu einer macht, und er
   **läuft in CI**: echtes btrfs auf einem Ferrite-Blockgerät, Bytes auf der
   Platte gekippt, echter `btrfs scrub` — der findet den Fehler und kann ihn
   nicht selbst beheben —, Befund aus dem Kernel-Ringpuffer gelesen, Bereich
   rekonstruiert, zurückgeschrieben, Datei wieder lesbar, zweiter Scrub sauber.
   Zwei Dinge daran sind nicht selbstverständlich:
   **Die Rekonstruktion wird gegengeprüft.** Bei Bit-Rot weiß niemand vorab,
   welche Quelle gelogen hat. Gerechnet wird deshalb aus P *und* aus Q; nur wenn
   beide Wege dasselbe ergeben, wird geschrieben. Ist es die Parität, die
   angefressen ist, meldet Ferrite das — und schreibt nichts. Wer hier nur aus P
   rekonstruierte, machte aus einem behebbaren Fehler echten Datenverlust.
   **Die Parität bleibt unberührt.** Sie ist die Quelle, nicht die Mitschrift;
   über den Schreibpfad zu gehen faltete den Rost in sie ein.
   Offen bleibt der Lesefehler zur Laufzeit: Er nennt nur den Offset in der
   Datei, nicht den auf der Platte, und ihn umzurechnen heißt, den Chunk-Baum von
   btrfs zu lesen. Bis dahin ist der Scrub der Weg — und der ist ohnehin das,
   was ein NAS regelmäßig laufen lässt.
5. **Pool-Namespace.** Ein Baum über alle Platten statt `Platte 3/Filme/`. Die
   Regeln stehen und sind vollständig geprüft, ohne dass etwas gemountet
   werden muss: wohin ein neues Objekt gehört (`MostFree`, `FillUp`,
   `RoundRobin`, jeweils deterministisch), wie tief ein Verzeichnis über
   Platten verteilt sein darf, wieviel Reserve frei bleibt, und was ein Name
   bedeutet, den zwei Platten tragen.
   Zwei Festlegungen tragen das Ganze. **Eine Datei liegt vollständig auf
   genau einer Platte** — sonst wäre die Kerninvariante hin, denn eine Datei,
   deren zweite Hälfte auf der verlorenen Platte lag, ist beim Ausbau nicht
   mehr lesbar. Und **der Pool speichert nichts**: kein Index, keine
   Zuordnungstabelle, nichts, dessen Verlust eine Platte unlesbar machte. Er
   ist eine Sicht, kein Zustand.
   Ein doppelter Dateiname wird deterministisch bedient und **gemeldet**, statt
   still nach Plattenreihenfolge aufgelöst zu werden — das ist die Ursache der
   scheinbar wiederauferstandenen Dateien, die man aus Unraid kennt.
   Der Pool ist **eingehängt und beschreibbar, und das läuft in CI**:
   `/dev/fuse` und `mount(2)` von Hand, ohne libfuse und ohne `fusermount3`,
   weil der spätere Passthrough den Datenpfad aus diesem Prozess herausnehmen
   muss und eine Bindung, die ihn selbst in der Hand hält, ihn nicht abgeben
   kann. Auflisten, lesen, schreiben, anlegen, löschen, umbenennen, Rechte und
   Zeitstempel — geprüft über `std::fs` und damit über dieselben Systemaufrufe,
   die jedes andere Programm auch benutzt. Die Platzierungsregeln greifen dabei
   wirklich: Wo eine neue Datei landet, entscheidet die Policy, und die Tests
   sehen danach auf den Platten selbst nach.
   Zwei Dinge tut der Pool anders als Unraid. **Gelöscht wird auf jeder
   Platte**, die den Namen trägt — sonst taucht die zweite Kopie beim nächsten
   Blick wieder auf. Und **beim Umbenennen wird weggeräumt, was am Ziel im Weg
   lag**: Bliebe es stehen, lieferte der Pool danach den alten Inhalt, weil die
   Platte mit dem kleineren Index zuerst bedient.
   Dabei ist eine Falle umgangen, in die vereinigende Dateisysteme regelmäßig
   treten: Zwei Platten vergeben ihre Inode-Nummern unabhängig voneinander, und
   wer sie durchreicht, zeigt zwei verschiedene Dateien mit derselben Nummer —
   `tar` und `rsync` halten sie dann für Hardlinks und speichern die zweite als
   Verweis auf die erste. Ferrite vergibt eigene, poolweit eindeutige Nummern.
   Offen bleibt der Passthrough — bis dahin geht jedes Byte durch den
   Userspace; richtig ist das Ergebnis auch so, nur langsamer. Ebenso offen:
   erweiterte Attribute und Sperren. Eine ACL, die nur auf einer von mehreren
   Platten eines Verzeichnisses liegt, gilt je nachdem, welche gerade bedient —
   das gehört entschieden, bevor es gebaut wird.
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
| `engine/` | Planung von Schreibpfad und Rebuild, Gerätezugriff, Array, Flush-Test nach 5.3, Write-Log auf Platte, ublk-Target mit btrfs darauf, Schreibpfad mit Parität, Rekonstruktion (ein **und zwei** ausgefallene Slots), Rebuild, Recovery und Reparatur mit Gegenprobe — 160 Tests grün (150 davon plattformunabhängig), dazu 9 auf Blockgeräten und 9 auf echten ublk-Geräten |
| `broker/` | Parser für die Scrub-Meldungen von btrfs, Zusammenfassung benachbarter Befunde, Zuordnung Gerät → Slot, Kernel-Ringpuffer — 23 Tests grün, alles ohne I/O prüfbar ausser dem Ringpuffer |
| `harness/` | Crash-Harness: Absturz an jedem I/O-Punkt, drei Zusagen, Selbsttest gegen einen bekannten Fehler — 5 Tests in CI. Dazu 7 für den Broker an einem echten Array, 5 gegen fehlerhafte Geräte (`dm-dust`, `dm-flakey`) und einer für die ganze Kette mit echtem btrfs und echtem Scrub — alle in CI, die letzten beiden Gruppen mit Root |
| `pool/` | Platzierung nach Allocation, Split-Tiefe und Reserve, Vereinigung mehrerer Branches, Konflikterkennung — 99 Tests grün, davon 67 dependency- und I/O-frei. Dazu die FUSE-Schale von Hand über `/dev/fuse` und `mount(2)`, Lesen und Schreiben: 24 Tests an einem echt eingehängten Pool, in CI. Passthrough, xattrs und Sperren offen |
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

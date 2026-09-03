# Ferrite On-Disk Format

**Version 1.0 — eingefroren.**

Dieses Dokument ist normativ. Der Code richtet sich nach diesem Dokument, nicht
umgekehrt.

**Was der Freeze bedeutet.** Ein Array, das von einer `1.x`-Implementierung
erstellt wurde, muss von jeder späteren `1.y`-Implementierung lesbar sein.
Eingefroren sind damit: das Byte-Layout des Superblocks (Abschnitt 4) und des
Log-Record-Headers (Abschnitt 5.1), die Bedeutung jedes Feldes, die
Gültigkeitsregeln, die Assemble-Bedingungen (Abschnitt 2.1) und der
Recovery-Algorithmus (Abschnitt 5.2). Keines davon ändert sich noch.

Erweitert wird ausschliesslich über die Feature-Bits aus Abschnitt 4.1 und über
die reservierten Bereiche, die als Null geschrieben werden. Jedes künftige
Feature bekommt sein Bit, bevor es Code bekommt — so, wie `member_state` und
`rebuild_progress` in Version 0.2 aus dem reservierten Bereich entstanden sind,
solange das noch ohne Bit ging.

Die Versionen `0.x` waren Entwürfe und durften brechen. Eine
`1.0`-Implementierung liest sie **nicht**: `version_major` MUSS `1` sein, alles
andere wird abgewiesen. Es gibt keine `0.x`-Arrays mit Nutzdaten, weil bis hier
kein Code auf eine echte Platte geschrieben hat.

Alle Mehrbyte-Ganzzahlen sind **little-endian**. Alle Offsets sind Byte-Offsets
vom Anfang des jeweiligen Blockgeräts. Alle Prüfsummen sind **CRC-32C**
(Castagnoli, Polynom `0x1EDC6F41`, reflektiert `0x82F63B78`, Init `0xFFFFFFFF`,
finales XOR `0xFFFFFFFF`).

---

## 1. Begriffe

| Begriff | Bedeutung |
|---|---|
| **Array** | Menge von Members, die gemeinsam eine Paritätsgruppe bilden. |
| **Member** | Ein einzelnes Blockgerät im Array. |
| **Role** | Funktion eines Members: `Data`, `ParityP`, `ParityQ` oder `Log`. |
| **Slot** | Index eines `Data`-Members, `0..data_slot_count`. |
| **Payload-Region** | Der Bereich eines Members, über den Parität gebildet wird. |
| **Parity-Block** | Kleinste Einheit der Paritätsberechnung, `2^parity_block_size_log2` Bytes. |

## 2. Kerninvariante

Ferrite ist **kein Striping-Layout**. Jeder Data-Member trägt ein eigenständiges,
vollständig unabhängig lesbares Dateisystem. Parität wird über *gleiche Offsets*
innerhalb der Payload-Region gebildet:

```
Für jeden Parity-Block-Index i:
    P[i] = D₀[i] ⊕ D₁[i] ⊕ … ⊕ Dₙ₋₁[i]
    Q[i] = ⨁ⱼ  gʲ · Dⱼ[i]        (GF(2⁸), g = 0x02, j = slot_index)
```

**Feldparameter.** GF(2⁸) mit dem Reduktionspolynom `x⁸ + x⁴ + x³ + x² + 1`
(`0x11D`), Generator `g = 0x02`. Das ist dasselbe Feld, das Linux `md` für
RAID6 verwendet — nicht aus Bequemlichkeit, sondern weil bestehende
SIMD-Implementierungen und Testvektoren darauf passen. Die Multiplikation ist
byteweise; ein Parity-Block ist ein Array unabhängiger GF(2⁸)-Elemente, keine
Zahl mit Übertrag.

Bei bis zu 64 Data-Slots bleiben alle `gʲ` paarweise verschieden und ungleich
null, `g` hat in diesem Feld die Ordnung 255. Mehr als 64 Slots wären nicht
prinzipiell unmöglich, sind aber nicht spezifiziert und werden abgewiesen.

**Zero-Extension-Regel.** Ist die Payload-Region eines Data-Members kürzer als
die des Parity-Members, so liest sie jenseits ihres Endes als Nullbytes. Das ist
die Regel, die gemischte Plattengrössen erlaubt, und sie ist zwingend: Eine
Implementierung, die stattdessen abbricht, produziert falsche Parität.

Daraus folgen harte Bedingungen, die beim Assemble geprüft werden müssen. Sie
stehen vollständig in Abschnitt 2.1.

**Konsequenz für den Ausfall.** Fallen mehr Members aus, als Parität abdeckt,
bleiben die verbleibenden Data-Members vollständig und einzeln montierbar. Das
ist der zentrale Vorteil gegenüber RAID5/6 und wird durch kein späteres Feature
aufgegeben.

### 2.1 Assemble

Ein Array wird aus den Superblöcken seiner Members zusammengesetzt. Vorher MUSS
geprüft werden:

1. Es gibt mindestens einen Member, und jeder einzelne erfüllt die
   Gültigkeitsregeln aus Abschnitt 4.
2. Alle Members haben identische `array_uuid`, `parity_block_size_log2` und
   `data_slot_count`.
3. Alle `member_uuid` sind paarweise verschieden. Zwei Members mit derselben
   `member_uuid` sind dieselbe Platte — etwa nach einer Kopie mit `dd`.
4. Es gibt **genau einen** Member mit `role == ParityP`, **höchstens einen** mit
   `role == ParityQ` und **höchstens einen** mit `role == Log`.
5. Es gibt genau `data_slot_count` Members mit `role == Data`, und ihre
   `slot_index` decken `0..data_slot_count` genau einmal ab.
6. `payload_size(ParityP) >= max(payload_size(Dⱼ))` für alle `j`. Ist ein
   ParityQ vorhanden, gilt dasselbe für ihn.

Aus 4 und 5 folgt, dass ein Array höchstens 67 Members hat.

Zu Regel 6: Ein Parity-Member, der kürzer ist als der längste Data-Member, hat
für die Offsets dahinter keine Parität. Dort liesse sich nichts rekonstruieren —
die Redundanz endete still mitten im Array. Die umgekehrte Richtung ist dagegen
ausdrücklich erlaubt und der Sinn der Zero-Extension-Regel: Data-Members dürfen
kürzer sein als die Parität und beliebig voneinander abweichen.

Schlägt eine dieser Prüfungen fehl, wird das Array **nicht** zusammengesetzt.
Ein Array, das mit einem fehlenden Slot läuft, rechnet Parität über eine
unvollständige Menge. Die sieht gültig aus und fällt erst auf, wenn jemand
daraus rekonstruiert — also im Ernstfall.

## 3. Geräte-Layout

Jeder Member, unabhängig von der Rolle:

```
Offset            Grösse      Inhalt
0                 65536       Reserviert (Partitionstabellen, Fremd-Superblöcke)
65536             4096        Superblock, primär
69632             1048576-69632  Reserviert
1048576           payload_size   Payload-Region  (Data / ParityP / ParityQ)
                                 bzw. Log-Region (Log)
end - 65536       4096        Superblock, Backup
```

`payload_offset` ist immer `1048576` in Version 1.0, wird aber im Superblock
geführt, damit spätere Versionen es verschieben können.

`payload_size` MUSS ein Vielfaches von `2^parity_block_size_log2` sein und darf
nicht in den Backup-Superblock hineinreichen.

Die zweite Hälfte lässt sich aus dem Superblock allein nicht prüfen: Die
Gerätegrösse steht nicht darin, sie kommt vom Blockgerät. Ausformuliert sind es
zwei Bedingungen:

1. `device_size >= 135168` — 69632 für den Bereich bis hinter den primären
   Superblock, plus 65536 für den Bereich am Geräteende. Darunter überlappen die
   beiden Superblöcke.
2. `payload_offset + payload_size <= device_size - 65536`.

Eine Implementierung, die Bedingung 2 nicht prüft, legt die Payload-Region über
den Backup-Superblock. Der erste Write auf den letzten Block zerstört dann genau
die Kopie, die für den Fall da ist, dass der primäre Superblock unlesbar wird —
und zwar unbemerkt, weil beim Schreiben nichts auffällt.

Beide Superblöcke werden bei jeder Änderung geschrieben, **primär zuerst,
Backup nach einem Flush**. Beim Lesen gilt der Superblock mit gültiger CRC und
höherer `generation`.

## 4. Superblock

Fixe Grösse 4096 Bytes.

| Offset | Grösse | Typ | Feld |
|---:|---:|---|---|
| 0 | 8 | `[u8;8]` | `magic` = `FERRITE1` |
| 8 | 2 | `u16` | `version_major` |
| 10 | 2 | `u16` | `version_minor` |
| 12 | 4 | `u32` | `header_size` = 4096 |
| 16 | 16 | `uuid` | `array_uuid` |
| 32 | 16 | `uuid` | `member_uuid` |
| 48 | 1 | `u8` | `role` (0=Data, 1=ParityP, 2=ParityQ, 3=Log) |
| 49 | 1 | `u8` | `parity_block_size_log2` |
| 50 | 2 | `u16` | `slot_index` (nur bei `role == Data` gültig) |
| 52 | 4 | `u32` | `data_slot_count` |
| 56 | 8 | `u64` | `payload_offset` |
| 64 | 8 | `u64` | `payload_size` |
| 72 | 8 | `u64` | `generation` |
| 80 | 8 | `u64` | `created_unix` |
| 88 | 8 | `u64` | `feature_compat` |
| 96 | 8 | `u64` | `feature_incompat` |
| 104 | 8 | `u64` | `feature_ro_compat` |
| 112 | 32 | `[u8;32]` | `label`, UTF-8, mit Nullbytes aufgefüllt |
| 144 | 1 | `u8` | `member_state` (0=Clean, 1=Rebuilding, 2=Stale) |
| 145 | 7 | — | Reserviert, MUSS als Null geschrieben werden |
| 152 | 8 | `u64` | `rebuild_progress` |
| 160 | 3932 | — | Reserviert, MUSS als Null geschrieben werden |
| 4092 | 4 | `u32` | `crc32c` über Bytes `0..4092` |

**Gültigkeitsregeln.** `parity_block_size_log2` ∈ `[12, 24]` (4 KiB … 16 MiB).
`data_slot_count` ∈ `[1, 64]`. Bei `role == Data` MUSS
`slot_index < data_slot_count` gelten. `payload_offset` MUSS 4096-aligned sein.
`label` MUSS gültiges UTF-8 sein, höchstens 32 Bytes lang und DARF KEIN Nullbyte
enthalten: Das Feld wird mit Nullbytes aufgefüllt, das erste Nullbyte ist damit
das Ende des Labels. Ein Label mit eingebettetem Nullbyte wäre nach einem
Schreib-Lese-Zyklus ein anderes — die Metadaten änderten sich still.

Für `member_state` und `rebuild_progress` gelten die Regeln aus Abschnitt 4.2.

### 4.1 Feature-Flags

Drei Sätze, nach ext4/btrfs-Vorbild:

- `feature_compat` — unbekannte Bits: mounten und schreiben erlaubt.
- `feature_ro_compat` — unbekannte Bits: nur lesend mounten.
- `feature_incompat` — unbekannte Bits: **verweigern**.

In Version 1.0 sind alle drei Felder `0`. Jedes künftige Format-Feature bekommt
ein Bit, bevor es Code bekommt.

### 4.2 Member-Zustand

Ein Member weiss selbst, ob sein Inhalt zur Parität des Arrays passt. Ohne
dieses Feld liesse sich eine frisch getauschte Platte nicht von einer intakten
unterscheiden: Beide tragen einen gültigen Superblock, und die Payload einer
halb wiederhergestellten Platte sieht aus wie Daten.

| Wert | Zustand | Bedeutung |
|---:|---|---|
| 0 | `Clean` | Die Payload passt zur Parität. Normalfall. |
| 1 | `Rebuilding` | Nur `[0, rebuild_progress)` ist gültig, der Rest noch nicht geschrieben. |
| 2 | `Stale` | Der Inhalt ist älter als das Array. Nichts davon ist gültig. |

**Gültigkeitsregeln.** `member_state` ∈ `[0, 2]`; jeder andere Wert wird
abgewiesen. `rebuild_progress` MUSS `0` sein, ausser bei `Rebuilding`. Bei
`Rebuilding` MUSS `rebuild_progress <= payload_size` gelten und ein Vielfaches
von `2^parity_block_size_log2` sein — rekonstruiert wird blockweise, ein
Fortschritt mitten in einem Parity-Block wäre nicht wiederaufsetzbar. Ein Member
mit `role == Log` MUSS `Clean` sein: Die Log-Region ist von keiner Parität
gedeckt, ein leeres Log ist immer zulässig, und es gibt daran nichts zu
rekonstruieren.

**Folge für die Rekonstruktion.** Ein Member, der nicht `Clean` ist, ist keine
gültige Datenquelle. Bei `Stale` gar nicht, bei `Rebuilding` nur unterhalb von
`rebuild_progress`. Wer ihn trotzdem in die Paritätsrechnung nimmt, bekommt ein
Ergebnis, das plausibel aussieht und falsch ist. Das Array bleibt trotzdem
zusammensetzbar (Abschnitt 2.1) — ein degradiertes Array muss sich öffnen
lassen, sonst wäre die Kerninvariante aus Abschnitt 2 wertlos.

**Verträglichkeit.** Beide Felder lagen bis Version 0.1 im reservierten Bereich
und wurden als Null geschrieben. Null bedeutet `Clean` mit Fortschritt `0` —
ein Array aus Version 0.1 liest sich unter den neuen Regeln unverändert
richtig. Deshalb kostet diese Änderung kein Feature-Bit, und deshalb muss sie
vor dem Einfrieren passieren: Nachträglich eingeführt bräuchte sie ein
`feature_incompat`-Bit, und jede 1.0-Implementierung würde dann jedes Array
ablehnen, das je einen Rebuild gesehen hat.

## 5. Write-Log

Das Log-Gerät ist ein Member mit `role == Log`. Seine Payload-Region ist ein
zirkulärer Puffer aus 4096-Byte-Sektoren.

Ein Write wird bestätigt, sobald sein Record im Log **durable** ist. Die
Übertragung auf den Data-Member und die Paritätsaktualisierung erfolgen danach,
gebündelt über ganze Parity-Blöcke.

### 5.1 Record-Header

Fixe Grösse 64 Bytes, gefolgt von `payload_len` Bytes Nutzdaten, aufgefüllt auf
ein Vielfaches von 4096.

| Offset | Grösse | Typ | Feld |
|---:|---:|---|---|
| 0 | 4 | `[u8;4]` | `magic` = `FLOG` |
| 4 | 2 | `u16` | `record_type` (1=Write, 2=Checkpoint, 3=reserviert, 4=Padding) |
| 6 | 2 | `u16` | `header_size` = 64 |
| 8 | 8 | `u64` | `seq`, streng monoton steigend über die Lebensdauer des Arrays |
| 16 | 8 | `u64` | `target_offset`, relativ zur Payload-Region des Ziel-Members |
| 24 | 4 | `u32` | `payload_len` |
| 28 | 2 | `u16` | `slot_index` |
| 30 | 2 | — | Reserviert |
| 32 | 8 | `u64` | `generation` |
| 40 | 8 | `u64` | `commit_unix` |
| 48 | 4 | `u32` | `payload_crc32c` |
| 52 | 8 | — | Reserviert |
| 60 | 4 | `u32` | `header_crc32c` über Bytes `0..60` |

Ein Record belegt auf der Platte immer `aufgerundet(64 + payload_len, 4096)`
Bytes, unabhängig vom `record_type`. Der Bereich zwischen dem Ende der Nutzdaten
und dem Ende des letzten belegten Sektors MUSS als Null geschrieben werden. Er
liegt sonst als Rest einer früheren Runde des Ringpuffers herum, und der Scan aus
Abschnitt 5.2 sieht jeden Sektor an — auch die, die zu den Nutzdaten eines
Records gehören.

`Padding` füllt den Rest des Puffers, wenn ein Record nicht mehr vor das Ende
passt. Sein `payload_len` zählt wie bei jedem anderen Record die Bytes **nach**
dem eigenen Header, deckt also zusammen mit ihm den Rest der Region ab. Der
nächste Record beginnt bei Offset 0 der Log-Region.

Ein `Padding`-Record nimmt an der Sequenzkette aus Abschnitt 5.2 **nicht** teil:
Er trägt keine Nutzdaten, über die sich `payload_crc32c` bilden liesse, und
verbraucht keine Sequenznummer. Sein `seq` ist für die Kette bedeutungslos. Beim
Replay wird er übersprungen, und die Wiedergabe setzt bei Offset 0 fort.

`Checkpoint` bedeutet: alle Records mit `seq <= self.seq` sind auf ihren
Data-Members und in der Parität persistent. Der Platz davor darf überschrieben
werden. Anders als `Padding` ist ein Checkpoint Teil der Kette — er hat
`payload_len == 0` und verbraucht eine Sequenznummer.

Der Wert `3` ist **reserviert** und MUSS abgelehnt werden. Es gibt bewusst
keinen Barrier-Record: Ein Write wird erst bestätigt, wenn sein Record durable
ist, und die Reihenfolge ergibt sich vollständig aus `seq`. Ein FLUSH des Gastes
verlangt damit nichts, was nicht ohnehin schon gilt, und hätte auf der Platte
keine eigene Bedeutung. Braucht eine spätere Version doch einen, bekommt sie ein
Feature-Bit nach Abschnitt 4.1 und eine ausformulierte Regel — vorher nicht.

### 5.2 Recovery

1. Alle 4096-Sektoren der Log-Region scannen, gültige Header sammeln (Magic
   korrekt **und** `header_crc32c` korrekt).
2. Den `Checkpoint` mit der höchsten `seq` bestimmen. Fehlt einer, bei der
   niedrigsten gültigen `seq` beginnen.
3. Ab dort vorwärts laufen. Ein Record wird nur akzeptiert, wenn
   `seq == vorheriges_seq + 1`, `generation` zur Superblock-Generation passt und
   `payload_crc32c` stimmt.
4. **Beim ersten Bruch dieser Kette abbrechen.** Alles danach wird verworfen,
   auch wenn einzelne spätere Records gültig aussehen — ein torn write beim
   Absturz kann alte, intakte Records aus einer früheren Runde des Ringpuffers
   sichtbar lassen.
5. Die akzeptierten Writes anwenden, dann Parität über alle berührten
   Parity-Blöcke neu berechnen, dann Checkpoint schreiben.

Schritt 4 ist der Punkt, an dem naive Implementierungen still Daten verlieren.
Er ist testpflichtig.

Zu Schritt 3 gehört eine Grenze, die in der Praxis nie erreicht wird und trotzdem
festgelegt sein muss: Ein Record mit `seq == 2^64 - 1` beendet die Kette. Er wird
noch angewendet, aber es kann keinen Nachfolger geben. Eine Implementierung, die
den Zähler stattdessen überlaufen lässt, erwartet als nächstes `seq == 0` und
akzeptiert damit den ältesten Record des Ringpuffers — derselbe stille
Datenverlust, den Schritt 4 verhindern soll.

Zu Schritt 5: Ein `Write`-Record ist nur anwendbar, wenn
`slot_index < data_slot_count` gilt und `target_offset + payload_len` die
`payload_size` des Ziel-Members nicht überschreitet. Beide Werte kommen
ungeprüft von der Platte. Eine Implementierung, die sie nicht prüft, schreibt
über das Ende der Payload-Region hinaus — im besten Fall in den
Backup-Superblock, im schlechteren irgendwohin. Ein Record, der eine der beiden
Bedingungen verletzt, ist korrupt und beendet den Replay wie jeder andere Bruch
der Kette.

### 5.3 Degradierter Betrieb ohne ehrliches Flush

Ein virtualisiertes Log-Gerät kann `FLUSH` bestätigen, ohne dass Daten die
Platte erreicht haben. Vor der ersten Nutzung MUSS die Implementierung einen
Flush-Test durchführen. Fällt er negativ aus oder ist er nicht durchführbar,
läuft das Array im **Write-Through-Modus**: Der Write wird erst bestätigt, wenn
Data-Member und Parität aktualisiert sind. Langsamer, aber korrekt.

## 6. Verhältnis zu den Dateisystem-Prüfsummen

Ferrite speichert **keine** eigenen Prüfsummen über Nutzdaten. Die Integrität
der Payload liegt beim Dateisystem des jeweiligen Data-Members (btrfs mit
`datasum`). Ferrite liefert die Redundanz, aus der ein als korrupt erkannter
Block rekonstruiert wird.

Damit hat jeder Block genau eine Instanz, die für seine Korrektheit zuständig
ist. Zwei konkurrierende Prüfsummenschichten wären nicht sicherer, sondern nur
teurer und schwerer zu debuggen.

## 7. Versionshistorie

| Version | Änderung |
|---|---|
| 0.1 | Erstentwurf. Superblock, Write-Log, Zero-Extension, Feature-Flags. |
| 0.1a | Klarstellungen, kein Layout-Wechsel. `label` darf kein Nullbyte enthalten (Abschnitt 4), gefunden vom Fuzz-Target `superblock_roundtrip`. Ein Record mit `seq == 2^64 - 1` beendet die Kette (Abschnitt 5.2), gefunden vom Fuzz-Target `chain_replay`. Die Assemble-Bedingungen stehen jetzt vollständig in Abschnitt 2.1; Bedingung 6 gilt neu auch für ParityQ. Abschnitt 5.1 legt fest, dass der Rest des letzten Sektors als Null geschrieben wird, dass `payload_len` bei `Padding` wie überall die Bytes nach dem Header zählt, und dass `Padding` nicht an der Sequenzkette teilnimmt. Abschnitt 3 formuliert die beiden Bedingungen aus, die die Gerätegrösse brauchen. Record-Typ 3 (vormals `Barrier`, nie definiert) ist reserviert und wird abgelehnt. Abschnitt 5.2 nennt die Bedingungen, unter denen ein `Write` anwendbar ist — gefunden von der In-Memory-Generalprobe in `integration/`. |
| 0.2 | `member_state` und `rebuild_progress` auf Offset 144 und 152, Abschnitt 4.2. Erster echter Layout-Wechsel: Bis 0.1 lagen beide Felder im reservierten Bereich und wurden als Null geschrieben — Null bedeutet `Clean` mit Fortschritt `0`, ein 0.1-Array liest sich damit unverändert richtig. Ohne diese Felder ist eine frisch getauschte Platte nicht von einer intakten zu unterscheiden. |
| 1.0 | **Eingefroren.** Keine Layout-Änderung gegenüber 0.2 — nur die Zusage, dass es dabei bleibt. Vorher: beide Decode-Pfade und der Recovery-Pfad gefuzzt, das Format einmal vollständig von einer In-Memory-Implementierung durchlaufen, Golden Vectors gegen stille Encoding-Änderungen eingezogen. |

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Der Speicherbedarf beim Öffnen hängt an der Grösse **eines Records**, nicht
//! an der Grösse der Log-Platte.
//!
//! # Woher dieser Test kommt
//!
//! Aus dem ersten Tag Testbetrieb, auf einer VM mit einer 8-GiB-Log-Platte:
//!
//! ```text
//! jeremy@gg-ferrite-01:~$ sudo ferrite run
//! 6 Members gefunden.
//! memory allocation of 8588820480 bytes failed
//! ```
//!
//! `DeviceLog::open` las die komplette Log-Region am Stück in einen `Vec`. Der
//! Kommentar im Modul wusste es sogar — *„die Log-Region ist so gross wie die
//! Payload-Region ihres Members, und die haelt kein Arbeitsspeicher"* — und tat
//! es zwanzig Zeilen weiter trotzdem, mit der Begründung, das sei ja nur
//! *„beim Mounten, einmal"*. Einmal reicht: Eine fehlgeschlagene Allokation ist
//! in Rust kein Fehler, den man zurückgeben kann, sondern das Ende des
//! Prozesses.
//!
//! # Warum gemessen wird und nicht auf einen Absturz gewartet
//!
//! Ein Test, der eine 8-GiB-Region anlegt und darauf hofft, dass die
//! Allokation scheitert, prüft die Ausstattung des Testrechners und nicht den
//! Code — auf einer Maschine mit genug Arbeitsspeicher wäre er grün, während
//! der Fehler unverändert dasteht.
//!
//! Deshalb zählt dieser Test mit. Der Allokator hier merkt sich die höchste je
//! gleichzeitig gehaltene Menge, und der Test verlangt eine Obergrenze. Das ist
//! die Eigenschaft, um die es geht, und sie gilt unabhängig davon, wieviel RAM
//! die Kiste hat.
//!
//! Ein eigener Testbinary, weil ein `#[global_allocator]` für alles gilt, was
//! darin läuft.

#![cfg(unix)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::fs::File;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use ferrite_engine::{DeviceLog, MemberDevice};
use ferrite_format::superblock::{Role, Superblock, DEFAULT_PAYLOAD_OFFSET};
use ferrite_format::Uuid;

/// Ein Log-Member, der weit grösser ist als jeder vertretbare Puffer.
///
/// 256 MiB und nicht 8 GiB: Die Datei ist dünn besetzt und kostet keinen
/// Plattenplatz, aber der Scan liest sie ganz — und bei 8 GiB dauerte das in
/// CI länger, als der Nachweis wert ist. Für die Aussage genügt es, dass die
/// Region zwei Zehnerpotenzen über der Grenze liegt.
const REGION: u64 = 256 << 20;
const DEVICE_SIZE: u64 = DEFAULT_PAYLOAD_OFFSET + REGION + 65_536;

/// Was `DeviceLog::open` höchstens gleichzeitig halten darf.
///
/// Der Leseblock ist ein Megabyte, dazu kommt Kleinkram. Acht Megabyte sind
/// reichlich Luft und trotzdem ein Dreissigstel der Region — die Grenze
/// unterscheidet also sicher zwischen „liest in Blöcken" und „liest alles".
const LIMIT: usize = 8 << 20;

// --- Ein Allokator, der mitzählt ------------------------------------------

static IN_USE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

struct Zaehlend;

impl Zaehlend {
    fn dazu(bytes: usize) {
        let jetzt = IN_USE.fetch_add(bytes, Ordering::Relaxed) + bytes;
        PEAK.fetch_max(jetzt, Ordering::Relaxed);
    }

    fn weg(bytes: usize) {
        IN_USE.fetch_sub(bytes, Ordering::Relaxed);
    }
}

// SAFETY: Jeder Aufruf wird unveraendert an den System-Allokator
// weitergereicht; die Zaehler sind nebenher und beeinflussen die Rueckgabe
// nicht.
unsafe impl GlobalAlloc for Zaehlend {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            Self::dazu(layout.size());
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        Self::weg(layout.size());
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let neu = unsafe { System.realloc(pointer, layout, new_size) };
        if !neu.is_null() {
            Self::weg(layout.size());
            Self::dazu(new_size);
        }
        neu
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            Self::dazu(layout.size());
        }
        pointer
    }
}

#[global_allocator]
static ALLOKATOR: Zaehlend = Zaehlend;

// --- Der Test --------------------------------------------------------------

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("ferrite-mem-{name}-{}.img", std::process::id()));
        let file = File::create(&path).expect("Datei anlegen");
        // Duenn besetzt: Die Datei belegt nichts, liest sich aber als Nullen.
        file.set_len(DEVICE_SIZE).expect("Groesse setzen");
        Scratch(path)
    }

    fn open(&self) -> MemberDevice {
        MemberDevice::open(&self.0).expect("oeffnen")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn log_superblock() -> Superblock {
    Superblock::new(
        Uuid::from_random_bytes([0x31; 16]),
        Uuid::from_random_bytes([0x32; 16]),
        Role::Log,
        4,
        REGION,
    )
}

#[test]
fn opening_a_huge_log_does_not_pull_it_into_memory() {
    let scratch = Scratch::new("gross");

    // Nicht `initialize`: Das nullt die Region und machte aus der duenn
    // besetzten Datei 256 MiB auf der Platte. Eine Region aus lauter Nullen
    // traegt keinen gueltigen Header — der Scan laeuft trotzdem ueber jeden
    // Sektor, und genau der ist der teure Teil.
    let superblock = log_superblock();

    PEAK.store(IN_USE.load(Ordering::Relaxed), Ordering::Relaxed);
    let (log, recovery) = DeviceLog::open(scratch.open(), &superblock).expect("Log oeffnen");
    let hoechststand = PEAK.load(Ordering::Relaxed);

    // Ohne Records gibt es nichts zurueckzuspielen — aber der ganze Weg ist
    // gelaufen: Scan, Checkpoint-Suche, Replay.
    assert_eq!(recovery.accepted, 0);
    assert_eq!(log.head(), 0);

    assert!(
        hoechststand < LIMIT,
        "beim Oeffnen lagen {hoechststand} Bytes gleichzeitig im Speicher, \
         erlaubt sind {LIMIT} — die Log-Region ({REGION} Bytes) landet wieder \
         am Stueck im Arbeitsspeicher"
    );
}

#[test]
fn reading_the_records_back_stays_bounded_too() {
    // Der zweite Weg in dieselbe Falle: `LogRecovery::records` lief frueher
    // ueber eine Kopie der Region, die die Recovery festhielt. Wer nur
    // `open` repariert und das vergisst, hat den Fehler halb behoben.
    let scratch = Scratch::new("records");
    let superblock = log_superblock();
    let (log, recovery) = DeviceLog::open(scratch.open(), &superblock).expect("Log oeffnen");

    PEAK.store(IN_USE.load(Ordering::Relaxed), Ordering::Relaxed);
    let mut walk = recovery.records(&log).expect("Replay");
    let mut gesehen = 0;
    while walk.next_record().is_some() {
        gesehen += 1;
    }
    let hoechststand = PEAK.load(Ordering::Relaxed);

    assert_eq!(gesehen, 0);
    assert!(
        hoechststand < LIMIT,
        "beim Zurueckspielen lagen {hoechststand} Bytes gleichzeitig im \
         Speicher, erlaubt sind {LIMIT}"
    );
}

#[test]
fn a_header_that_claims_an_absurd_record_does_not_get_a_buffer_for_it() {
    // Die zweite Haelfte derselben Fehlerklasse. `payload_len` ist ein `u32`
    // und kommt ungeprueft von der Platte: Ein angefressener Header darf
    // behaupten, sein Record sei hundert Megabyte gross. Wer ihm einen Puffer
    // dieser Groesse anlegt, hat den Fehler bloss verschoben — nur dass ihn
    // dann kein grosses Log mehr ausloest, sondern ein einziges gekipptes Bit.
    use ferrite_format::log::LogRecordHeader;
    use std::io::{Seek, SeekFrom, Write};

    let scratch = Scratch::new("absurd");
    let superblock = log_superblock();

    // Ein Header, der in sich stimmig ist — Magic und Pruefsumme passen —,
    // aber Nutzdaten von hundert Megabyte behauptet. Er liegt damit innerhalb
    // der Region und wird nicht schon als `RecordPastEnd` abgewiesen.
    let mut header = LogRecordHeader::write(1, 0, 0, &[]);
    header.payload_len = 100 << 20;
    header.generation = superblock.generation;
    {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(&scratch.0)
            .expect("oeffnen");
        file.seek(SeekFrom::Start(superblock.payload_offset))
            .expect("springen");
        file.write_all(&header.encode()).expect("Header schreiben");
    }

    PEAK.store(IN_USE.load(Ordering::Relaxed), Ordering::Relaxed);
    let geoeffnet = DeviceLog::open(scratch.open(), &superblock);
    let hoechststand = PEAK.load(Ordering::Relaxed);

    // Das Oeffnen gelingt: Ein unglaubwuerdiger Record ist ein Befund und kein
    // Grund, das Array nicht in Betrieb zu nehmen. Angewendet wird er nicht.
    let (_, recovery) = geoeffnet.expect("ein kaputter Record darf das Oeffnen nicht verhindern");
    assert_eq!(recovery.accepted, 0);

    assert!(
        hoechststand < LIMIT,
        "ein Header mit erfundener Laenge hat {hoechststand} Bytes Puffer \
         bekommen, erlaubt sind {LIMIT}"
    );
}

#[test]
fn a_log_disk_that_stops_answering_is_reported_and_not_read_as_an_end() {
    // Ein `EIO` beim Lesen der Log-Region sieht von aussen aus wie das Ende
    // des Logs: Der Replay findet keinen Header mehr und hoert auf. Wuerde das
    // so durchgehen, hiesse „die Log-Platte antwortet nicht" beim Start
    // stillschweigend „hier ist das Log zu Ende" — und der Betrieb liefe
    // weiter, auf einer Platte, die gerade stirbt. Regel 5 kennt dafuer keine
    // Ausnahme.
    //
    // Der Lesefehler wird hier ohne Root erzeugt: Das Geraet wird geoeffnet,
    // danach die Datei abgeschnitten. Die Groessenpruefung ist dann schon
    // durch, und jeder Zugriff dahinter scheitert.
    let scratch = Scratch::new("stumm");
    let superblock = log_superblock();
    let device = scratch.open();

    File::options()
        .write(true)
        .open(&scratch.0)
        .expect("oeffnen")
        .set_len(DEFAULT_PAYLOAD_OFFSET)
        .expect("abschneiden");

    let fehler = DeviceLog::open(device, &superblock)
        .expect_err("eine Log-Platte, die nicht antwortet, darf nicht als leeres Log durchgehen");

    // Welcher Fehler genau, ist Sache des Betriebssystems — dass es einer ist
    // und keiner verschluckt wurde, ist der Punkt.
    let gesagt = format!("{fehler:?}");
    assert!(
        !gesagt.is_empty(),
        "der Fehler kam ohne Begruendung zurueck"
    );
}

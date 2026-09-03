// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Flush-Test fuer das Log-Geraet, `docs/FORMAT.md` Abschnitt 5.3.
//!
//! # Was dieser Test kann und was nicht
//!
//! Ein Log-Geraet beantwortet `FLUSH` ehrlich, wenn nach der Bestaetigung ein
//! Stromausfall die Daten nicht mehr verliert. **Das laesst sich aus dem
//! Userspace eines laufenden Systems nicht nachweisen.** Der einzige Beweis ist,
//! den Strom tatsaechlich abzuschalten und danach nachzusehen — das ist das
//! Crash-Harness aus Meilenstein 3, nicht dieser Test hier.
//!
//! Daraus folgt die Form: Dieser Test ist **asymmetrisch**. Er kann Ehrlichkeit
//! widerlegen, aber kaum belegen. Entsprechend ist [`FlushVerdict::Undecidable`]
//! die Vorgabe und nicht der Ausnahmefall, und nach Abschnitt 5.3 fuehrt sie
//! zum selben Ergebnis wie ein negativer Ausgang: Write-Through.
//!
//! Ein Flush-Test, der im Zweifel „ehrlich" sagt, ist schlimmer als keiner — er
//! erzeugt Vertrauen, das nicht gedeckt ist, und zwar genau in dem Moment, in
//! dem jemand entscheidet, ob ein Write frueh bestaetigt werden darf.
//!
//! # Was verworfen wurde
//!
//! **Zeitmessung.** Ein `FLUSH`, das nach 4 MiB Schreiblast in 20 µs
//! zurueckkommt, ist auf einer drehenden Platte physikalisch unmoeglich — auf
//! einer NVMe mit Power-Loss-Protection dagegen voellig normal. Die Messung
//! trennt also nicht Ehrlichkeit von Luege, sondern schnelle Geraete von
//! langsamen. Als Grundlage einer Entscheidung waere sie eine Muenze, die
//! aussieht wie eine Messung.
//!
//! **Erfolgreiches `sync_data` als Beleg.** Genau das ist die Luege, um die es
//! in Abschnitt 5.3 geht. Ein virtualisiertes Geraet bestaetigt sie sofort.
//!
//! # Was gilt
//!
//! Die Entscheidung faellt [`judge`] — eine reine Funktion ueber [`DeviceFacts`].
//! Das Zusammentragen der Fakten ist plattformabhaengig und steht getrennt
//! davon in [`collect_facts`]. So laesst sich jede Kombination pruefen, auch
//! die, fuer die hier keine Platte steht.

use std::path::Path;

use crate::device::MemberDevice;
use crate::error::Result;

/// Was fuer eine Art Geraet der Log-Member ist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    /// Ein echtes Blockgeraet.
    BlockDevice,
    /// Ein Loop-Geraet. Die Blockschicht darueber ist echt, die Dauerhaftigkeit
    /// darunter gehoert dem Dateisystem — hier misst man nicht die Platte.
    LoopDevice,
    /// Eine gewoehnliche Datei. Wie beim Loop-Geraet entscheidet das
    /// Dateisystem darunter, nicht dieser Member.
    RegularFile,
    /// Nicht feststellbar.
    Unknown,
}

/// Was der Kernel ueber den Schreibcache des Geraets sagt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteCache {
    /// Fluechtiger Schreibcache vorhanden. Ein `FLUSH` muss ihn leeren — ob es
    /// das tut, sagt diese Angabe nicht.
    WriteBack,
    /// Kein fluechtiger Schreibcache. Es gibt nichts zu verlieren, was ein
    /// `FLUSH` retten muesste.
    WriteThrough,
}

/// Alles, was ueber das Geraet zusammengetragen wurde.
///
/// Kommt als Wert in [`judge`] herein und nicht aus der Umgebung — aus
/// demselben Grund, aus dem `parity` seinen Zufall als Parameter bekommt: Eine
/// Entscheidung, die von der Maschine abhaengt, auf der sie faellt, laesst sich
/// nicht pruefen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceFacts {
    pub kind: DeviceKind,
    /// `None`, wenn der Kernel dazu nichts sagt.
    pub write_cache: Option<WriteCache>,
    /// Laeuft das System virtualisiert? `None`, wenn nicht feststellbar.
    ///
    /// Ist es virtualisiert, kann jede Angabe des Geraets die Angabe des
    /// Hypervisors sein und nicht die der Platte darunter. Das ist der Fall,
    /// den Abschnitt 5.3 beschreibt.
    pub virtualized: Option<bool>,
    /// Hat `sync_data` fehlerfrei geantwortet?
    ///
    /// Ein Fehler ist ein Beweis gegen das Geraet. Ein Erfolg ist keiner dafuer.
    pub flush_succeeded: bool,
    /// Ergebnis der Schreibprobe aus [`probe_write_path`], falls sie lief.
    ///
    /// `Some(false)` heisst: Was geschrieben und geflusht wurde, kam nicht
    /// zurueck. Das ist ein Beweis gegen das Geraet.
    pub write_read_back: Option<bool>,
}

/// Ergebnis des Tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlushVerdict {
    /// Nachgewiesenermassen unbrauchbar als Log-Geraet.
    Refused,
    /// Kein Nachweis in die eine oder andere Richtung moeglich.
    ///
    /// Der Normalfall. Nach Abschnitt 5.3 wie ein negativer Ausgang zu
    /// behandeln.
    Undecidable,
    /// Es gibt nichts zu verlieren: Der Kernel meldet keinen fluechtigen
    /// Schreibcache, das Geraet ist ein echtes Blockgeraet, und das System
    /// laeuft nicht virtualisiert.
    ///
    /// Das ist der einzige Weg zu diesem Ergebnis, und es bleibt eine Angabe
    /// des Geraets. Wer sie hart nachweisen will, braucht das Crash-Harness.
    Honest,
}

/// Wie ein Write bestaetigt werden darf, Abschnitt 5.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteMode {
    /// Bestaetigung, sobald der Log-Record durable ist.
    WriteBack,
    /// Bestaetigung erst, wenn Data-Member und Paritaet aktualisiert sind.
    /// Langsamer, aber korrekt.
    WriteThrough,
}

/// Das Ergebnis samt Begruendung und den Fakten, auf denen es beruht.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlushCheck {
    pub verdict: FlushVerdict,
    pub reason: &'static str,
    pub facts: DeviceFacts,
}

impl FlushCheck {
    /// Der Betriebsmodus, der sich aus dem Ergebnis ergibt.
    ///
    /// Nur [`FlushVerdict::Honest`] erlaubt Write-Back. Abschnitt 5.3 stellt
    /// „faellt negativ aus" und „ist nicht durchfuehrbar" ausdruecklich gleich.
    pub fn write_mode(&self) -> WriteMode {
        match self.verdict {
            FlushVerdict::Honest => WriteMode::WriteBack,
            FlushVerdict::Refused | FlushVerdict::Undecidable => WriteMode::WriteThrough,
        }
    }
}

/// Entscheidet aus den Fakten. Rein, ohne I/O, ohne Umgebung.
pub fn judge(facts: &DeviceFacts) -> FlushCheck {
    let (verdict, reason) = decide(facts);
    FlushCheck {
        verdict,
        reason,
        facts: facts.clone(),
    }
}

fn decide(facts: &DeviceFacts) -> (FlushVerdict, &'static str) {
    // Zuerst die Beweise gegen das Geraet. Wer ein `FLUSH` mit einem Fehler
    // beantwortet oder Geschriebenes nicht zurueckgibt, ist als Log-Geraet
    // erledigt, unabhaengig von allem anderen.
    if !facts.flush_succeeded {
        return (
            FlushVerdict::Refused,
            "das Geraet beantwortet FLUSH mit einem Fehler",
        );
    }
    if facts.write_read_back == Some(false) {
        return (
            FlushVerdict::Refused,
            "geschriebene und geflushte Daten kamen nicht zurueck",
        );
    }

    // Ab hier gibt es nichts mehr zu widerlegen, sondern nur noch zu belegen —
    // und dafuer muessen alle vier Bedingungen zugleich gelten.
    match facts.kind {
        DeviceKind::LoopDevice => {
            return (
                FlushVerdict::Undecidable,
                "Loop-Geraet: gemessen wuerde das Dateisystem darunter, nicht die Platte",
            )
        }
        DeviceKind::RegularFile => {
            return (
                FlushVerdict::Undecidable,
                "gewoehnliche Datei: die Dauerhaftigkeit gehoert dem Dateisystem darunter",
            )
        }
        DeviceKind::Unknown => {
            return (
                FlushVerdict::Undecidable,
                "die Art des Geraets ist nicht feststellbar",
            )
        }
        DeviceKind::BlockDevice => {}
    }

    if facts.virtualized != Some(false) {
        return (
            FlushVerdict::Undecidable,
            "virtualisiert oder nicht feststellbar: jede Angabe kann die des Hypervisors sein",
        );
    }

    match facts.write_cache {
        Some(WriteCache::WriteThrough) => (
            FlushVerdict::Honest,
            "kein fluechtiger Schreibcache: es gibt nichts, was ein FLUSH retten muesste",
        ),
        Some(WriteCache::WriteBack) => (
            FlushVerdict::Undecidable,
            "fluechtiger Schreibcache vorhanden; ob FLUSH ihn leert, zeigt erst ein Stromausfall",
        ),
        None => (
            FlushVerdict::Undecidable,
            "der Kernel sagt nichts ueber den Schreibcache des Geraets",
        ),
    }
}

/// Fuehrt den Test durch: Fakten sammeln, dann [`judge`].
///
/// Schreibt nichts. Die Schreibprobe ist [`probe_write_path`] und muss
/// ausdruecklich angefordert werden, weil sie den Bereich zerstoert, auf den
/// sie zeigt.
pub fn check_flush(device: &MemberDevice, write_read_back: Option<bool>) -> FlushCheck {
    let mut facts = collect_facts(device.path());
    facts.flush_succeeded = device.flush().is_ok();
    facts.write_read_back = write_read_back;
    judge(&facts)
}

/// Schreibprobe: Muster schreiben, flushen, zuruecklesen.
///
/// **Zerstoert den Bereich `offset .. offset + 4096`.** Der Aufrufer muss ihn
/// besitzen — beim Anlegen eines Arrays ist das die Log-Region, die ohnehin
/// gleich initialisiert wird.
///
/// Ein Fehlschlag ist ein Beweis gegen das Geraet. Ein Erfolg ist keiner dafuer:
/// Gelesen wird moeglicherweise aus dem Seitencache, und ueber den fluechtigen
/// Cache des Geraets sagt er ohnehin nichts. Genau deshalb geht das Ergebnis
/// als `Option<bool>` in [`DeviceFacts`] ein und nicht als Beleg.
pub fn probe_write_path(device: &MemberDevice, offset: u64) -> Result<bool> {
    // Ein Muster, das sich von Nullen, Einsen und einem stehengebliebenen
    // Superblock unterscheidet.
    let mut pattern = [0u8; 4096];
    for (index, byte) in pattern.iter_mut().enumerate() {
        *byte = (index as u8) ^ 0x5A;
    }

    device.write_at(offset, &pattern)?;
    device.flush()?;

    let mut read_back = [0u8; 4096];
    device.read_at(offset, &mut read_back)?;
    Ok(read_back == pattern)
}

// --- Fakten sammeln -------------------------------------------------------

/// Traegt zusammen, was die Plattform ueber das Geraet hergibt.
///
/// `flush_succeeded` steht hier auf `true` und wird von [`check_flush`]
/// ueberschrieben — diese Funktion fasst das Geraet nicht an.
#[cfg(target_os = "linux")]
pub fn collect_facts(path: &Path) -> DeviceFacts {
    DeviceFacts {
        kind: device_kind(path),
        write_cache: read_write_cache(path),
        virtualized: Some(is_virtualized()),
        flush_succeeded: true,
        write_read_back: None,
    }
}

/// Ausserhalb von Linux gibt es keine dieser Angaben.
///
/// Das Ergebnis ist damit immer [`FlushVerdict::Undecidable`] und nach
/// Abschnitt 5.3 Write-Through. Das ist die richtige Antwort und keine Luecke:
/// Ferrite laeuft auf Linux, und wer den Code anderswo uebersetzt, bekommt die
/// vorsichtige Variante.
#[cfg(not(target_os = "linux"))]
pub fn collect_facts(_path: &Path) -> DeviceFacts {
    DeviceFacts {
        kind: DeviceKind::Unknown,
        write_cache: None,
        virtualized: None,
        flush_succeeded: true,
        write_read_back: None,
    }
}

#[cfg(target_os = "linux")]
fn device_kind(path: &Path) -> DeviceKind {
    let Some(name) = sys_block_name(path) else {
        return DeviceKind::RegularFile;
    };
    // Ein Loop-Geraet traegt ein `loop`-Verzeichnis mit der Hintergrunddatei.
    if Path::new(&format!("/sys/class/block/{name}/loop")).exists() {
        return DeviceKind::LoopDevice;
    }
    DeviceKind::BlockDevice
}

/// Name unter `/sys/class/block`, falls der Pfad ein Blockgeraet ist.
#[cfg(target_os = "linux")]
fn sys_block_name(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    // Kein `stat` auf den Geraetetyp — ohne `libc` gibt es den Modus nicht.
    // Der Eintrag unter `/sys/class/block` ist die Auskunft des Kernels selbst
    // und genauer als ein Pfadvergleich: Er existiert genau fuer Blockgeraete.
    Path::new(&format!("/sys/class/block/{name}"))
        .exists()
        .then(|| name.to_string())
}

#[cfg(target_os = "linux")]
fn read_write_cache(path: &Path) -> Option<WriteCache> {
    let name = sys_block_name(path)?;
    // Bei einer Partition haengt die Queue am uebergeordneten Geraet.
    let contents = std::fs::read_to_string(format!("/sys/class/block/{name}/queue/write_cache"))
        .or_else(|_| {
            std::fs::read_to_string(format!("/sys/class/block/{name}/../queue/write_cache"))
        })
        .ok()?;

    match contents.trim() {
        "write back" => Some(WriteCache::WriteBack),
        "write through" => Some(WriteCache::WriteThrough),
        _ => None,
    }
}

/// Laeuft das System virtualisiert?
///
/// Im Zweifel `true`. Ein falsches „nein" fuehrt zu Write-Back auf einem
/// Geraet, das den Flush womoeglich nur behauptet; ein falsches „ja" kostet
/// Geschwindigkeit. Die beiden Fehler sind nicht gleich teuer.
#[cfg(target_os = "linux")]
fn is_virtualized() -> bool {
    // WSL und andere Kernel, die sich im Namen zu erkennen geben.
    if let Ok(release) = std::fs::read_to_string("/proc/sys/kernel/osrelease") {
        let release = release.to_ascii_lowercase();
        if release.contains("microsoft") || release.contains("wsl") {
            return true;
        }
    }
    // Ein Hypervisor, der sich beim Kernel gemeldet hat.
    if Path::new("/sys/hypervisor/type").exists() {
        return true;
    }
    if let Ok(vendor) = std::fs::read_to_string("/sys/class/dmi/id/sys_vendor") {
        let vendor = vendor.to_ascii_lowercase();
        for known in [
            "qemu",
            "vmware",
            "innotek",
            "xen",
            "parallels",
            "bochs",
            "microsoft corporation",
            "amazon ec2",
            "google",
            "alibaba",
        ] {
            if vendor.contains(known) {
                return true;
            }
        }
        // Ein gelesener, unverdaechtiger DMI-Hersteller ist die einzige
        // Auskunft, die hier zu einem „nein" fuehrt.
        return false;
    }
    true
}

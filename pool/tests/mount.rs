// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Der Pool eingehaengt, mit einem echten Kernel dahinter.
//!
//! Die uebrigen Tests pruefen die Regeln. Dieser prueft, ob aus den Regeln ein
//! Dateisystem wird: einhaengen, auflisten, lesen, `df`, aushaengen — mit
//! `std::fs` und damit ueber dieselben Systemaufrufe, die jedes andere
//! Programm auch benutzt.
//!
//! Braucht Linux, `/dev/fuse` und das Recht einzuhaengen.

#![cfg(target_os = "linux")]

use std::collections::BTreeSet;
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::FileExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

use ferrite_pool::fuse::{BranchRoot, Connection, Counters, MountOptions, PoolFs};
use ferrite_pool::{Allocation, BranchId, SharePolicy, SplitDepth};

/// Ein Arbeitsverzeichnis samt Einhaengepunkt, das sich selbst wegraeumt.
struct Workspace(PathBuf);

impl Workspace {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("ferrite-pool-{name}-{}", std::process::id()));
        let _ = umount(&path.join("mount"));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(path.join("mount")).expect("Einhaengepunkt anlegen");
        Workspace(path)
    }

    fn branch_root(&self, index: u16) -> PathBuf {
        self.0.join(format!("branch{index}"))
    }

    fn mountpoint(&self) -> PathBuf {
        self.0.join("mount")
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = umount(&self.mountpoint());
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn umount(path: &Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let target = CString::new(path.as_os_str().as_bytes())?;
    // `MNT_DETACH`: Der Test soll nicht daran haengenbleiben, dass noch
    // jemand im Verzeichnis steht.
    if unsafe { libc::umount2(target.as_ptr(), libc::MNT_DETACH) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

/// `None` mit Begruendung, wenn etwas fehlt. Kein stiller Erfolg.
fn prerequisites() -> Option<()> {
    if !is_root() {
        eprintln!("uebersprungen: braucht das Recht einzuhaengen");
        return None;
    }
    if !Path::new("/dev/fuse").exists() {
        eprintln!("uebersprungen: /dev/fuse fehlt — fuse-Modul nicht geladen");
        return None;
    }
    Some(())
}

/// Ein eingehaengter Pool, der beim Fallenlassen wieder verschwindet.
struct Mounted {
    mountpoint: PathBuf,
    worker: Option<std::thread::JoinHandle<()>>,
    counters: std::sync::Arc<Counters>,
}

impl Mounted {
    /// Haengt ein und startet die Schleife in einem eigenen Thread.
    ///
    /// Eingehaengt wird **vor** dem Start des Threads: Danach ist der
    /// Einhaengepunkt live, und der Test kann ohne Wettlauf losgehen.
    fn start(workspace: &Workspace, branches: Vec<BranchRoot>) -> Self {
        Self::with_policy(workspace, branches, SharePolicy::default())
    }

    /// Wie [`Mounted::start`], aber ohne Passthrough: Lesen und Schreiben
    /// laufen dann durch diesen Prozess und nicht am ihm vorbei.
    fn plain(workspace: &Workspace, branches: Vec<BranchRoot>) -> Self {
        Self::mount(workspace, branches, SharePolicy::default(), false, true)
    }

    /// Wie [`Mounted::start`], aber ohne ausgehandelte POSIX-ACLs.
    fn without_acls(workspace: &Workspace, branches: Vec<BranchRoot>) -> Self {
        Self::mount(workspace, branches, SharePolicy::default(), true, false)
    }

    fn with_policy(workspace: &Workspace, branches: Vec<BranchRoot>, policy: SharePolicy) -> Self {
        Self::mount(workspace, branches, policy, true, true)
    }

    fn mount(
        workspace: &Workspace,
        branches: Vec<BranchRoot>,
        policy: SharePolicy,
        passthrough: bool,
        posix_acl: bool,
    ) -> Self {
        let mountpoint = workspace.mountpoint();
        let connection =
            Connection::mount(&mountpoint, &MountOptions::default()).expect("Pool einhaengen");

        // Der Server zieht in den Thread um, die Zaehler bleiben hier:
        // sonst gaebe es nach dem Start keinen Weg mehr an sie heran.
        let mut filesystem = PoolFs::new(branches, policy);
        if !passthrough {
            filesystem = filesystem.without_passthrough();
        }
        if !posix_acl {
            filesystem = filesystem.without_posix_acl();
        }
        let counters = filesystem.counters();

        let worker = std::thread::spawn(move || {
            if let Err(error) = filesystem.run(&connection) {
                eprintln!("die Schleife ist gestolpert: {error}");
            }
        });

        Mounted {
            mountpoint,
            worker: Some(worker),
            counters,
        }
    }

    fn path(&self, relative: &str) -> PathBuf {
        if relative.is_empty() {
            self.mountpoint.clone()
        } else {
            self.mountpoint.join(relative)
        }
    }
}

impl Drop for Mounted {
    fn drop(&mut self) {
        // Aushaengen laesst `read` auf `/dev/fuse` mit `ENODEV` zurueckkommen,
        // und daran endet die Schleife. Ohne das liefe der Thread weiter und
        // der Test haengte im `join`.
        let _ = umount(&self.mountpoint);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("Verzeichnis anlegen");
    }
    std::fs::write(path, content).expect("Datei anlegen");
}

/// Zwei Branches mit einem ueberlappenden Verzeichnis.
fn two_branches(workspace: &Workspace) -> Vec<BranchRoot> {
    let zero = workspace.branch_root(0);
    let one = workspace.branch_root(1);

    write(&zero.join("Filme/eins.mkv"), "auf Platte null");
    write(&zero.join("nur-null.txt"), "nur hier");
    write(&one.join("Filme/zwei.mkv"), "auf Platte eins");
    write(&one.join("Musik/lied.flac"), "klingt gut");

    vec![
        BranchRoot::new(BranchId(0), zero),
        BranchRoot::new(BranchId(1), one),
    ]
}

fn names_in(path: &Path) -> BTreeSet<String> {
    std::fs::read_dir(path)
        .expect("auflisten")
        .map(|entry| {
            entry
                .expect("Eintrag")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect()
}

#[test]
#[ignore = "braucht Linux, /dev/fuse und das Recht einzuhaengen"]
fn a_mounted_pool_shows_the_union_of_its_branches() {
    if prerequisites().is_none() {
        return;
    }
    let workspace = Workspace::new("vereinigung");
    let pool = Mounted::start(&workspace, two_branches(&workspace));

    // Die Wurzel: Eintraege von beiden Platten, jeder einmal.
    assert_eq!(
        names_in(&pool.path("")),
        ["Filme", "Musik", "nur-null.txt"]
            .iter()
            .map(|name| name.to_string())
            .collect::<BTreeSet<_>>()
    );

    // `Filme` liegt auf beiden Platten. Der Pool zeigt ein Verzeichnis mit
    // dem Inhalt von beiden — das ist der Grund, warum es ihn gibt.
    assert_eq!(
        names_in(&pool.path("Filme")),
        ["eins.mkv", "zwei.mkv"]
            .iter()
            .map(|name| name.to_string())
            .collect::<BTreeSet<_>>()
    );
}

#[test]
#[ignore = "braucht Linux, /dev/fuse und das Recht einzuhaengen"]
fn a_file_is_read_from_the_branch_that_carries_it() {
    if prerequisites().is_none() {
        return;
    }
    let workspace = Workspace::new("lesen");
    let pool = Mounted::start(&workspace, two_branches(&workspace));

    assert_eq!(
        std::fs::read_to_string(pool.path("Filme/eins.mkv")).expect("lesen"),
        "auf Platte null"
    );
    assert_eq!(
        std::fs::read_to_string(pool.path("Filme/zwei.mkv")).expect("lesen"),
        "auf Platte eins"
    );
    assert_eq!(
        std::fs::read_to_string(pool.path("Musik/lied.flac")).expect("lesen"),
        "klingt gut"
    );
}

#[test]
#[ignore = "braucht Linux, /dev/fuse und das Recht einzuhaengen"]
fn a_large_file_comes_back_byte_for_byte() {
    // Der Kernel holt eine grosse Datei in mehreren Stuecken. Ein Test mit
    // fuenfzehn Bytes bemerkte einen Fehler im Offset nie.
    if prerequisites().is_none() {
        return;
    }
    let workspace = Workspace::new("gross");
    let root = workspace.branch_root(0);
    let content: Vec<u8> = (0..(3 << 20)).map(|index| (index % 251) as u8).collect();
    std::fs::create_dir_all(&root).expect("Branch anlegen");
    std::fs::write(root.join("gross.bin"), &content).expect("Datei anlegen");

    let pool = Mounted::start(&workspace, vec![BranchRoot::new(BranchId(0), root)]);
    let read_back = std::fs::read(pool.path("gross.bin")).expect("lesen");
    assert_eq!(read_back.len(), content.len());
    assert!(read_back == content, "der Inhalt kam veraendert zurueck");
}

#[test]
#[ignore = "braucht Linux, /dev/fuse und das Recht einzuhaengen"]
fn two_files_on_two_branches_never_look_like_a_hardlink() {
    // Der gefaehrlichste Fehler eines vereinigenden Dateisystems: Zwei
    // Platten vergeben ihre Inode-Nummern unabhaengig voneinander, und wer sie
    // durchreicht, zeigt zwei verschiedene Dateien mit derselben Nummer.
    // `tar` und `rsync` erkennen daran Hardlinks und speichern die zweite
    // Datei als Verweis auf die erste — beim Auspacken stuende dort der
    // falsche Inhalt.
    if prerequisites().is_none() {
        return;
    }
    let workspace = Workspace::new("inodes");

    // Zwei **eigene** Dateisysteme, und das ist der Kern des Tests: Laegen
    // beide Branches im selben, haetten ihre Dateien ohnehin verschiedene
    // Inode-Nummern, und der Test bliebe gruen, egal was der Server liefert.
    // Auf zwei frischen tmpfs bekommt die erste Datei jeweils dieselbe Zahl.
    let zero = workspace.branch_root(0);
    let one = workspace.branch_root(1);
    let Some(_mounts) = Tmpfs::pair(&zero, &one) else {
        return;
    };
    write(&zero.join("auf-null.txt"), "eins");
    write(&one.join("auf-eins.txt"), "zwei");

    // Nachweis, dass die Falle wirklich aufgestellt ist. Ohne ihn koennte der
    // Test darunter gruen sein, weil die beiden tmpfs zufaellig verschieden
    // zaehlen.
    assert_eq!(
        std::fs::metadata(zero.join("auf-null.txt")).unwrap().ino(),
        std::fs::metadata(one.join("auf-eins.txt")).unwrap().ino(),
        "die beiden Dateisysteme vergeben verschiedene Nummern — der Test prueft nichts"
    );

    let pool = Mounted::start(
        &workspace,
        vec![
            BranchRoot::new(BranchId(0), zero),
            BranchRoot::new(BranchId(1), one),
        ],
    );

    let mut seen = BTreeSet::new();
    for relative in ["auf-null.txt", "auf-eins.txt"] {
        let ino = std::fs::metadata(pool.path(relative)).expect("stat").ino();
        assert!(
            seen.insert(ino),
            "{relative} teilt sich eine Inode-Nummer mit einer anderen Datei"
        );
    }
}

/// Zwei frisch eingehaengte tmpfs, die sich selbst wieder abbauen.
struct Tmpfs(Vec<PathBuf>);

impl Tmpfs {
    fn pair(first: &Path, second: &Path) -> Option<Self> {
        Self::sized(&[(first, 16), (second, 16)])
    }

    /// Haengt je Pfad ein tmpfs der angegebenen Groesse in Mebibyte ein.
    ///
    /// Verschiedene Groessen sind der einzige Weg, die Platzierungsregeln
    /// gegen echte Dateisysteme zu pruefen: `MostFree` braucht Platten, die
    /// sich im freien Platz wirklich unterscheiden.
    fn sized(targets: &[(&Path, u64)]) -> Option<Self> {
        let mut mounted: Vec<PathBuf> = Vec::new();
        for (target, megabytes) in targets {
            std::fs::create_dir_all(target).expect("Branch-Wurzel anlegen");
            if mount_tmpfs(target, *megabytes).is_err() {
                eprintln!("uebersprungen: tmpfs laesst sich nicht einhaengen");
                for done in &mounted {
                    let _ = umount(done);
                }
                return None;
            }
            mounted.push(target.to_path_buf());
        }
        Some(Tmpfs(mounted))
    }
}

impl Drop for Tmpfs {
    fn drop(&mut self) {
        for target in &self.0 {
            let _ = umount(target);
        }
    }
}

fn mount_tmpfs(target: &Path, megabytes: u64) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let source = CString::new("tmpfs")?;
    let fstype = CString::new("tmpfs")?;
    let target = CString::new(target.as_os_str().as_bytes())?;
    let options = CString::new(format!("size={megabytes}m"))?;
    let result = unsafe {
        libc::mount(
            source.as_ptr(),
            target.as_ptr(),
            fstype.as_ptr(),
            0,
            options.as_ptr().cast(),
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[test]
#[ignore = "braucht Linux, /dev/fuse und das Recht einzuhaengen"]
fn the_same_file_keeps_its_inode_number_across_calls() {
    // Der Kernel darf dieselbe Nodeid nur fuer dasselbe Objekt bekommen.
    // Waere sie je Aufruf neu, zeigte ein offener Deskriptor nach kurzer Zeit
    // auf etwas anderes.
    if prerequisites().is_none() {
        return;
    }
    let workspace = Workspace::new("stabil");
    let pool = Mounted::start(&workspace, two_branches(&workspace));

    let first = std::fs::metadata(pool.path("Filme/eins.mkv"))
        .unwrap()
        .ino();
    let _ = names_in(&pool.path("Filme"));
    let second = std::fs::metadata(pool.path("Filme/eins.mkv"))
        .unwrap()
        .ino();
    assert_eq!(first, second);
}

#[test]
#[ignore = "braucht Linux, /dev/fuse und das Recht einzuhaengen"]
fn a_symlink_stays_a_symlink() {
    // Wer beim Nachschlagen dem Symlink folgte, machte aus einem Verweis auf
    // `/etc` ein Loch aus dem Pool heraus.
    if prerequisites().is_none() {
        return;
    }
    let workspace = Workspace::new("symlink");
    let root = workspace.branch_root(0);
    write(&root.join("ziel.txt"), "ich bin das Ziel");
    std::os::unix::fs::symlink("ziel.txt", root.join("verweis")).expect("Symlink anlegen");

    let pool = Mounted::start(&workspace, vec![BranchRoot::new(BranchId(0), root)]);

    let meta = std::fs::symlink_metadata(pool.path("verweis")).expect("lstat");
    assert!(
        meta.file_type().is_symlink(),
        "der Verweis wurde aufgeloest"
    );
    assert_eq!(
        std::fs::read_link(pool.path("verweis")).expect("readlink"),
        Path::new("ziel.txt")
    );
    assert_eq!(
        std::fs::read_to_string(pool.path("verweis")).expect("ueber den Verweis lesen"),
        "ich bin das Ziel"
    );
}

#[test]
#[ignore = "braucht Linux, /dev/fuse und das Recht einzuhaengen"]
fn the_same_name_on_two_branches_is_served_from_the_lower_slot() {
    // Ferrite versteckt den Konflikt nicht, aber es beantwortet ihn immer
    // gleich. Eine Antwort, die von der Lesereihenfolge abhinge, waere
    // schlimmer als beides.
    if prerequisites().is_none() {
        return;
    }
    let workspace = Workspace::new("doppelt");
    let zero = workspace.branch_root(0);
    let one = workspace.branch_root(1);
    write(&zero.join("gleich.txt"), "von Platte null");
    write(&one.join("gleich.txt"), "von Platte eins");

    let pool = Mounted::start(
        &workspace,
        vec![
            BranchRoot::new(BranchId(1), one),
            BranchRoot::new(BranchId(0), zero),
        ],
    );

    assert_eq!(
        names_in(&pool.path("")).len(),
        1,
        "der Name erscheint einmal"
    );
    for _ in 0..3 {
        assert_eq!(
            std::fs::read_to_string(pool.path("gleich.txt")).expect("lesen"),
            "von Platte null",
            "der bedienende Branch haengt nicht an der Reihenfolge der Liste"
        );
    }
}

#[test]
#[ignore = "braucht Linux, /dev/fuse und das Recht einzuhaengen"]
fn the_free_space_is_the_sum_over_the_branches() {
    if prerequisites().is_none() {
        return;
    }
    let workspace = Workspace::new("platz");
    let pool = Mounted::start(&workspace, two_branches(&workspace));

    let raw = statfs(&pool.path(""));
    let single = statfs(&workspace.branch_root(0));

    // Beide Branches liegen im selben Dateisystem, der Pool zeigt also rund
    // das Doppelte. Genau kann es nicht sein — zwischen den beiden Aufrufen
    // schreibt der Rechner weiter.
    let pool_total = raw.f_blocks * raw.f_frsize;
    let branch_total = single.f_blocks * single.f_frsize;
    assert!(
        pool_total > branch_total,
        "der Pool ({pool_total}) ist nicht groesser als ein Branch ({branch_total})"
    );
    assert!(
        pool_total <= branch_total * 2 + branch_total / 100,
        "der Pool ({pool_total}) ist mehr als die Summe seiner Branches"
    );
}

fn statfs(path: &Path) -> libc::statvfs {
    use std::os::unix::ffi::OsStrExt;
    let target = CString::new(path.as_os_str().as_bytes()).expect("Pfad");
    let mut raw: libc::statvfs = unsafe { std::mem::zeroed() };
    assert_eq!(
        unsafe { libc::statvfs(target.as_ptr(), &mut raw) },
        0,
        "statvfs auf {}",
        path.display()
    );
    raw
}

#[test]
#[ignore = "braucht Linux, /dev/fuse und das Recht einzuhaengen"]
fn an_empty_pool_is_an_empty_directory_and_not_an_error() {
    if prerequisites().is_none() {
        return;
    }
    let workspace = Workspace::new("leer");
    let root = workspace.branch_root(0);
    std::fs::create_dir_all(&root).expect("Branch anlegen");
    let pool = Mounted::start(&workspace, vec![BranchRoot::new(BranchId(0), root)]);

    assert!(names_in(&pool.path("")).is_empty());
    assert!(
        std::fs::metadata(pool.path("gibt-es-nicht")).is_err(),
        "ein fehlender Name muss ENOENT geben"
    );
}

#[test]
#[ignore = "braucht Linux, /dev/fuse und das Recht einzuhaengen"]
fn a_branch_that_does_not_exist_does_not_take_the_pool_down() {
    // Eine Platte, die gerade nicht da ist. Der Pool muss den Rest weiter
    // zeigen — sonst nimmt ein ausgehaengter Member das ganze NAS mit.
    if prerequisites().is_none() {
        return;
    }
    let workspace = Workspace::new("fehlt");
    let root = workspace.branch_root(0);
    write(&root.join("da.txt"), "immer noch da");

    let pool = Mounted::start(
        &workspace,
        vec![
            BranchRoot::new(BranchId(0), root),
            BranchRoot::new(BranchId(1), workspace.branch_root(9)),
        ],
    );

    assert_eq!(names_in(&pool.path("")).len(), 1);
    assert_eq!(
        std::fs::read_to_string(pool.path("da.txt")).expect("lesen"),
        "immer noch da"
    );
}

// --- Der Schreibpfad ------------------------------------------------------
//
// Hier ist die Frage nicht mehr, ob der Pool zeigt, was auf den Platten liegt,
// sondern ob er es an die richtige Stelle legt. Die Platzierungsregeln sind in
// `place.rs` fuer sich geprueft; diese Tests fuehren sie durch den Kernel
// hindurch aus und sehen nach, was danach wirklich auf welcher Platte steht.

/// Auf welchen Branch-Wurzeln liegt dieser Pfad wirklich?
fn on_disk(workspace: &Workspace, count: u16, relative: &str) -> Vec<u16> {
    (0..count)
        .filter(|index| {
            std::fs::symlink_metadata(workspace.branch_root(*index).join(relative)).is_ok()
        })
        .collect()
}

/// Zwei tmpfs, das zweite deutlich leerer.
///
/// Damit hat `MostFree` etwas zu entscheiden — auf zwei gleich vollen Platten
/// bewiese der Test nichts.
fn uneven(workspace: &Workspace) -> Option<(Tmpfs, Vec<BranchRoot>)> {
    let zero = workspace.branch_root(0);
    let one = workspace.branch_root(1);
    let mounts = Tmpfs::sized(&[(&zero, 8), (&one, 64)])?;
    Some((
        mounts,
        vec![
            BranchRoot::new(BranchId(0), zero),
            BranchRoot::new(BranchId(1), one),
        ],
    ))
}

#[test]
#[ignore = "braucht Linux, /dev/fuse und das Recht einzuhaengen"]
fn a_new_file_is_written_and_read_back() {
    if prerequisites().is_none() {
        return;
    }
    let workspace = Workspace::new("schreiben");
    let root = workspace.branch_root(0);
    std::fs::create_dir_all(&root).expect("Branch anlegen");
    let pool = Mounted::start(&workspace, vec![BranchRoot::new(BranchId(0), root)]);

    // Gross genug, dass der Kernel den Inhalt in mehreren Stuecken schickt.
    let content: Vec<u8> = (0..(2 << 20)).map(|index| (index % 251) as u8).collect();
    std::fs::write(pool.path("neu.bin"), &content).expect("schreiben");

    assert!(
        std::fs::read(pool.path("neu.bin")).expect("lesen") == content,
        "der Inhalt kam veraendert zurueck"
    );
    assert_eq!(
        std::fs::read(workspace.branch_root(0).join("neu.bin")).expect("direkt lesen"),
        content,
        "die Datei liegt nicht auf der Platte, sondern nur im Cache"
    );
}

#[test]
#[ignore = "braucht Linux, /dev/fuse und das Recht einzuhaengen"]
fn most_free_decides_where_a_new_file_goes() {
    if prerequisites().is_none() {
        return;
    }
    let workspace = Workspace::new("platzierung");
    let Some((_mounts, branches)) = uneven(&workspace) else {
        return;
    };
    let pool = Mounted::with_policy(&workspace, branches, SharePolicy::default());

    std::fs::write(pool.path("datei.bin"), b"irgendwas").expect("schreiben");
    assert_eq!(
        on_disk(&workspace, 2, "datei.bin"),
        vec![1],
        "die neue Datei gehoert auf die leerere Platte, und auf genau eine"
    );
}

#[test]
#[ignore = "braucht Linux, /dev/fuse und das Recht einzuhaengen"]
fn fill_up_decides_differently_on_the_same_disks() {
    // Die Gegenprobe zum Test darueber: Ohne sie koennte die Platzierung eine
    // feste Platte waehlen und beide Tests zufaellig bestehen.
    if prerequisites().is_none() {
        return;
    }
    let workspace = Workspace::new("fuellen");
    let Some((_mounts, branches)) = uneven(&workspace) else {
        return;
    };
    let policy = SharePolicy {
        allocation: Allocation::FillUp,
        ..SharePolicy::default()
    };
    let pool = Mounted::with_policy(&workspace, branches, policy);

    std::fs::write(pool.path("datei.bin"), b"irgendwas").expect("schreiben");
    assert_eq!(
        on_disk(&workspace, 2, "datei.bin"),
        vec![0],
        "FillUp nimmt die erste Platte mit Platz, nicht die leerste"
    );
}

#[test]
#[ignore = "braucht Linux, /dev/fuse und das Recht einzuhaengen"]
fn the_split_rule_keeps_a_directory_together_on_disk() {
    // Die Regel aus `policy.rs`, diesmal durch den Kernel hindurch.
    if prerequisites().is_none() {
        return;
    }
    let workspace = Workspace::new("split");
    let Some((_mounts, branches)) = uneven(&workspace) else {
        return;
    };
    let policy = SharePolicy {
        split: SplitDepth::UpTo(1),
        ..SharePolicy::default()
    };
    let pool = Mounted::with_policy(&workspace, branches, policy);

    std::fs::create_dir_all(pool.path("Serien/S01")).expect("Verzeichnis anlegen");
    for episode in 1..=5 {
        std::fs::write(pool.path(&format!("Serien/S01/E{episode:02}.mkv")), b"x")
            .expect("Folge schreiben");
    }

    let branches_used: BTreeSet<u16> = (1..=5)
        .flat_map(|episode| on_disk(&workspace, 2, &format!("Serien/S01/E{episode:02}.mkv")))
        .collect();
    assert_eq!(
        branches_used.len(),
        1,
        "die Staffel ist ueber mehrere Platten verteilt: {branches_used:?}"
    );
}

#[test]
#[ignore = "braucht Linux, /dev/fuse und das Recht einzuhaengen"]
fn a_directory_that_grows_onto_a_second_branch_keeps_its_permissions() {
    // Ein Verzeichnis liegt auf Platte 0, die naechste Datei darin gehoert
    // nach `MostFree` auf Platte 1. Dort muss es mit denselben Rechten
    // entstehen — sonst haengt es davon ab, welche Platte gerade bedient, und
    // das aendert sich, sobald eine geloescht wird.
    if prerequisites().is_none() {
        return;
    }
    let workspace = Workspace::new("rechte");
    let Some((_mounts, branches)) = uneven(&workspace) else {
        return;
    };

    // Von Hand auf Platte 0 anlegen, mit auffaelligen Rechten.
    let ordner = workspace.branch_root(0).join("Ordner");
    std::fs::create_dir(&ordner).expect("Verzeichnis anlegen");
    std::fs::set_permissions(&ordner, std::fs::Permissions::from_mode(0o2710))
        .expect("Rechte setzen");

    let pool = Mounted::with_policy(&workspace, branches, SharePolicy::default());
    std::fs::write(pool.path("Ordner/datei.bin"), b"x").expect("schreiben");

    assert_eq!(
        on_disk(&workspace, 2, "Ordner/datei.bin"),
        vec![1],
        "die Datei sollte auf der leereren Platte landen"
    );
    let copied = std::fs::symlink_metadata(workspace.branch_root(1).join("Ordner"))
        .expect("das Verzeichnis muss auf Platte 1 entstanden sein");
    assert_eq!(
        copied.mode() & 0o7777,
        0o2710,
        "die Rechte wurden nicht uebernommen"
    );
}

#[test]
#[ignore = "braucht Linux, /dev/fuse und das Recht einzuhaengen"]
fn deleting_a_name_removes_it_from_every_branch() {
    // Der Fall, an dem sich Ferrite von Unraid unterscheidet: Wer nur die
    // bedienende Kopie loescht, sieht die zweite danach wieder auftauchen.
    if prerequisites().is_none() {
        return;
    }
    let workspace = Workspace::new("loeschen");
    let zero = workspace.branch_root(0);
    let one = workspace.branch_root(1);
    write(&zero.join("doppelt.txt"), "von null");
    write(&one.join("doppelt.txt"), "von eins");

    let pool = Mounted::start(
        &workspace,
        vec![
            BranchRoot::new(BranchId(0), zero),
            BranchRoot::new(BranchId(1), one),
        ],
    );

    std::fs::remove_file(pool.path("doppelt.txt")).expect("loeschen");
    assert!(
        on_disk(&workspace, 2, "doppelt.txt").is_empty(),
        "eine Kopie ist stehengeblieben und taucht beim naechsten Blick wieder auf"
    );
    assert!(names_in(&pool.path("")).is_empty());
}

#[test]
#[ignore = "braucht Linux, /dev/fuse und das Recht einzuhaengen"]
fn a_directory_is_only_removed_when_it_is_empty_on_every_branch() {
    if prerequisites().is_none() {
        return;
    }
    let workspace = Workspace::new("rmdir");
    let zero = workspace.branch_root(0);
    let one = workspace.branch_root(1);
    std::fs::create_dir_all(zero.join("Ordner")).expect("anlegen");
    write(&one.join("Ordner/inhalt.txt"), "noch da");

    let pool = Mounted::start(
        &workspace,
        vec![
            BranchRoot::new(BranchId(0), zero),
            BranchRoot::new(BranchId(1), one),
        ],
    );

    assert!(
        std::fs::remove_dir(pool.path("Ordner")).is_err(),
        "das Verzeichnis ist auf einer Platte nicht leer"
    );
    assert_eq!(
        on_disk(&workspace, 2, "Ordner"),
        vec![0, 1],
        "die leere Kopie darf dabei nicht verschwunden sein"
    );

    std::fs::remove_file(pool.path("Ordner/inhalt.txt")).expect("loeschen");
    std::fs::remove_dir(pool.path("Ordner")).expect("jetzt ist es leer");
    assert!(on_disk(&workspace, 2, "Ordner").is_empty());
}

#[test]
#[ignore = "braucht Linux, /dev/fuse und das Recht einzuhaengen"]
fn a_rename_takes_the_file_along_and_clears_what_was_in_the_way() {
    // Die Datei liegt auf Platte 1, am Ziel liegt schon etwas auf Platte 0.
    // Bliebe das stehen, verdeckte es das Ergebnis — Platte 0 bedient zuerst.
    if prerequisites().is_none() {
        return;
    }
    let workspace = Workspace::new("umbenennen");
    let zero = workspace.branch_root(0);
    let one = workspace.branch_root(1);
    write(&zero.join("ziel.txt"), "das Alte");
    write(&one.join("quelle.txt"), "das Neue");

    let pool = Mounted::start(
        &workspace,
        vec![
            BranchRoot::new(BranchId(0), zero),
            BranchRoot::new(BranchId(1), one),
        ],
    );

    std::fs::rename(pool.path("quelle.txt"), pool.path("ziel.txt")).expect("umbenennen");

    assert_eq!(
        std::fs::read_to_string(pool.path("ziel.txt")).expect("lesen"),
        "das Neue"
    );
    assert_eq!(
        on_disk(&workspace, 2, "ziel.txt"),
        vec![1],
        "das Alte muss weg sein, sonst verdeckt es das Neue"
    );
    assert!(on_disk(&workspace, 2, "quelle.txt").is_empty());
}

#[test]
#[ignore = "braucht Linux, /dev/fuse und das Recht einzuhaengen"]
fn a_rename_into_a_directory_that_lies_elsewhere_works() {
    if prerequisites().is_none() {
        return;
    }
    let workspace = Workspace::new("umzug");
    let zero = workspace.branch_root(0);
    let one = workspace.branch_root(1);
    std::fs::create_dir_all(zero.join("Ordner")).expect("anlegen");
    write(&one.join("datei.txt"), "unterwegs");

    let pool = Mounted::start(
        &workspace,
        vec![
            BranchRoot::new(BranchId(0), zero),
            BranchRoot::new(BranchId(1), one),
        ],
    );

    // `Ordner` gibt es nur auf Platte 0, die Datei liegt auf Platte 1. Der
    // Umzug muss `Ordner` dort erst anlegen.
    std::fs::rename(pool.path("datei.txt"), pool.path("Ordner/datei.txt")).expect("umbenennen");

    assert_eq!(
        std::fs::read_to_string(pool.path("Ordner/datei.txt")).expect("lesen"),
        "unterwegs"
    );
    assert_eq!(on_disk(&workspace, 2, "Ordner/datei.txt"), vec![1]);
}

#[test]
#[ignore = "braucht Linux, /dev/fuse und das Recht einzuhaengen"]
fn chmod_reaches_every_branch_that_carries_the_directory() {
    if prerequisites().is_none() {
        return;
    }
    let workspace = Workspace::new("chmod");
    let zero = workspace.branch_root(0);
    let one = workspace.branch_root(1);
    std::fs::create_dir_all(zero.join("Ordner")).expect("anlegen");
    std::fs::create_dir_all(one.join("Ordner")).expect("anlegen");

    let pool = Mounted::start(
        &workspace,
        vec![
            BranchRoot::new(BranchId(0), zero),
            BranchRoot::new(BranchId(1), one),
        ],
    );

    std::fs::set_permissions(pool.path("Ordner"), std::fs::Permissions::from_mode(0o750))
        .expect("Rechte setzen");

    for index in 0..2u16 {
        let mode = std::fs::symlink_metadata(workspace.branch_root(index).join("Ordner"))
            .expect("stat")
            .mode()
            & 0o7777;
        assert_eq!(
            mode, 0o750,
            "auf Platte {index} gelten andere Rechte — welche zaehlen, haengt dann vom Zufall ab"
        );
    }
}

#[test]
#[ignore = "braucht Linux, /dev/fuse und das Recht einzuhaengen"]
fn truncating_a_file_shortens_it_on_disk() {
    if prerequisites().is_none() {
        return;
    }
    let workspace = Workspace::new("kuerzen");
    let root = workspace.branch_root(0);
    write(&root.join("lang.txt"), "viel zu lang fuer diesen Zweck");
    let pool = Mounted::start(&workspace, vec![BranchRoot::new(BranchId(0), root)]);

    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(pool.path("lang.txt"))
        .expect("oeffnen");
    file.set_len(4).expect("kuerzen");
    drop(file);

    assert_eq!(
        std::fs::read_to_string(workspace.branch_root(0).join("lang.txt")).expect("lesen"),
        "viel"
    );
}

#[test]
#[ignore = "braucht Linux, /dev/fuse und das Recht einzuhaengen"]
fn a_full_pool_refuses_before_it_starts_writing() {
    // Die Reserve aus der Policy, an einem echten Dateisystem. Ein ENOSPC
    // mitten im Schreiben waere teurer als eine Absage vorher.
    if prerequisites().is_none() {
        return;
    }
    let workspace = Workspace::new("voll");
    let root = workspace.branch_root(0);
    let Some(_mounts) = Tmpfs::sized(&[(&root, 8)]) else {
        return;
    };
    let policy = SharePolicy {
        min_free: 4 << 20,
        ..SharePolicy::default()
    };
    let pool = Mounted::with_policy(&workspace, vec![BranchRoot::new(BranchId(0), root)], policy);

    // Erst die Platte fuellen, bis die Reserve angeknabbert ist. Genau so
    // entsteht die Lage im Betrieb — ein frisches Dateisystem hat immer
    // genug frei.
    let block = vec![0u8; 6 << 20];
    std::fs::write(pool.path("gross.bin"), &block).expect("das passt noch");

    // Der naechste Versuch faellt schon bei der Platzierung durch. Die
    // Reserve wird beim Anlegen geprueft und nicht bei jedem Write: Ein
    // einzelner grosser Write kann sie anknabbern, wie hier gerade geschehen.
    // Was sie verhindert, ist das Vollaufen ueber viele Dateien hinweg — und
    // das ist der Fall, der ein btrfs unaufraeumbar macht.
    let error = std::fs::write(pool.path("noch-eine.bin"), b"x").expect_err("darf nicht gehen");
    assert_eq!(
        error.raw_os_error(),
        Some(libc::ENOSPC),
        "erwartet war ENOSPC, kam: {error}"
    );
    assert!(on_disk(&workspace, 1, "noch-eine.bin").is_empty());
}

#[test]
#[ignore = "braucht Linux, /dev/fuse und das Recht einzuhaengen"]
fn a_symlink_can_be_created_through_the_pool() {
    if prerequisites().is_none() {
        return;
    }
    let workspace = Workspace::new("neuer-symlink");
    let root = workspace.branch_root(0);
    write(&root.join("ziel.txt"), "hier bin ich");
    let pool = Mounted::start(&workspace, vec![BranchRoot::new(BranchId(0), root)]);

    std::os::unix::fs::symlink("ziel.txt", pool.path("verweis")).expect("Symlink anlegen");
    assert_eq!(
        std::fs::read_to_string(pool.path("verweis")).expect("ueber den Verweis lesen"),
        "hier bin ich"
    );
    assert!(
        std::fs::symlink_metadata(workspace.branch_root(0).join("verweis"))
            .expect("stat")
            .file_type()
            .is_symlink()
    );
}

#[test]
#[ignore = "braucht Linux, /dev/fuse und das Recht einzuhaengen"]
fn a_new_file_belongs_to_whoever_created_it() {
    // Der Server laeuft als Root. Ohne das Nachziehen des Eigentuemers
    // gehoerte jede Datei Root — und der Nutzer, der sie angelegt hat, kaeme
    // an seine eigene Datei nicht mehr heran. Auf einem NAS, hinter dem Samba
    // und NFS mit eigenen Nutzern stehen, ist das der Regelfall.
    if prerequisites().is_none() {
        return;
    }
    if !have("setpriv") {
        eprintln!("uebersprungen: setpriv fehlt");
        return;
    }
    let workspace = Workspace::new("eigentuemer");
    let root = workspace.branch_root(0);
    std::fs::create_dir_all(&root).expect("Branch anlegen");
    // Damit ein fremder Nutzer ueberhaupt hineinschreiben darf.
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o777)).expect("Rechte");

    let pool = Mounted::start(&workspace, vec![BranchRoot::new(BranchId(0), root)]);

    const NOBODY: u32 = 65534;
    let status = std::process::Command::new("setpriv")
        .args([
            &format!("--reuid={NOBODY}"),
            &format!("--regid={NOBODY}"),
            "--clear-groups",
            "touch",
        ])
        .arg(pool.path("fremd.txt"))
        .status()
        .expect("setpriv starten");
    assert!(status.success(), "setpriv ist gescheitert");

    let owner = std::fs::symlink_metadata(workspace.branch_root(0).join("fremd.txt"))
        .expect("stat")
        .uid();
    assert_eq!(
        owner, NOBODY,
        "die Datei gehoert Root statt dem Nutzer, der sie angelegt hat"
    );
}

fn have(program: &str) -> bool {
    std::process::Command::new("sh")
        .args(["-c", &format!("command -v {program}")])
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

// --- Passthrough ----------------------------------------------------------

use std::sync::atomic::Ordering;

/// Die Version des laufenden Kernels als `(major, minor)`.
fn kernel_version() -> (u32, u32) {
    let release = std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default();
    let mut parts = release.trim().split(['.', '-']);
    let major = parts.next().unwrap_or("0").parse().unwrap_or(0);
    let minor = parts.next().unwrap_or("0").parse().unwrap_or(0);
    (major, minor)
}

/// `None` mit Begruendung, wenn der Kernel zu alt fuer den Passthrough ist.
///
/// # Warum die Kernelversion und nicht der eigene Zaehler
///
/// Ob der Passthrough ausgehandelt wurde, entscheidet genau der Code, den
/// diese Tests pruefen sollen. Wer daran das Ueberspringen festmacht, baut
/// einen Test, der bei jedem Fehler still durchwinkt statt rot zu werden —
/// gemessen: Mit `FUSE_PASSTHROUGH` auf dem falschen Bit blieben alle 27
/// Tests gruen. Deshalb entscheidet die Umgebung ueber das Ueberspringen und
/// der Zaehler ueber das Bestehen.
fn passthrough_or_skip(pool: &Mounted) -> Option<()> {
    let (major, minor) = kernel_version();
    if (major, minor) < (6, 9) {
        eprintln!("uebersprungen: Kernel {major}.{minor} kennt keinen FUSE-Passthrough (ab 6.9)");
        return None;
    }
    // Erst nach der ersten Anfrage steht fest, was `INIT` ergeben hat.
    let _ = std::fs::read_dir(pool.path(""));
    assert!(
        pool.counters.passthrough_available.load(Ordering::Relaxed),
        "Kernel {major}.{minor} kann den Passthrough — er wurde trotzdem nicht ausgehandelt"
    );
    Some(())
}

#[test]
#[ignore = "braucht Linux, /dev/fuse und das Recht einzuhaengen"]
fn the_kernel_serves_a_read_without_asking_us() {
    let Some(()) = prerequisites() else { return };
    let workspace = Workspace::new("passthrough-read");
    let branches = two_branches(&workspace);
    write(&workspace.branch_root(0).join("gross.bin"), "");
    let content: Vec<u8> = (0..(3 << 20)).map(|i| (i % 251) as u8).collect();
    std::fs::write(workspace.branch_root(0).join("gross.bin"), &content).expect("schreiben");

    let pool = Mounted::start(&workspace, branches);
    let Some(()) = passthrough_or_skip(&pool) else {
        return;
    };

    let read = std::fs::read(pool.path("gross.bin")).expect("lesen");

    assert_eq!(
        read, content,
        "der Inhalt muss stimmen, egal wer ihn liefert"
    );
    // Der eigentliche Nachweis: Der Inhalt stimmt, obwohl dieser Prozess kein
    // einziges `READ` gesehen hat. Also hat der Kernel selbst gelesen.
    assert_eq!(
        pool.counters.reads.load(Ordering::Relaxed),
        0,
        "beim Passthrough darf kein READ hier ankommen"
    );
    assert!(
        pool.counters.passthrough_opens.load(Ordering::Relaxed) >= 1,
        "mindestens ein OPEN muss eine backing_id getragen haben"
    );
}

#[test]
#[ignore = "braucht Linux, /dev/fuse und das Recht einzuhaengen"]
fn the_kernel_takes_a_write_without_asking_us() {
    let Some(()) = prerequisites() else { return };
    let workspace = Workspace::new("passthrough-write");
    let branches = two_branches(&workspace);

    let pool = Mounted::start(&workspace, branches);
    let Some(()) = passthrough_or_skip(&pool) else {
        return;
    };

    let content: Vec<u8> = (0..(2 << 20)).map(|i| (i % 253) as u8).collect();
    std::fs::write(pool.path("neu.bin"), &content).expect("schreiben");

    assert_eq!(
        pool.counters.writes.load(Ordering::Relaxed),
        0,
        "beim Passthrough darf kein WRITE hier ankommen"
    );
    // Und trotzdem liegt es auf einer Platte, nicht im Nirgendwo.
    let on_disk: Vec<PathBuf> = (0..2)
        .map(|slot| workspace.branch_root(slot).join("neu.bin"))
        .filter(|path| path.exists())
        .collect();
    assert_eq!(on_disk.len(), 1, "genau ein Branch traegt die Datei");
    assert_eq!(
        std::fs::read(&on_disk[0]).expect("von der Platte lesen"),
        content
    );
}

#[test]
#[ignore = "braucht Linux, /dev/fuse und das Recht einzuhaengen"]
fn without_passthrough_this_process_serves_it_itself() {
    let Some(()) = prerequisites() else { return };
    let workspace = Workspace::new("kein-passthrough");
    let branches = two_branches(&workspace);
    let content: Vec<u8> = (0..(3 << 20)).map(|i| (i % 251) as u8).collect();
    std::fs::create_dir_all(workspace.branch_root(0)).expect("Branch anlegen");
    std::fs::write(workspace.branch_root(0).join("gross.bin"), &content).expect("schreiben");

    let pool = Mounted::plain(&workspace, branches);
    let read = std::fs::read(pool.path("gross.bin")).expect("lesen");
    std::fs::write(pool.path("neu.bin"), &content).expect("schreiben");

    assert_eq!(read, content);
    assert_eq!(
        std::fs::read(workspace.branch_root(0).join("neu.bin"))
            .or_else(|_| std::fs::read(workspace.branch_root(1).join("neu.bin")))
            .expect("von der Platte lesen"),
        content
    );

    // Die Gegenprobe zu den beiden Tests darueber: Hier *muessen* Anfragen
    // ankommen. Kaemen auch ohne Passthrough keine, zaehlten die Zaehler
    // nichts, und die Nullen dort waeren wertlos.
    assert!(
        pool.counters.reads.load(Ordering::Relaxed) > 0,
        "ohne Passthrough muss dieser Prozess die READs sehen"
    );
    assert!(
        pool.counters.writes.load(Ordering::Relaxed) > 0,
        "ohne Passthrough muss dieser Prozess die WRITEs sehen"
    );
    assert_eq!(
        pool.counters.passthrough_opens.load(Ordering::Relaxed),
        0,
        "abgeschaltet heisst abgeschaltet"
    );
    assert!(pool.counters.plain_opens.load(Ordering::Relaxed) >= 2);
}

#[test]
#[ignore = "braucht Linux, /dev/fuse und das Recht einzuhaengen"]
fn every_handed_over_file_is_taken_back() {
    let Some(()) = prerequisites() else { return };
    let workspace = Workspace::new("passthrough-leck");
    let branches = two_branches(&workspace);
    for index in 0..8 {
        write(
            &workspace.branch_root(0).join(format!("datei-{index}")),
            "Inhalt",
        );
    }

    let pool = Mounted::start(&workspace, branches);
    let Some(()) = passthrough_or_skip(&pool) else {
        return;
    };

    for index in 0..8 {
        assert_eq!(
            std::fs::read_to_string(pool.path(&format!("datei-{index}"))).expect("lesen"),
            "Inhalt"
        );
    }

    let opens = pool.counters.backing_opens.load(Ordering::Relaxed);
    assert_eq!(opens, 8, "acht Dateien, acht hinterlegte Deskriptoren");
    assert_eq!(
        pool.counters.passthrough_opens.load(Ordering::Relaxed),
        8,
        "und acht Handles, die je eine davon halten"
    );

    // `RELEASE` kommt erst, wenn der Kernel die letzte Referenz fallen laesst,
    // und das ist nicht der Rueckkehrpunkt von `read_to_string`. Deshalb
    // warten statt sofort messen — mit Frist, damit ein Ausbleiben rot wird
    // und nicht haengt.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while pool.counters.backing_closes.load(Ordering::Relaxed) < opens
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    // Der Kernel zaehlt hier mit: `backing_closes` steigt nur, wenn das
    // `ioctl` die `backing_id` wirklich kannte. Eine erfundene oder doppelt
    // geschlossene zaehlt nicht mit.
    assert_eq!(
        pool.counters.backing_closes.load(Ordering::Relaxed),
        opens,
        "jede hinterlegte Datei muss wieder freigegeben werden"
    );
}

// --- Erweiterte Attribute -------------------------------------------------
//
// Die Regel, die hier geprueft wird: gelesen vom bedienenden Branch,
// geschrieben auf jeden, der den Namen traegt. Fuer eine Datei ist das genau
// einer; ein Verzeichnis liegt oft auf mehreren, und truegen die
// verschiedene Attribute, haenge es vom bedienenden Branch ab, welche gelten.

fn c_str(path: &Path) -> CString {
    CString::new(path.as_os_str().as_bytes()).expect("Pfad ohne Nullbyte")
}

fn set_xattr(path: &Path, name: &str, value: &[u8]) -> std::io::Result<()> {
    let path = c_str(path);
    let name = CString::new(name).expect("Name ohne Nullbyte");
    let result = unsafe {
        libc::lsetxattr(
            path.as_ptr(),
            name.as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn get_xattr(path: &Path, name: &str) -> std::io::Result<Vec<u8>> {
    let path = c_str(path);
    let name = CString::new(name).expect("Name ohne Nullbyte");
    let mut value = vec![0u8; 4096];
    let read = unsafe {
        libc::lgetxattr(
            path.as_ptr(),
            name.as_ptr(),
            value.as_mut_ptr().cast(),
            value.len(),
        )
    };
    if read < 0 {
        return Err(std::io::Error::last_os_error());
    }
    value.truncate(read as usize);
    Ok(value)
}

fn list_xattr(path: &Path) -> std::io::Result<BTreeSet<String>> {
    let path = c_str(path);
    let mut names = vec![0u8; 8192];
    let read = unsafe { libc::llistxattr(path.as_ptr(), names.as_mut_ptr().cast(), names.len()) };
    if read < 0 {
        return Err(std::io::Error::last_os_error());
    }
    names.truncate(read as usize);
    Ok(names
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
        .map(|name| String::from_utf8_lossy(name).into_owned())
        .collect())
}

fn remove_xattr(path: &Path, name: &str) -> std::io::Result<()> {
    let path = c_str(path);
    let name = CString::new(name).expect("Name ohne Nullbyte");
    let result = unsafe { libc::lremovexattr(path.as_ptr(), name.as_ptr()) };
    if result != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// `None` mit Begruendung, wenn das Dateisystem unter den Branches keine
/// `user.*`-Attribute kann.
///
/// tmpfs konnte sie erst ab Kernel 6.6, und ein Dateisystem ohne `user_xattr`
/// gibt es auch heute noch. Dann ueberspringen — ein Test, der gruen ist,
/// ohne dass je ein Attribut geschrieben wurde, sagt nichts.
fn xattrs_or_skip(workspace: &Workspace) -> Option<()> {
    std::fs::create_dir_all(workspace.branch_root(0)).expect("Branch anlegen");
    let probe = workspace.branch_root(0).join(".probe");
    std::fs::write(&probe, b"x").expect("Probe anlegen");
    let supported = set_xattr(&probe, "user.ferrite.probe", b"1").is_ok();
    let _ = std::fs::remove_file(&probe);
    if !supported {
        eprintln!("uebersprungen: das Dateisystem unter den Branches kann keine user.*-Attribute");
        return None;
    }
    Some(())
}

#[test]
#[ignore = "braucht Linux, /dev/fuse und das Recht einzuhaengen"]
fn an_attribute_on_a_file_comes_back_byte_for_byte() {
    let Some(()) = prerequisites() else { return };
    let workspace = Workspace::new("xattr-datei");
    let Some(()) = xattrs_or_skip(&workspace) else {
        return;
    };
    let branches = two_branches(&workspace);
    let pool = Mounted::start(&workspace, branches);

    let datei = pool.path("nur-null.txt");
    // Ein Wert mit Nullbyte und mit hohen Bytes: Attributwerte sind Bytes und
    // keine Zeichenkette. Wer sie durch einen String reicht, verliert genau
    // das hier.
    let wert: &[u8] = &[0x00, 0xff, b'a', 0x00, 0x80];
    set_xattr(&datei, "user.ferrite.test", wert).expect("Attribut setzen");

    assert_eq!(
        get_xattr(&datei, "user.ferrite.test").expect("Attribut lesen"),
        wert
    );
    assert!(list_xattr(&datei)
        .expect("Attribute auflisten")
        .contains("user.ferrite.test"));

    // Und wirklich auf der Platte, nicht nur im Kopf des Kernels.
    assert_eq!(
        get_xattr(
            &workspace.branch_root(0).join("nur-null.txt"),
            "user.ferrite.test"
        )
        .expect("auf der Platte lesen"),
        wert
    );

    remove_xattr(&datei, "user.ferrite.test").expect("Attribut entfernen");
    assert_eq!(
        get_xattr(&datei, "user.ferrite.test")
            .expect_err("das Attribut ist weg")
            .raw_os_error(),
        Some(libc::ENODATA)
    );
}

#[test]
#[ignore = "braucht Linux, /dev/fuse und das Recht einzuhaengen"]
fn an_attribute_on_a_directory_reaches_every_branch() {
    let Some(()) = prerequisites() else { return };
    let workspace = Workspace::new("xattr-ordner");
    let Some(()) = xattrs_or_skip(&workspace) else {
        return;
    };
    let branches = two_branches(&workspace);
    // `Filme` liegt bisher nur auf Branch 0 — fuer diesen Test soll es auf
    // beiden liegen.
    write(
        &workspace.branch_root(1).join("Filme/drei.mkv"),
        "auch hier",
    );
    let pool = Mounted::start(&workspace, branches);

    set_xattr(&pool.path("Filme"), "user.ferrite.share", b"medien").expect("Attribut setzen");

    for slot in 0..2 {
        assert_eq!(
            get_xattr(
                &workspace.branch_root(slot).join("Filme"),
                "user.ferrite.share"
            )
            .unwrap_or_else(|error| panic!("Branch {slot} traegt das Attribut nicht: {error}")),
            b"medien",
            "Branch {slot}"
        );
    }

    // Und wieder weg, ebenfalls ueberall. Bliebe es auf einem Branch stehen,
    // taeuchte es wieder auf, sobald der die Auskunft bedient.
    remove_xattr(&pool.path("Filme"), "user.ferrite.share").expect("Attribut entfernen");
    for slot in 0..2 {
        assert_eq!(
            get_xattr(
                &workspace.branch_root(slot).join("Filme"),
                "user.ferrite.share"
            )
            .expect_err("das Attribut muss weg sein")
            .raw_os_error(),
            Some(libc::ENODATA),
            "Branch {slot}"
        );
    }
}

#[test]
#[ignore = "braucht Linux, /dev/fuse und das Recht einzuhaengen"]
fn a_directory_that_grows_onto_a_second_branch_takes_its_attributes_along() {
    // Derselbe Fall wie bei den Rechten, eine Ebene tiefer: Eine Default-ACL
    // liegt als erweitertes Attribut auf dem Verzeichnis. Entsteht die Kopie
    // auf der zweiten Platte ohne sie, bekommen Dateien dort andere Rechte —
    // und welche, haengt davon ab, wohin die Platzierung sie gerade legt.
    let Some(()) = prerequisites() else { return };
    let workspace = Workspace::new("xattr-wachstum");
    let Some((_mounts, branches)) = uneven(&workspace) else {
        return;
    };

    let ordner = workspace.branch_root(0).join("Ordner");
    std::fs::create_dir(&ordner).expect("Verzeichnis anlegen");
    if set_xattr(&ordner, "user.ferrite.share", b"medien").is_err() {
        eprintln!("uebersprungen: die tmpfs-Branches nehmen keine user.*-Attribute");
        return;
    }

    let pool = Mounted::with_policy(&workspace, branches, SharePolicy::default());
    std::fs::write(pool.path("Ordner/datei.bin"), b"x").expect("schreiben");

    assert_eq!(
        on_disk(&workspace, 2, "Ordner/datei.bin"),
        vec![1],
        "die Datei sollte auf der leereren Platte landen"
    );
    assert_eq!(
        get_xattr(
            &workspace.branch_root(1).join("Ordner"),
            "user.ferrite.share"
        )
        .expect("das Attribut kam nicht mit auf die zweite Platte"),
        b"medien"
    );
    assert_eq!(
        pool.counters.xattrs_not_mirrored.load(Ordering::Relaxed),
        0,
        "kein Attribut darf unterwegs verloren gehen"
    );
}

#[test]
#[ignore = "braucht Linux, /dev/fuse und das Recht einzuhaengen"]
fn a_buffer_that_is_too_small_is_erange_and_not_a_half_value() {
    // Eine halbe ACL ist eine andere ACL. Der zweistufige Ablauf — erst nach
    // der Groesse fragen, dann holen — muss deshalb `ERANGE` liefern und
    // nicht abschneiden.
    let Some(()) = prerequisites() else { return };
    let workspace = Workspace::new("xattr-erange");
    let Some(()) = xattrs_or_skip(&workspace) else {
        return;
    };
    let branches = two_branches(&workspace);
    let pool = Mounted::start(&workspace, branches);

    let datei = pool.path("nur-null.txt");
    set_xattr(&datei, "user.ferrite.lang", &[b'x'; 200]).expect("Attribut setzen");

    let path = c_str(&datei);
    let name = CString::new("user.ferrite.lang").expect("Name ohne Nullbyte");

    // Erst die Frage nach der Groesse.
    let needed = unsafe { libc::lgetxattr(path.as_ptr(), name.as_ptr(), std::ptr::null_mut(), 0) };
    assert_eq!(needed, 200, "die gemeldete Groesse stimmt nicht");

    // Dann mit einem Puffer, der eins zu klein ist.
    let mut small = vec![0u8; 199];
    let read = unsafe {
        libc::lgetxattr(
            path.as_ptr(),
            name.as_ptr(),
            small.as_mut_ptr().cast(),
            small.len(),
        )
    };
    assert_eq!(read, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ERANGE)
    );
}

// --- POSIX-ACLs -----------------------------------------------------------
//
// ACLs sind der Grund, warum ein NAS ueberhaupt erweiterte Attribute braucht:
// Samba legt seine Rechte dort ab. Sie laufen durch dieselben Handler wie
// jedes andere Attribut — was hier zusaetzlich geprueft wird, ist die zweite
// Haelfte von `FUSE_POSIX_ACL`: Ab diesem Bit wendet der Kernel die `umask`
// beim Anlegen nicht mehr an, und das muss dieser Server uebernehmen.

/// Fuehrt `setfacl`/`getfacl` aus und gibt die Standardausgabe zurueck.
fn acl_tool(program: &str, args: &[&str], path: &Path) -> std::io::Result<String> {
    let output = std::process::Command::new(program)
        .args(args)
        .arg(path)
        .output()?;
    if !output.status.success() {
        return Err(std::io::Error::other(format!(
            "{program} {args:?} {path:?}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// `None` mit Begruendung, wenn die ACL-Werkzeuge fehlen oder das
/// Dateisystem unter den Branches keine ACLs kann.
fn acls_or_skip(workspace: &Workspace) -> Option<()> {
    if !have("setfacl") || !have("getfacl") {
        eprintln!("uebersprungen: setfacl/getfacl fehlen — Paket `acl` nicht installiert");
        return None;
    }
    std::fs::create_dir_all(workspace.branch_root(0)).expect("Branch anlegen");
    let probe = workspace.branch_root(0).join(".probe-acl");
    std::fs::create_dir_all(&probe).expect("Probe anlegen");
    let works = acl_tool("setfacl", &["-m", "u:12345:rwx"], &probe).is_ok();
    let _ = std::fs::remove_dir_all(&probe);
    if !works {
        eprintln!("uebersprungen: das Dateisystem unter den Branches kann keine POSIX-ACLs");
        return None;
    }
    Some(())
}

/// Legt eine Datei in einem **Kindprozess** mit gesetzter `umask` an.
///
/// # Warum nicht einfach `libc::umask` im Test
///
/// Der Server laeuft in einem Thread desselben Prozesses und teilt sich die
/// `umask` mit dem Test. Setzte der Test sie, zoege das Dateisystem unter dem
/// Branch sie beim Anlegen selbst ab — und der Test bliebe gruen, auch wenn
/// dieser Server sie gar nicht anwendet. Gemessen: Mit der `umask` im
/// Testprozess blieb die Sabotage "nie abziehen" unentdeckt.
///
/// Im Kindprozess ist sie dagegen genau das, was der Kernel in der Anfrage
/// mitschickt — und sonst nichts.
fn create_with_umask(path: &Path, umask: &str) {
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("umask {umask}; : > \"$1\""))
        .arg("sh")
        .arg(path)
        .status()
        .expect("sh starten");
    assert!(status.success(), "Datei mit umask {umask} anlegen");
}

#[test]
#[ignore = "braucht Linux, /dev/fuse und das Recht einzuhaengen"]
fn an_acl_set_through_the_pool_reaches_every_branch() {
    let Some(()) = prerequisites() else { return };
    let workspace = Workspace::new("acl-alle");
    let Some(()) = acls_or_skip(&workspace) else {
        return;
    };
    let branches = two_branches(&workspace);
    // `Filme` soll auf beiden Branches liegen.
    write(
        &workspace.branch_root(1).join("Filme/drei.mkv"),
        "auch hier",
    );
    let pool = Mounted::start(&workspace, branches);
    // Erst nach der ersten Anfrage steht fest, was `INIT` ergeben hat.
    let _ = std::fs::read_dir(pool.path(""));
    assert!(
        pool.counters.posix_acl_available.load(Ordering::Relaxed),
        "POSIX-ACLs wurden nicht ausgehandelt"
    );
    acl_tool("setfacl", &["-m", "u:12345:rwx"], &pool.path("Filme")).expect("ACL setzen");

    for slot in 0..2 {
        let text = acl_tool(
            "getfacl",
            &["-c"],
            &workspace.branch_root(slot).join("Filme"),
        )
        .expect("ACL lesen");
        assert!(
            text.contains("user:12345:rwx"),
            "Branch {slot} traegt die ACL nicht:\n{text}"
        );
    }
}

#[test]
#[ignore = "braucht Linux, /dev/fuse und das Recht einzuhaengen"]
fn a_default_acl_wins_over_the_umask() {
    // Der Kern von `FUSE_POSIX_ACL`: Traegt das Elternverzeichnis eine
    // Default-ACL, vergibt sie die Rechte, und die `umask` zieht nichts mehr
    // ab. Wer sie trotzdem anwendet, macht aus einer Freigabe fuer die Gruppe
    // eine Datei, in die nur der Eigentuemer kommt — und merkt es erst, wenn
    // der naechste Nutzer nicht mehr herankommt.
    let Some(()) = prerequisites() else { return };
    let workspace = Workspace::new("acl-umask");
    let Some(()) = acls_or_skip(&workspace) else {
        return;
    };
    let branches = two_branches(&workspace);
    let pool = Mounted::start(&workspace, branches);

    std::fs::create_dir(pool.path("Freigabe")).expect("Verzeichnis anlegen");
    std::fs::set_permissions(
        pool.path("Freigabe"),
        std::fs::Permissions::from_mode(0o775),
    )
    .expect("Rechte setzen");
    acl_tool(
        "setfacl",
        &["-d", "-m", "u:12345:rwx"],
        &pool.path("Freigabe"),
    )
    .expect("Default-ACL setzen");

    create_with_umask(&pool.path("Freigabe/datei.txt"), "077");
    let text = acl_tool("getfacl", &["-c"], &pool.path("Freigabe/datei.txt")).expect("ACL lesen");
    let mode = std::fs::symlink_metadata(pool.path("Freigabe/datei.txt"))
        .expect("Attribute lesen")
        .mode();

    assert!(
        text.contains("user:12345:"),
        "die Default-ACL wurde nicht vererbt:\n{text}"
    );
    // 0o077 haette genau diese Bits weggenommen. Dass sie stehen, ist der
    // Nachweis, dass die `umask` hier nichts zu sagen hatte.
    assert_ne!(
        mode & 0o070,
        0,
        "die umask hat der Default-ACL die Gruppenrechte weggenommen: {mode:o}"
    );
}

#[test]
#[ignore = "braucht Linux, /dev/fuse und das Recht einzuhaengen"]
fn without_a_default_acl_the_umask_still_applies() {
    // Die Gegenprobe. Ohne Default-ACL gilt die `umask` — und da der Kernel
    // sie mit `FUSE_POSIX_ACL` nicht mehr selbst anwendet, muss dieser Server
    // es tun. Faellt das aus, entsteht jede Datei mit mehr Rechten als
    // bestellt, und das faellt niemandem auf.
    let Some(()) = prerequisites() else { return };
    let workspace = Workspace::new("acl-umask-gegenprobe");
    let branches = two_branches(&workspace);
    let pool = Mounted::start(&workspace, branches);
    let _ = std::fs::read_dir(pool.path(""));
    assert!(
        pool.counters.posix_acl_available.load(Ordering::Relaxed),
        "POSIX-ACLs wurden nicht ausgehandelt"
    );

    create_with_umask(&pool.path("neu.txt"), "027");
    let mode = std::fs::symlink_metadata(pool.path("neu.txt"))
        .expect("Attribute lesen")
        .mode();

    // `: > datei` bittet um 0o666.
    assert_eq!(
        mode & 0o777,
        0o640,
        "0o666 abzueglich umask 0o027 sind 0o640, nicht {:o}",
        mode & 0o777
    );
}

#[test]
#[ignore = "braucht Linux, /dev/fuse und das Recht einzuhaengen"]
fn a_directory_that_grows_takes_its_default_acl_along() {
    // Der Fall, an dem eine ACL sonst still auseinanderliefe: Das Verzeichnis
    // liegt auf Platte 0, die naechste Datei gehoert auf Platte 1. Entstuende
    // die Kopie dort ohne Default-ACL, bekaeme dieselbe Datei je nach Platte
    // andere Rechte — und wohin sie faellt, entscheidet die Platzierung.
    let Some(()) = prerequisites() else { return };
    let workspace = Workspace::new("acl-wachstum");
    if !have("setfacl") || !have("getfacl") {
        eprintln!("uebersprungen: setfacl/getfacl fehlen — Paket `acl` nicht installiert");
        return;
    }
    let Some((_mounts, branches)) = uneven(&workspace) else {
        return;
    };

    let ordner = workspace.branch_root(0).join("Ordner");
    std::fs::create_dir(&ordner).expect("Verzeichnis anlegen");
    std::fs::set_permissions(&ordner, std::fs::Permissions::from_mode(0o775))
        .expect("Rechte setzen");
    if acl_tool("setfacl", &["-d", "-m", "u:12345:rwx"], &ordner).is_err() {
        eprintln!("uebersprungen: die tmpfs-Branches nehmen keine POSIX-ACLs");
        return;
    }

    let pool = Mounted::with_policy(&workspace, branches, SharePolicy::default());
    std::fs::write(pool.path("Ordner/datei.bin"), b"x").expect("schreiben");

    assert_eq!(
        on_disk(&workspace, 2, "Ordner/datei.bin"),
        vec![1],
        "die Datei sollte auf der leereren Platte landen"
    );
    let text =
        acl_tool("getfacl", &["-c"], &workspace.branch_root(1).join("Ordner")).expect("ACL lesen");
    assert!(
        text.contains("default:user:12345:rwx"),
        "die Default-ACL kam nicht mit auf die zweite Platte:\n{text}"
    );
    let inherited = acl_tool(
        "getfacl",
        &["-c"],
        &workspace.branch_root(1).join("Ordner/datei.bin"),
    )
    .expect("ACL lesen");
    assert!(
        inherited.contains("user:12345:"),
        "die Datei auf Platte 1 hat die ACL nicht geerbt:\n{inherited}"
    );
}

#[test]
#[ignore = "braucht Linux, /dev/fuse und das Recht einzuhaengen"]
fn without_the_negotiated_bit_the_umask_beats_the_default_acl() {
    // Die Gegenprobe zur Aushandlung, und zugleich der Grund, warum sie
    // noetig ist.
    //
    // Ohne `FUSE_POSIX_ACL` weist der Kernel ACL-Attribute **nicht** zurueck:
    // `setfacl` gelingt, das Attribut landet auf der Platte, und es sieht von
    // aussen richtig aus. Nur zieht `fuse_create_open` weiterhin die `umask`
    // ab, und damit verliert die Default-ACL. Genau dieser stille Fall wird
    // hier festgehalten — er ist der Unterschied zwischen "ACLs gespeichert"
    // und "ACLs wirksam".
    let Some(()) = prerequisites() else { return };
    let workspace = Workspace::new("acl-abgeschaltet");
    let Some(()) = acls_or_skip(&workspace) else {
        return;
    };
    let branches = two_branches(&workspace);
    let pool = Mounted::without_acls(&workspace, branches);
    let _ = std::fs::read_dir(pool.path(""));
    assert!(!pool.counters.posix_acl_available.load(Ordering::Relaxed));

    std::fs::create_dir(pool.path("Freigabe")).expect("Verzeichnis anlegen");
    std::fs::set_permissions(
        pool.path("Freigabe"),
        std::fs::Permissions::from_mode(0o775),
    )
    .expect("Rechte setzen");
    // Gelingt — der Kernel legt ACL-Attribute auch ohne das Bit ab.
    acl_tool(
        "setfacl",
        &["-d", "-m", "u:12345:rwx"],
        &pool.path("Freigabe"),
    )
    .expect("Default-ACL setzen");

    create_with_umask(&pool.path("Freigabe/datei.txt"), "077");
    let mode = std::fs::symlink_metadata(pool.path("Freigabe/datei.txt"))
        .expect("Attribute lesen")
        .mode();

    assert_eq!(
        mode & 0o777,
        0o600,
        "ohne das ausgehandelte Bit gewinnt die umask — das ist der Fall, den
\n         `a_default_acl_wins_over_the_umask` ausschliesst"
    );
}

#[test]
#[ignore = "braucht Linux, /dev/fuse und das Recht einzuhaengen"]
fn the_same_file_can_be_open_three_times_at_once() {
    // Der Fall, an dem der Passthrough beim ersten Anlauf zerbrach: Der Kernel
    // laesst **eine** hinterlegte Datei je Inode zu. Bekam jedes Handle eine
    // eigene `backing_id`, schlug das zweite gleichzeitige Oeffnen mit `EIO`
    // fehl — und zwar jedes, nicht nur ein seltenes. Zwei Leser auf derselben
    // Datei sind der Normalfall eines NAS, nicht die Ausnahme.
    let Some(()) = prerequisites() else { return };
    let workspace = Workspace::new("mehrfach-offen");
    let branches = two_branches(&workspace);
    let content: Vec<u8> = (0..(1 << 20)).map(|i| (i % 249) as u8).collect();
    std::fs::create_dir_all(workspace.branch_root(0)).expect("Branch anlegen");
    std::fs::write(workspace.branch_root(0).join("gross.bin"), &content).expect("schreiben");

    let pool = Mounted::start(&workspace, branches);
    let Some(()) = passthrough_or_skip(&pool) else {
        return;
    };

    let mut open = Vec::new();
    for round in 0..3 {
        open.push(
            std::fs::File::open(pool.path("gross.bin"))
                .unwrap_or_else(|error| panic!("das {round}. gleichzeitige Oeffnen: {error}")),
        );
    }

    // Und jedes davon liefert wirklich die Datei, nicht nur einen Deskriptor.
    for (round, file) in open.iter().enumerate() {
        let mut buffer = vec![0u8; content.len()];
        file.read_exact_at(&mut buffer, 0)
            .unwrap_or_else(|error| panic!("Lesen aus Handle {round}: {error}"));
        assert_eq!(buffer, content, "Handle {round}");
    }

    assert_eq!(
        pool.counters.passthrough_opens.load(Ordering::Relaxed),
        3,
        "drei Handles"
    );
    assert_eq!(
        pool.counters.backing_opens.load(Ordering::Relaxed),
        1,
        "aber nur eine hinterlegte Datei — mehr nimmt der Kernel je Inode nicht"
    );
    assert_eq!(
        pool.counters.reads.load(Ordering::Relaxed),
        0,
        "auch das zweite und dritte Handle bedient der Kernel selbst"
    );

    // Erst wenn das letzte Handle geht, darf die Datei freigegeben werden.
    let last = open.pop().expect("drei Handles");
    drop(open);
    // Kurz warten, damit ein verfruehtes `RELEASE` eine Chance haette,
    // aufzufallen — aber nicht auf eines warten, das nicht kommen soll.
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert_eq!(
        pool.counters.backing_closes.load(Ordering::Relaxed),
        0,
        "solange ein Handle offen ist, darf die backing_id nicht weg sein"
    );

    drop(last);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while pool.counters.backing_closes.load(Ordering::Relaxed) < 1
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(
        pool.counters.backing_closes.load(Ordering::Relaxed),
        1,
        "nach dem letzten Handle muss sie freigegeben werden"
    );
}

// --- Sperren --------------------------------------------------------------
//
// Die Entscheidung, die diese Tests festhalten: Der Pool meldet weder
// `FUSE_POSIX_LOCKS` noch `FUSE_FLOCK_LOCKS` an. Dann fuehrt der Kernel
// `fcntl`- und `flock`-Sperren selbst, auf dem Inode des Pools — und damit
// fuer jeden Prozess auf dieser Maschine richtig. Genau das ist der Fall, der
// zaehlt: Samba, der NFS-Server und die VMs laufen hier.
//
// Sie selbst zu fuehren waere teurer und schlechter. Ein blockierendes
// `SETLKW` muesste die Schleife dieses Servers offenhalten, dazu kaemen
// Abbruch ueber `INTERRUPT` und eine eigene Buchfuehrung nach Besitzer — und
// am Ende stuende dieselbe Semantik, die der Kernel schon hat.
//
// Was dabei **nicht** geht, steht als eigener Test darunter: Eine Sperre ueber
// den Pool haelt niemanden auf, der die Platte unter dem Pool direkt oeffnet.
// Das ist keiner Union-Schicht anders moeglich und gehoert deshalb aufgeschrieben.

/// Was ein Kindprozess mit der Datei versuchen soll.
#[derive(Debug, Clone, Copy)]
enum Attempt {
    /// `flock(LOCK_EX | LOCK_NB)`.
    Flock,
    /// `fcntl(F_SETLK, F_WRLCK)` ueber einen Bereich.
    Range(u64, u64),
}

/// Versucht die Sperre in einem **eigenen Prozess** und sagt, ob er sie bekam.
///
/// Ein eigener Prozess ist Pflicht und keine Umstaendlichkeit: `flock` haengt
/// an der offenen Dateibeschreibung, `fcntl` am Prozess. Beides kollidiert
/// innerhalb desselben Prozesses nicht — ein Test, der die zweite Sperre im
/// Testprozess naehme, bekaeme sie immer und pruefte nichts.
///
/// Zwischen `fork` und `_exit` steht nur, was dort stehen darf: `open`,
/// `flock`/`fcntl`, `_exit`. Alles, was allokiert, ist vorher passiert.
fn lock_taken_by_another_process(path: &Path, attempt: Attempt) -> bool {
    let path = c_str(path);

    let child = unsafe { libc::fork() };
    assert!(child >= 0, "fork: {}", std::io::Error::last_os_error());
    if child == 0 {
        let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDWR) };
        if fd < 0 {
            let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
            unsafe { libc::_exit((100 + errno.min(50)) as libc::c_int) };
        }
        let got = match attempt {
            Attempt::Flock => (unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) }) == 0,
            Attempt::Range(start, len) => {
                let mut lock: libc::flock = unsafe { std::mem::zeroed() };
                lock.l_type = libc::F_WRLCK as libc::c_short;
                lock.l_whence = libc::SEEK_SET as libc::c_short;
                lock.l_start = start as libc::off_t;
                lock.l_len = len as libc::off_t;
                unsafe { libc::fcntl(fd, libc::F_SETLK, &lock) == 0 }
            }
        };
        unsafe { libc::_exit(if got { 0 } else { 1 }) };
    }

    let mut status = 0;
    let waited = unsafe { libc::waitpid(child, &mut status, 0) };
    assert_eq!(waited, child, "waitpid");
    let code = if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else {
        -1
    };
    assert!(
        code < 100,
        "das Kind konnte die Datei nicht oeffnen: errno {}",
        code - 100
    );
    assert!(code == 0 || code == 1, "unerwarteter Ausgang: {code}");
    code == 0
}

#[test]
#[ignore = "braucht Linux, /dev/fuse und das Recht einzuhaengen"]
fn a_flock_through_the_pool_keeps_the_next_process_out() {
    let Some(()) = prerequisites() else { return };
    let workspace = Workspace::new("flock");
    let branches = two_branches(&workspace);
    let pool = Mounted::start(&workspace, branches);

    let held = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(pool.path("nur-null.txt"))
        .expect("oeffnen");
    // Vorher: Ohne gehaltene Sperre muss das Kind sie bekommen. Sonst
    // bewiese der Test darunter nur, dass irgendetwas fehlschlaegt.
    assert!(
        lock_taken_by_another_process(&pool.path("nur-null.txt"), Attempt::Flock),
        "ohne gehaltene Sperre muss die Falle offen sein"
    );

    assert_eq!(
        unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0,
        "die eigene Sperre: {}",
        std::io::Error::last_os_error()
    );
    assert!(
        !lock_taken_by_another_process(&pool.path("nur-null.txt"), Attempt::Flock),
        "ein zweiter Prozess hat die Sperre trotzdem bekommen"
    );

    assert_eq!(unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_UN) }, 0);
    assert!(
        lock_taken_by_another_process(&pool.path("nur-null.txt"), Attempt::Flock),
        "nach dem Loslassen muss sie wieder zu haben sein"
    );

    assert_eq!(
        pool.counters.lock_requests.load(Ordering::Relaxed),
        0,
        "die Sperren fuehrt der Kernel, nicht dieser Server"
    );
}

#[test]
#[ignore = "braucht Linux, /dev/fuse und das Recht einzuhaengen"]
fn two_fcntl_ranges_in_the_same_file_do_not_collide() {
    // Bereichssperren sind das, was Datenbanken und Samba benutzen. Ein
    // Pool, der sie zu grob fuehrte, machte aus einer Datei einen
    // Flaschenhals — und einer, der sie zu fein fuehrte, liesse zwei
    // Schreiber auf dieselben Bytes.
    let Some(()) = prerequisites() else { return };
    let workspace = Workspace::new("fcntl");
    let branches = two_branches(&workspace);
    let pool = Mounted::start(&workspace, branches);

    let datei = pool.path("gross.bin");
    std::fs::write(&datei, vec![0u8; 4096]).expect("schreiben");
    let held = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&datei)
        .expect("oeffnen");

    let mut lock: libc::flock = unsafe { std::mem::zeroed() };
    lock.l_type = libc::F_WRLCK as libc::c_short;
    lock.l_whence = libc::SEEK_SET as libc::c_short;
    lock.l_start = 0;
    lock.l_len = 100;
    assert_eq!(
        unsafe { libc::fcntl(held.as_raw_fd(), libc::F_SETLK, &lock) },
        0,
        "die eigene Bereichssperre: {}",
        std::io::Error::last_os_error()
    );

    assert!(
        !lock_taken_by_another_process(&datei, Attempt::Range(50, 100)),
        "der ueberlappende Bereich haette gesperrt sein muessen"
    );
    assert!(
        lock_taken_by_another_process(&datei, Attempt::Range(100, 100)),
        "der Bereich dahinter ist frei und muss zu haben sein"
    );

    assert_eq!(
        pool.counters.lock_requests.load(Ordering::Relaxed),
        0,
        "die Sperren fuehrt der Kernel, nicht dieser Server"
    );
}

#[test]
#[ignore = "braucht Linux, /dev/fuse und das Recht einzuhaengen"]
fn a_lock_through_the_pool_does_not_reach_the_disk_underneath() {
    // Die Grenze, schwarz auf weiss. Eine Sperre gilt fuer den Inode des
    // Pools; wer die Platte darunter direkt oeffnet, sieht sie nicht. Keine
    // vereinigende Schicht kann das anders — nur muss es dastehen, damit es
    // niemand fuer einen Fehler haelt, wenn es ihm auffaellt.
    let Some(()) = prerequisites() else { return };
    let workspace = Workspace::new("sperre-grenze");
    let branches = two_branches(&workspace);
    let pool = Mounted::start(&workspace, branches);

    let held = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(pool.path("nur-null.txt"))
        .expect("oeffnen");
    assert_eq!(
        unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );

    assert!(
        lock_taken_by_another_process(
            &workspace.branch_root(0).join("nur-null.txt"),
            Attempt::Flock
        ),
        "unerwartet: die Sperre reicht bis auf die Platte — dann stimmt die \
         Beschreibung im Modulkopf nicht mehr"
    );
}

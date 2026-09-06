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
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use ferrite_pool::fuse::{BranchRoot, Connection, MountOptions, PoolFs};
use ferrite_pool::BranchId;

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
}

impl Mounted {
    /// Haengt ein und startet die Schleife in einem eigenen Thread.
    ///
    /// Eingehaengt wird **vor** dem Start des Threads: Danach ist der
    /// Einhaengepunkt live, und der Test kann ohne Wettlauf losgehen.
    fn start(workspace: &Workspace, branches: Vec<BranchRoot>) -> Self {
        let mountpoint = workspace.mountpoint();
        let connection =
            Connection::mount(&mountpoint, &MountOptions::default()).expect("Pool einhaengen");

        let worker = std::thread::spawn(move || {
            let mut filesystem = PoolFs::new(branches);
            if let Err(error) = filesystem.run(&connection) {
                eprintln!("die Schleife ist gestolpert: {error}");
            }
        });

        Mounted {
            mountpoint,
            worker: Some(worker),
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
        let mut mounted: Vec<PathBuf> = Vec::new();
        for target in [first, second] {
            std::fs::create_dir_all(target).expect("Branch-Wurzel anlegen");
            if mount_tmpfs(target).is_err() {
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

fn mount_tmpfs(target: &Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let source = CString::new("tmpfs")?;
    let fstype = CString::new("tmpfs")?;
    let target = CString::new(target.as_os_str().as_bytes())?;
    let options = CString::new("size=16m")?;
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

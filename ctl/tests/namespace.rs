// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Erkennt Ferrite, dass seine Mounts niemand sieht?
//!
//! Der Fehler dahinter stand auf einer echten Maschine: Der Dienst meldete
//! `Pool eingehaengt unter /mnt/pool` und `Bereit.`, und `findmnt` fand
//! nichts. Kein Fehler, kein Journal-Eintrag, `active (running)`.

use ferrite_ctl::namespace::{judge, Visibility, IM_EIGENEN_NAMESPACE};

#[test]
fn the_same_namespace_as_pid_one_means_the_machine_sees_our_mounts() {
    assert_eq!(
        judge(Some("mnt:[4026531840]"), Some("mnt:[4026531840]")),
        Visibility::Shared
    );
    // Der Zeilenumbruch, den `read_link` nicht liefert, aber eine Shell schon.
    assert_eq!(
        judge(Some("mnt:[4026531840]\n"), Some("mnt:[4026531840]")),
        Visibility::Shared
    );
}

#[test]
fn a_different_namespace_than_pid_one_means_nobody_else_sees_them() {
    assert_eq!(
        judge(Some("mnt:[4026532299]"), Some("mnt:[4026531840]")),
        Visibility::Private
    );
}

#[test]
fn not_being_able_to_look_is_not_the_same_as_having_looked() {
    // Der Unterschied, an dem es haengt: Ein Werkzeug, das „nicht lesbar" wie
    // „falsch" behandelt, verweigert eines Tages den Dienst auf einem System,
    // das voellig in Ordnung ist.
    assert!(matches!(
        judge(None, Some("mnt:[4026531840]")),
        Visibility::Unknown(_)
    ));
    assert!(matches!(
        judge(Some("mnt:[4026531840]"), None),
        Visibility::Unknown(_)
    ));
    assert!(matches!(judge(None, None), Visibility::Unknown(_)));
}

#[test]
fn the_message_names_the_directives_instead_of_describing_them() {
    // Wer diese Meldung liest, sitzt vor einer Unit-Datei. „Eine
    // Sandbox-Option" hilft ihm nicht; die Namen helfen ihm.
    for direktive in ["ProtectHome=no", "ProtectHostname=no", "PrivateMounts=no"] {
        assert!(
            IM_EIGENEN_NAMESPACE.contains(direktive),
            "die Meldung nennt {direktive} nicht"
        );
    }
    // Und den Weg, der ohne Bearbeiten der Paketdatei auskommt.
    assert!(IM_EIGENEN_NAMESPACE.contains("ferrite.service.d"));
}

/// Gegen einen echten Namespace, nicht gegen ausgedachte Zeichenketten.
///
/// Braucht Root: `readlink /proc/1/ns/mnt` verlangt Ptrace-Zugriff auf PID 1.
/// Ohne den kaeme `Unknown` heraus, und ein Test, der sich mit `Unknown`
/// zufriedengibt, prueft nichts.
///
/// Darum `#[ignore]` und kein Ueberspringen zur Laufzeit: Der gewoehnliche
/// `cargo test`-Lauf in CI ist nicht Root. Ein Test, der sich dort selbst
/// stillschweigend abmeldet, waere immer gruen und bewiese nichts — er wird
/// eigens unter `sudo` gestartet, oder er laeuft gar nicht.
#[cfg(target_os = "linux")]
#[test]
#[ignore = "braucht Root; laeuft in CI unter sudo"]
fn an_actual_private_mount_namespace_is_recognised_as_one() {
    use std::process::Command;

    // SAFETY: `geteuid` liest nur.
    assert_eq!(
        unsafe { libc::geteuid() },
        0,
        "dieser Test braucht Root — ohne ihn ist /proc/1/ns/mnt nicht lesbar"
    );

    let lesen = "readlink /proc/self/ns/mnt; readlink /proc/1/ns/mnt";
    let urteil = |ausgabe: std::process::Output| {
        let text = String::from_utf8_lossy(&ausgabe.stdout).into_owned();
        let mut zeilen = text.lines();
        let own = zeilen.next().map(str::to_owned);
        let init = zeilen.next().map(str::to_owned);
        judge(own.as_deref(), init.as_deref())
    };

    let normal = Command::new("sh")
        .args(["-c", lesen])
        .output()
        .expect("sh laeuft");
    if urteil(normal) != Visibility::Shared {
        // Nicht unser Ergebnis, sondern die Umgebung: Wer die Tests selbst
        // schon in einem eigenen Namespace startet, kann den Unterschied
        // nicht messen, den dieser Test messen will.
        eprintln!("uebersprungen: die Testumgebung laeuft selbst in einem eigenen Namespace");
        return;
    }

    let unshared = Command::new("unshare")
        .args(["--mount", "sh", "-c", lesen])
        .output();
    // Kein Ueberspringen: `unshare` kommt aus util-linux und ist auf jedem
    // System da, auf dem dieser Test ueberhaupt gestartet wird. Fehlte es,
    // haette der Lauf nichts geprueft, und das soll man sehen.
    let unshared = unshared.expect("unshare(1) aus util-linux fehlt");
    assert!(
        unshared.status.success(),
        "unshare --mount schlug fehl: {}",
        String::from_utf8_lossy(&unshared.stderr)
    );
    assert_eq!(
        urteil(unshared),
        Visibility::Private,
        "ein echter eigener Mount-Namespace wurde nicht als solcher erkannt"
    );
}

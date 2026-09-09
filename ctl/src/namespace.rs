// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Sieht der Rest der Maschine, was wir einhaengen?
//!
//! # Der Fehler, der das hier noetig gemacht hat
//!
//! Auf einer Testmaschine lief der Dienst, meldete
//! `Pool eingehaengt unter /mnt/pool` und `Bereit.` — und `findmnt /mnt/pool`
//! fand nichts. Der Mount war da, nur in einem eigenen Mount-Namespace, den
//! systemd wegen eines `ProtectHome=yes` in der Unit aufgemacht hatte. Ein
//! Dateiserver, dessen Dateien nur er selbst sieht.
//!
//! Daran ist das Unangenehme nicht der Namespace, sondern die Stille: Der
//! Startlauf war erfolgreich, das Journal fehlerfrei, `systemctl status` sagte
//! `active (running)`. Von Hand gestartet lief dasselbe Programm richtig. Es
//! gab kein einziges Zeichen, an dem man den Unterschied haette sehen koennen.
//!
//! # Wie es sich feststellen laesst
//!
//! Jeder Prozess traegt seinen Mount-Namespace als Symlink unter
//! `/proc/self/ns/mnt`; das Ziel ist eine Kennung der Form `mnt:[4026531840]`.
//! Steht dort etwas anderes als bei PID 1, dann sind unsere Mounts fuer den
//! Rest der Maschine nicht da. Der Vergleich gegen PID 1 und nicht gegen einen
//! festen Wert ist Absicht: In einem Container ist PID 1 der Container-Init,
//! und dann stimmt das Urteil dort genauso.
//!
//! # Warum das ein Abbruch ist und keine Warnung
//!
//! Ein Pool, den niemand sieht, hat keinen Betriebsfall. Wer ihn einhaengt,
//! will, dass Samba, NFS oder ein Mensch hineinsehen — und keines davon
//! laeuft in unserem Namespace. Eine Warnung im Journal waere genau die Art
//! Meldung, die zwischen zwoelf anderen untergeht.

/// Wer sieht die Mounts dieses Prozesses?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    /// Derselbe Namespace wie PID 1: Was wir einhaengen, sieht die Maschine.
    Shared,
    /// Ein eigener Namespace. Was wir einhaengen, bleibt bei uns.
    Private,
    /// Nicht feststellbar, mit dem Grund.
    ///
    /// Kein Abbruch. Nicht nachsehen zu koennen ist etwas anderes als
    /// nachgesehen und Schlechtes gefunden zu haben, und ein Werkzeug, das
    /// beides gleich behandelt, verweigert eines Tages den Dienst auf einem
    /// System, das voellig in Ordnung ist.
    Unknown(&'static str),
}

/// Das Urteil aus zwei Namespace-Kennungen.
///
/// Getrennt vom Lesen, damit es sich ohne `/proc` und ohne Linux pruefen
/// laesst — und damit die Entscheidung an einer Stelle steht, die man ansehen
/// kann.
pub fn judge(own: Option<&str>, init: Option<&str>) -> Visibility {
    match (own, init) {
        (Some(own), Some(init)) if own.trim() == init.trim() => Visibility::Shared,
        (Some(_), Some(_)) => Visibility::Private,
        (None, _) => Visibility::Unknown("/proc/self/ns/mnt ist nicht lesbar"),
        (_, None) => Visibility::Unknown("/proc/1/ns/mnt ist nicht lesbar"),
    }
}

/// Was der Benutzer tun muss, wenn wir im eigenen Namespace stecken.
///
/// Die Direktiven stehen namentlich da: Wer diese Meldung liest, sitzt vor
/// einer Unit-Datei und soll nicht erst herausfinden muessen, welche der
/// zwanzig `Protect*`-Optionen es ist.
pub const IM_EIGENEN_NAMESPACE: &str = "\
dieser Prozess steckt in einem eigenen Mount-Namespace — der Pool waere \
eingehaengt, aber fuer den Rest der Maschine unsichtbar.\n\
  Als systemd-Dienst kommt das von einer Sandbox-Option in der Unit. Setze in\n\
  /etc/systemd/system/ferrite.service.d/namespace.conf:\n\
    [Service]\n\
    ProtectHome=no\n\
    ProtectHostname=no\n\
    PrivateMounts=no\n\
  Danach: systemctl daemon-reload && systemctl restart ferrite\n\
  Von Hand gestartet: nicht unter `unshare -m` aufrufen.";

/// Nachsehen, in welchem Namespace dieser Prozess laeuft.
#[cfg(target_os = "linux")]
pub fn look() -> Visibility {
    fn kennung(pfad: &str) -> Option<String> {
        std::fs::read_link(pfad)
            .ok()
            .map(|ziel| ziel.to_string_lossy().into_owned())
    }
    judge(
        kennung("/proc/self/ns/mnt").as_deref(),
        kennung("/proc/1/ns/mnt").as_deref(),
    )
}

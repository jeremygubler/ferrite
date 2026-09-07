// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler
//
// Die Oberflaeche zu Ferrite, als Cockpit-Modul.
//
// # Was hier bewusst fehlt
//
// Kein Framework, kein Build-Schritt, keine Abhaengigkeit. Dieselbe
// Ueberlegung wie bei ublk und FUSE: Was ein Betreiber im Fehlerfall lesen
// koennen soll, steht so im Paket, wie es hier steht. Eine gebuendelte
// Javascript-Datei koennte er nicht mehr lesen.
//
// # Was hier bewusst nicht passiert
//
// **Nichts wird geschrieben.** Kein Knopf loest einen Scrub aus, kein Knopf
// startet einen Rebuild. Eine Oberflaeche, die Platten beschreiben kann,
// braucht eine Rueckfrage, ein Rechtekonzept und einen Wiederanlauf nach dem
// geschlossenen Browserfenster — und alle drei gehoeren entschieden, nicht
// nebenbei gebaut. Bis dahin zeigt diese Seite an und tut nichts.

"use strict";

/// Wie oft neu gelesen wird.
///
/// Zwanzig Sekunden: Ein Rebuild bewegt sich in Minuten, ein Scrub in
/// Stunden. Haeufiger zu fragen liest nur Superbloecke, die sich nicht
/// geaendert haben, und weckt bei Platten im Standby genau die auf, die
/// schlafen sollen.
const INTERVALL = 20000;

/// Ruft `ferrite` auf und gibt das JSON zurueck.
///
/// `superuser: "require"` — die Superbloecke liegen auf Blockgeraeten, und
/// die liest kein gewoehnlicher Nutzer. Cockpit fragt dann selbst nach dem
/// Passwort; das ist genau die Stelle, an der es hingehoert.
function frage(argumente) {
    return cockpit
        .spawn(["ferrite"].concat(argumente), {
            superuser: "require",
            err: "message",
        })
        .then(function (ausgabe) {
            return JSON.parse(ausgabe);
        })
        .catch(function (fehler) {
            // `ferrite status` liefert bei einem degradierten Array den
            // Rueckgabewert 1. Das ist ein Befund und kein Fehler des
            // Aufrufs — die Ausgabe steht trotzdem da und ist genau die, die
            // angezeigt werden soll.
            if (fehler.exit_status !== undefined && typeof fehler.message === "string") {
                try {
                    return JSON.parse(fehler.message);
                } catch (ignoriert) {
                    void ignoriert;
                }
            }
            if (typeof fehler.problem === "string" || typeof fehler.message === "string") {
                throw new Error(fehler.message || fehler.problem);
            }
            throw fehler;
        });
}

/// Ein Element mit Klassen und Text.
function element(name, klasse, text) {
    const knoten = document.createElement(name);
    if (klasse) {
        knoten.className = klasse;
    }
    if (text !== undefined && text !== null) {
        knoten.textContent = String(text);
    }
    return knoten;
}

/// Bytes als etwas, das ein Mensch liest.
///
/// Basis 1024 und dieselben Einheitennamen wie im Werkzeug: Wer beides
/// nebeneinander sieht, soll nicht rechnen muessen, ob 4 TB und 3,6 TiB
/// dasselbe sind.
function groesse(bytes) {
    if (bytes === null || bytes === undefined) {
        return "—";
    }
    const einheiten = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let wert = bytes;
    let index = 0;
    while (wert >= 1024 && index < einheiten.length - 1) {
        wert /= 1024;
        index += 1;
    }
    const gerundet = index === 0 ? wert : wert.toFixed(1);
    return gerundet + " " + einheiten[index];
}

const ZUSTAND = {
    healthy: { text: "In Ordnung", klasse: "gut" },
    degraded: { text: "Degradiert", klasse: "warnung" },
    broken: { text: "Nicht zusammensetzbar", klasse: "schlecht" },
};

const ROLLE = {
    data: "Daten",
    "parity-p": "Paritaet P",
    "parity-q": "Paritaet Q",
    log: "Log",
};

const MEMBER_ZUSTAND = {
    clean: { text: "in Ordnung", klasse: "gut" },
    stale: { text: "unbrauchbar, wartet auf Rebuild", klasse: "schlecht" },
    rebuilding: { text: "Rebuild laeuft", klasse: "warnung" },
};

/// Die Kopfzeile: der eine Satz, wegen dem jemand die Seite aufmacht.
function kopf(status) {
    const zustand = ZUSTAND[status.health] || {
        text: status.health,
        klasse: "warnung",
    };
    const abschnitt = element("section", "karte kopf " + zustand.klasse);
    abschnitt.appendChild(element("h2", null, zustand.text));

    if (status.error) {
        abschnitt.appendChild(element("p", "grund", status.error));
        return abschnitt;
    }
    if (status.cannot_assemble) {
        abschnitt.appendChild(
            element("p", "grund", "Ergibt kein Array: " + status.cannot_assemble),
        );
    }
    if (status.array) {
        const zeile = element("p", "kennung");
        zeile.appendChild(element("span", "uuid", status.array));
        zeile.appendChild(
            element(
                "span",
                "nebensache",
                status.data_slots +
                    " Data-Slots, Blockgroesse " +
                    groesse(status.block_size),
            ),
        );
        abschnitt.appendChild(zeile);
    }
    return abschnitt;
}

/// Die Platten, eine Zeile je Member.
function platten(status) {
    const abschnitt = element("section", "karte");
    abschnitt.appendChild(element("h3", null, "Platten"));

    if (!status.members || status.members.length === 0) {
        abschnitt.appendChild(
            element("p", "nebensache", "Keine Members gefunden."),
        );
        return abschnitt;
    }

    const tabelle = element("table");
    const kopfzeile = element("tr");
    ["Rolle", "Geraet", "Zustand", "Groesse"].forEach(function (titel) {
        kopfzeile.appendChild(element("th", null, titel));
    });
    tabelle.appendChild(kopfzeile);

    status.members.forEach(function (member) {
        const zustand = MEMBER_ZUSTAND[member.state] || {
            text: member.state,
            klasse: "warnung",
        };
        const zeile = element("tr");
        const rolle = ROLLE[member.role] || member.role;
        zeile.appendChild(
            element(
                "td",
                null,
                member.role === "data" ? rolle + " " + member.slot : rolle,
            ),
        );
        zeile.appendChild(element("td", "geraet", member.device));

        const zelle = element("td", zustand.klasse, zustand.text);
        // Der Fortschritt kommt in Bytes. Prozente rechnet die Oberflaeche —
        // so kann sie sich nicht mit Bloecken vertun, und die Zahl im
        // Superblock bleibt die Zahl im Superblock.
        if (member.state === "rebuilding" && member.size > 0) {
            const anteil = Math.floor((member.rebuilt * 100) / member.size);
            zelle.textContent = "Rebuild bei " + anteil + " %";
            const balken = element("div", "balken");
            const gefuellt = element("div", "gefuellt");
            gefuellt.style.width = anteil + "%";
            balken.appendChild(gefuellt);
            zelle.appendChild(balken);
        }
        zeile.appendChild(zelle);
        zeile.appendChild(element("td", "zahl", groesse(member.size)));
        tabelle.appendChild(zeile);
    });
    abschnitt.appendChild(tabelle);

    // Was nicht dazugehoert, gehoert trotzdem gesagt: Wer es uebersieht,
    // nimmt eine fremde Platte versehentlich in dieses Array auf.
    (status.foreign || []).forEach(function (fremd) {
        abschnitt.appendChild(
            element(
                "p",
                "schlecht",
                fremd.device + " gehoert zu Array " + fremd.array + " — nicht zu diesem.",
            ),
        );
    });
    (status.without_superblock || []).forEach(function (pfad) {
        abschnitt.appendChild(
            element("p", "nebensache", pfad + ": kein Ferrite-Superblock."),
        );
    });

    return abschnitt;
}

/// Das Betriebstagebuch — die beiden Zahlen, um die es geht, zuerst.
function tagebuch(zusammenfassung) {
    const abschnitt = element("section", "karte");
    abschnitt.appendChild(element("h3", null, "Betriebstagebuch"));

    if (zusammenfassung === null) {
        abschnitt.appendChild(
            element(
                "p",
                "nebensache",
                "Es wird kein Tagebuch gefuehrt. `journal = /var/lib/ferrite/journal` " +
                    "in /etc/ferrite/ferrite.conf eintragen — ohne Aufzeichnung ergibt " +
                    "ein Jahr Betrieb nichts, was sich vorzeigen liesse.",
            ),
        );
        return abschnitt;
    }

    // Verloren und abgelehnt zuerst und in Rot, wenn sie nicht null sind.
    // Das sind die beiden Zahlen, die ein Jahr Betrieb wirklich beantwortet;
    // alles andere ist Zaehlwerk.
    const wichtig = [
        ["Bereiche verloren", zusammenfassung.ranges_lost],
        ["Reparaturen abgelehnt", zusammenfassung.repairs_refused],
    ];
    const liste = element("dl", "zahlen");
    wichtig.forEach(function (paar) {
        liste.appendChild(element("dt", paar[1] > 0 ? "wichtig schlecht" : "wichtig", paar[0]));
        liste.appendChild(
            element("dd", paar[1] > 0 ? "wichtig schlecht" : "wichtig gut", paar[1]),
        );
    });
    [
        ["Betriebsstunden", zusammenfassung.hours],
        ["Starts", zusammenfassung.starts],
        ["Scrubs", zusammenfassung.scrubs],
        ["Bit-Rot repariert", zusammenfassung.bit_rot_repaired],
        ["Dabei zurueckgeschrieben", groesse(zusammenfassung.bytes_repaired)],
        ["Rebuilds", zusammenfassung.rebuilds],
        ["Plattenwechsel", zusammenfassung.replacements],
    ].forEach(function (paar) {
        liste.appendChild(element("dt", null, paar[0]));
        liste.appendChild(element("dd", null, paar[1]));
    });
    abschnitt.appendChild(liste);
    return abschnitt;
}

/// Ein Fehler, den die Seite nicht auffangen kann.
function stoerung(fehler) {
    const abschnitt = element("section", "karte kopf schlecht");
    abschnitt.appendChild(element("h2", null, "Keine Auskunft"));
    abschnitt.appendChild(element("p", "grund", String(fehler.message || fehler)));
    abschnitt.appendChild(
        element(
            "p",
            "nebensache",
            "Ist `ferrite` installiert und liegt unter /usr/bin? " +
                "Sonst sagt `ferrite status` auf der Kommandozeile mehr.",
        ),
    );
    return abschnitt;
}

function zeichne(kinder) {
    const seite = document.getElementById("seite");
    seite.textContent = "";
    kinder.forEach(function (kind) {
        seite.appendChild(kind);
    });
    // Wann zuletzt gelesen wurde.
    //
    // Nicht Zierrat: Diese Seite laeuft ein Jahr lang auf einem Monitor oder
    // in einem Telefonbrowser, der sie im Hintergrund einfriert. Ohne diese
    // Zeile sieht ein Bild von gestern genauso aus wie eines von jetzt — und
    // "in Ordnung" von gestern ist keine Auskunft.
    const fuss = element("p", "nebensache fuss");
    fuss.textContent =
        "Zuletzt gelesen: " + new Date().toLocaleTimeString() + " · alle " +
        INTERVALL / 1000 + " s";
    seite.appendChild(fuss);
}

function lies() {
    // Das Tagebuch darf fehlen, ohne dass die Seite leer bleibt: Ohne
    // `journal =` in der Konfiguration gibt es keines, und das ist kein
    // Fehler, sondern eine Einstellung.
    const zustand = frage(["status", "--json"]);
    const buch = frage(["journal", "--json"]).catch(function () {
        return null;
    });

    Promise.all([zustand, buch])
        .then(function (antworten) {
            zeichne([
                kopf(antworten[0]),
                platten(antworten[0]),
                tagebuch(antworten[1]),
            ]);
        })
        .catch(function (fehler) {
            zeichne([stoerung(fehler)]);
        });
}

lies();
window.setInterval(lies, INTERVALL);

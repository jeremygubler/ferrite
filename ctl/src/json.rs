// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! JSON schreiben, für alles, was den Zustand weiterverarbeitet.
//!
//! # Wofür
//!
//! Die Ausgabe von `ferrite status` ist für Menschen gesetzt: Spalten,
//! deutsche Saetze, Groessen in MiB. Eine Oberflaeche, die das zerlegt,
//! bricht beim ersten geaenderten Wort — und zwar still, weil ein
//! Regulaerausdruck, der nichts findet, keinen Fehler wirft, sondern eine
//! leere Liste.
//!
//! Deshalb gibt es dieselben Zahlen noch einmal maschinenlesbar. Der Text
//! bleibt, was er ist, und darf sich aendern.
//!
//! # Warum von Hand und ohne Crate
//!
//! Regel 2 sinngemaess: `ctl/` traegt keine fremden Crates. JSON zu schreiben
//! — nicht zu lesen — ist eine Handvoll Zeilen, und die Fehler, die man dabei
//! machen kann, sind alle im Escaping. Genau das steht hier an einer Stelle
//! und ist einzeln geprueft.
//!
//! Gebaut wird ueber [`Value`] und nicht durch Aneinanderhaengen von Strings:
//! Ein Baum kann keine unbalancierten Klammern erzeugen.

/// Ein JSON-Wert.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    /// Ganze Zahlen. Fliesskomma gibt es hier nicht — jede Groesse in Ferrite
    /// ist eine Anzahl Bytes, und ein Prozentsatz wird ganzzahlig gerechnet.
    /// Ein `f64` brachte nur die Frage mit, wie `NaN` aussieht.
    Number(u64),
    Text(String),
    List(Vec<Value>),
    /// Reihenfolge bleibt, wie sie eingetragen wurde: Eine Ausgabe, die sich
    /// bei jedem Aufruf anders sortiert, laesst sich nicht vergleichen.
    Object(Vec<(String, Value)>),
}

impl Value {
    /// Ein Objekt aus Paaren.
    pub fn object<K: Into<String>>(fields: Vec<(K, Value)>) -> Value {
        Value::Object(
            fields
                .into_iter()
                .map(|(key, value)| (key.into(), value))
                .collect(),
        )
    }

    /// Eine Zeichenkette.
    pub fn text<T: Into<String>>(value: T) -> Value {
        Value::Text(value.into())
    }

    /// Der Wert als JSON, ohne Zeilenumbrueche.
    pub fn render(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }

    fn write(&self, out: &mut String) {
        match self {
            Value::Null => out.push_str("null"),
            Value::Bool(true) => out.push_str("true"),
            Value::Bool(false) => out.push_str("false"),
            Value::Number(number) => out.push_str(&number.to_string()),
            Value::Text(text) => escape(text, out),
            Value::List(items) => {
                out.push('[');
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    item.write(out);
                }
                out.push(']');
            }
            Value::Object(fields) => {
                out.push('{');
                for (index, (key, value)) in fields.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    escape(key, out);
                    out.push(':');
                    value.write(out);
                }
                out.push('}');
            }
        }
    }
}

/// Schreibt eine Zeichenkette als JSON-String.
///
/// # Was hier schiefgehen kann
///
/// Ein Geraetepfad darf jedes Byte ausser `/` und `\0` enthalten, ein
/// Fehlertext kommt vom Betriebssystem, und ein Array-Label hat der Betreiber
/// gesetzt. Ein Anfuehrungszeichen oder ein Backslash darin macht aus
/// gueltigem JSON kaputtes — und die Oberflaeche zeigt dann nichts an, statt
/// zu sagen, warum.
fn escape(text: &str, out: &mut String) {
    out.push('"');
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // Alles unter 0x20 muss escaped werden; JSON kennt nur fuer
            // wenige davon eine Kurzform.
            control if (control as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", control as u32));
            }
            // Alles andere geht als UTF-8 durch. Das ist gueltiges JSON, und
            // ein deutscher Umlaut in einem Fehlertext soll ankommen.
            other => out.push(other),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_simple_values_look_like_json() {
        assert_eq!(Value::Null.render(), "null");
        assert_eq!(Value::Bool(true).render(), "true");
        assert_eq!(Value::Bool(false).render(), "false");
        assert_eq!(Value::Number(0).render(), "0");
        assert_eq!(Value::Number(u64::MAX).render(), "18446744073709551615");
        assert_eq!(Value::text("hallo").render(), "\"hallo\"");
    }

    #[test]
    fn an_empty_list_and_an_empty_object_are_still_valid() {
        assert_eq!(Value::List(vec![]).render(), "[]");
        assert_eq!(Value::Object(vec![]).render(), "{}");
    }

    #[test]
    fn a_quote_in_a_device_path_does_not_break_the_output() {
        // `/dev/disk/by-id/` traegt, was der Hersteller ins Typenschild
        // geschrieben hat. Ein Anfuehrungszeichen darin machte aus gueltigem
        // JSON kaputtes, und die Oberflaeche zeigte danach gar nichts.
        assert_eq!(Value::text("a\"b").render(), "\"a\\\"b\"");
        assert_eq!(Value::text("a\\b").render(), "\"a\\\\b\"");
        assert_eq!(Value::text("a\nb").render(), "\"a\\nb\"");
        assert_eq!(Value::text("a\tb").render(), "\"a\\tb\"");
        assert_eq!(Value::text("a\rb").render(), "\"a\\rb\"");
    }

    #[test]
    fn a_control_character_becomes_an_escape_sequence() {
        assert_eq!(Value::text("a\u{1}b").render(), "\"a\\u0001b\"");
        assert_eq!(Value::text("\u{0}").render(), "\"\\u0000\"");
        assert_eq!(Value::text("\u{1f}").render(), "\"\\u001f\"");
        // 0x20 ist ein gewoehnliches Leerzeichen und bleibt eines.
        assert_eq!(Value::text(" ").render(), "\" \"");
    }

    #[test]
    fn an_umlaut_stays_an_umlaut() {
        // Gueltiges JSON ist UTF-8. Ein Fehlertext des Betriebssystems auf
        // Deutsch soll ankommen und nicht als \u00e4 durchgereicht werden.
        assert_eq!(Value::text("größer").render(), "\"größer\"");
    }

    #[test]
    fn a_key_is_escaped_like_a_value() {
        let value = Value::object(vec![("a\"b", Value::Number(1))]);
        assert_eq!(value.render(), "{\"a\\\"b\":1}");
    }

    #[test]
    fn nesting_keeps_the_brackets_balanced() {
        let value = Value::object(vec![
            ("array", Value::text("uuid")),
            (
                "members",
                Value::List(vec![
                    Value::object(vec![("slot", Value::Number(0))]),
                    Value::object(vec![("slot", Value::Number(1))]),
                ]),
            ),
            ("fehler", Value::Null),
        ]);
        assert_eq!(
            value.render(),
            "{\"array\":\"uuid\",\"members\":[{\"slot\":0},{\"slot\":1}],\"fehler\":null}"
        );
    }

    #[test]
    fn the_order_of_the_fields_is_kept() {
        // Eine Ausgabe, die sich bei jedem Aufruf anders sortiert, laesst sich
        // nicht vergleichen — weder von Hand noch in einem Test.
        let value = Value::object(vec![
            ("z", Value::Number(1)),
            ("a", Value::Number(2)),
            ("m", Value::Number(3)),
        ]);
        assert_eq!(value.render(), "{\"z\":1,\"a\":2,\"m\":3}");
    }
}

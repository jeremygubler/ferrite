// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Ein Array ueber mehrere Geraete anlegen und wieder oeffnen.
//!
//! Der Durchstich, den Meilenstein 2 verlangt: Superbloecke fuer Data-,
//! Parity- und Log-Member schreiben, danach ueber `assemble` einlesen und
//! vergleichen. Unterbau sind Dateien; was nur ein echtes Blockgeraet zeigt,
//! steht in `loop_device.rs`.

use std::fs::File;
use std::path::PathBuf;

use ferrite_engine::{
    create_array, max_payload_size, open_array, ArraySpec, EngineError, MemberDevice, MemberSpec,
};
use ferrite_format::superblock::{Role, DEFAULT_PAYLOAD_OFFSET, SUPERBLOCK_BACKUP_FROM_END};
use ferrite_format::{FormatError, Uuid};

const BLOCK_LOG2: u8 = 16;
const BLOCK: u64 = 1 << BLOCK_LOG2;

/// Ein Satz Geraete, der sich nach dem Test selbst wegraeumt.
struct Devices {
    paths: Vec<PathBuf>,
    devices: Vec<MemberDevice>,
}

impl Devices {
    fn new(name: &str, sizes: &[u64]) -> Self {
        let mut paths = Vec::new();
        let mut devices = Vec::new();
        for (nth, size) in sizes.iter().enumerate() {
            let path = std::env::temp_dir().join(format!(
                "ferrite-array-{name}-{}-{nth}.img",
                std::process::id()
            ));
            let file = File::create(&path).expect("Datei anlegen");
            file.set_len(*size).expect("Groesse setzen");
            drop(file);
            devices.push(MemberDevice::open(&path).expect("oeffnen"));
            paths.push(path);
        }
        Devices { paths, devices }
    }

    fn as_slice(&self) -> &[MemberDevice] {
        &self.devices
    }

    /// Oeffnet dieselben Geraete noch einmal, so wie es ein Neustart taete.
    fn reopen(&self) -> Vec<MemberDevice> {
        self.paths
            .iter()
            .map(|path| MemberDevice::open(path).expect("erneut oeffnen"))
            .collect()
    }
}

impl Drop for Devices {
    fn drop(&mut self) {
        for path in &self.paths {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn spec() -> ArraySpec {
    ArraySpec {
        array_uuid: Uuid::from_random_bytes([0x11; 16]),
        parity_block_size_log2: BLOCK_LOG2,
        created_unix: 1_767_225_600,
    }
}

fn member(nth: u8, role: Role, slot_index: u16) -> MemberSpec {
    MemberSpec {
        member_uuid: Uuid::from_random_bytes([nth; 16]),
        role,
        slot_index,
        label: format!("member-{nth}"),
    }
}

/// Geraet, auf das `blocks` ganze Parity-Bloecke passen, plus Reserve.
fn device_size(blocks: u64) -> u64 {
    DEFAULT_PAYLOAD_OFFSET + blocks * BLOCK + SUPERBLOCK_BACKUP_FROM_END
}

// --- Nutzbare Groesse -----------------------------------------------------

#[test]
fn the_payload_uses_everything_between_the_two_superblocks() {
    let size = device_size(12);
    assert_eq!(max_payload_size(size, BLOCK_LOG2).unwrap(), 12 * BLOCK);
}

#[test]
fn a_partial_last_block_is_left_unused() {
    // Ein angebrochener Block haette auf dem Parity-Member kein Gegenstueck
    // voller Breite. Die Kerninvariante rechnet ueber Bloecke, nicht ueber
    // Bytes — also faellt der Rest weg statt halb mitgenommen zu werden.
    let size = device_size(12) + BLOCK - 1;
    assert_eq!(max_payload_size(size, BLOCK_LOG2).unwrap(), 12 * BLOCK);
}

#[test]
fn a_device_too_small_for_one_block_is_refused() {
    let size = device_size(0) + BLOCK - 1;
    assert!(matches!(
        max_payload_size(size, BLOCK_LOG2),
        Err(EngineError::Format(FormatError::InvalidField {
            field: "device_size",
            ..
        }))
    ));
}

// --- Anlegen und wieder oeffnen ------------------------------------------

#[test]
fn an_array_written_to_devices_comes_back_through_assemble() {
    let devices = Devices::new("roundtrip", &[device_size(8); 5]);
    let specs = [
        member(0, Role::Data, 0),
        member(1, Role::Data, 1),
        member(2, Role::Data, 2),
        member(3, Role::ParityP, 0),
        member(4, Role::Log, 0),
    ];

    let written = create_array(devices.as_slice(), &specs, &spec()).unwrap();

    // Neu oeffnen — was zurueckkommt, kommt von der Platte und nicht aus dem
    // Vec, das `create_array` gerade zurueckgegeben hat.
    let reopened = devices.reopen();
    let array = open_array(&reopened).unwrap();

    assert_eq!(array.superblocks(), written);
    assert_eq!(array.layout().data_slot_count(), 3);
    assert_eq!(array.layout().parity_block_size(), BLOCK);
    assert_eq!(array.layout().array_uuid(), spec().array_uuid);
    assert!(array.log().is_some());
    assert!(array.parity_q().is_none());

    // Die Positionen im Layout zeigen wirklich auf die passenden Geraete.
    for slot in 0..3u16 {
        let found = array.data_member(slot).expect("Slot besetzt");
        assert_eq!(found.slot_index, slot);
        assert_eq!(found.label, format!("member-{slot}"));
    }
    assert_eq!(array.parity_p().role, Role::ParityP);
    assert_eq!(array.log().unwrap().role, Role::Log);
}

#[test]
fn members_of_different_sizes_form_one_array() {
    // Der Grund, warum es dieses Projekt gibt. Drei verschieden grosse
    // Data-Members, jeder behaelt seine eigene Payload-Groesse, und die
    // Paritaet deckt den groessten davon.
    let devices = Devices::new(
        "gemischt",
        &[
            device_size(4),
            device_size(9),
            device_size(6),
            device_size(9),
        ],
    );
    let specs = [
        member(0, Role::Data, 0),
        member(1, Role::Data, 1),
        member(2, Role::Data, 2),
        member(3, Role::ParityP, 0),
    ];

    create_array(devices.as_slice(), &specs, &spec()).unwrap();
    let reopened = devices.reopen();
    let array = open_array(&reopened).unwrap();

    assert_eq!(array.data_member(0).unwrap().payload_size, 4 * BLOCK);
    assert_eq!(array.data_member(1).unwrap().payload_size, 9 * BLOCK);
    assert_eq!(array.data_member(2).unwrap().payload_size, 6 * BLOCK);
    assert_eq!(array.parity_p().payload_size, 9 * BLOCK);
}

#[test]
fn a_parity_member_smaller_than_the_largest_data_member_is_refused() {
    // Regel 6 aus Abschnitt 2.1. Hinter dem Ende des Parity-Members laege
    // keine Redundanz mehr — sie endete still mitten im Array.
    let devices = Devices::new("kurze-paritaet", &[device_size(9), device_size(4)]);
    let specs = [member(0, Role::Data, 0), member(1, Role::ParityP, 0)];

    assert!(matches!(
        create_array(devices.as_slice(), &specs, &spec()),
        Err(EngineError::Format(_))
    ));
}

#[test]
fn nothing_is_written_when_the_array_does_not_hold() {
    // Abgelehnt wird vor dem ersten Schreibvorgang. Sonst stuenden hinterher
    // Superbloecke auf echten Platten, die zu keinem Array gehoeren.
    let devices = Devices::new("nichts-geschrieben", &[device_size(9), device_size(4)]);
    let specs = [member(0, Role::Data, 0), member(1, Role::ParityP, 0)];
    assert!(create_array(devices.as_slice(), &specs, &spec()).is_err());

    for device in devices.as_slice() {
        let mut head = [0xFFu8; 64];
        device.read_at(0, &mut head).unwrap();
        assert!(head.iter().all(|&byte| byte == 0), "Geraet unberuehrt");
    }
}

#[test]
fn two_members_with_the_same_uuid_are_refused() {
    // Regel 3: dieselbe member_uuid heisst dieselbe Platte, etwa nach einem
    // `dd`. Ein Array daraus rechnete Paritaet ueber eine Kopie ihrer selbst.
    let devices = Devices::new("doppelte-uuid", &[device_size(8); 3]);
    let specs = [
        member(0, Role::Data, 0),
        member(0, Role::Data, 1),
        member(2, Role::ParityP, 0),
    ];
    assert!(create_array(devices.as_slice(), &specs, &spec()).is_err());
}

#[test]
fn a_missing_slot_is_refused() {
    // Regel 5: die slot_index muessen 0..data_slot_count genau einmal decken.
    let devices = Devices::new("luecke", &[device_size(8); 3]);
    let specs = [
        member(0, Role::Data, 0),
        member(1, Role::Data, 2),
        member(2, Role::ParityP, 0),
    ];
    assert!(create_array(devices.as_slice(), &specs, &spec()).is_err());
}

#[test]
fn a_read_only_device_stops_the_creation() {
    let devices = Devices::new("nur-lesend", &[device_size(8), device_size(8)]);
    let read_only = vec![
        MemberDevice::open_read_only(&devices.paths[0]).unwrap(),
        MemberDevice::open(&devices.paths[1]).unwrap(),
    ];
    let specs = [member(0, Role::Data, 0), member(1, Role::ParityP, 0)];

    assert_eq!(
        create_array(&read_only, &specs, &spec()),
        Err(EngineError::NotWritable)
    );
}

#[test]
fn one_device_without_a_superblock_stops_the_opening() {
    // Eine fremde oder frisch getauschte Platte im Satz. Das Array darf nicht
    // ohne sie zusammengesetzt werden — sonst rechnete es Paritaet ueber eine
    // unvollstaendige Menge.
    let devices = Devices::new("fremde-platte", &[device_size(8); 3]);
    let specs = [
        member(0, Role::Data, 0),
        member(1, Role::Data, 1),
        member(2, Role::ParityP, 0),
    ];
    create_array(devices.as_slice(), &specs, &spec()).unwrap();

    // Beide Superbloecke des dritten Geraets ueberschreiben.
    let victim = &devices.as_slice()[2];
    victim.write_at(65_536, &[0u8; 4096]).unwrap();
    victim
        .write_at(victim.size() - SUPERBLOCK_BACKUP_FROM_END, &[0u8; 4096])
        .unwrap();
    victim.flush().unwrap();

    assert!(matches!(
        open_array(&devices.reopen()),
        Err(EngineError::Format(FormatError::BadMagic { .. }))
    ));
}

#[test]
fn an_array_with_parity_q_opens_with_both_parity_members() {
    let devices = Devices::new("mit-q", &[device_size(8); 4]);
    let specs = [
        member(0, Role::Data, 0),
        member(1, Role::Data, 1),
        member(2, Role::ParityP, 0),
        member(3, Role::ParityQ, 0),
    ];
    create_array(devices.as_slice(), &specs, &spec()).unwrap();

    let array = open_array(&devices.reopen()).unwrap();
    assert_eq!(array.parity_p().role, Role::ParityP);
    assert_eq!(array.parity_q().unwrap().role, Role::ParityQ);
    assert!(array.log().is_none());
}

#[test]
fn the_number_of_devices_and_roles_must_match() {
    let devices = Devices::new("anzahl", &[device_size(8); 2]);
    let specs = [member(0, Role::Data, 0)];
    assert!(matches!(
        create_array(devices.as_slice(), &specs, &spec()),
        Err(EngineError::Format(FormatError::InvalidField {
            field: "members",
            ..
        }))
    ));
}

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Jeremy Gubler

//! Dasselbe wie in `device.rs`, aber auf echten Blockgeraeten.
//!
//! Loop-Geraete sind kein Mock: Die Blockschicht des Kernels ist echt, und
//! genau die Unterschiede, die eine Datei nicht zeigt — Groesse ueber `seek`
//! statt ueber Metadaten, Sektorausrichtung, `sync_data` auf ein Blockgeraet —
//! stehen hier auf dem Pruefstand.
//!
//! **Diese Tests brauchen Linux und Root** und sind deshalb `#[ignore]`.
//! Ausfuehren mit:
//!
//! ```text
//! sudo -E cargo test -p ferrite-engine --test loop_device -- --ignored
//! ```
//!
//! Fehlt die Voraussetzung, sagen sie das und laufen nicht auf einer Attrappe
//! weiter.

#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};
use std::process::Command;

use ferrite_engine::{
    create_array, open_array, read_superblock, write_superblock, ArraySpec, MemberDevice,
    MemberSpec,
};
use ferrite_format::superblock::{
    Role, Superblock, DEFAULT_PAYLOAD_OFFSET, SUPERBLOCK_BACKUP_FROM_END,
};
use ferrite_format::Uuid;

const PAYLOAD_BLOCKS: u64 = 8;
const BLOCK: u64 = 64 * 1024;
const DEVICE_SIZE: u64 = 1_048_576 + PAYLOAD_BLOCKS * BLOCK + 65_536;

/// Ein Loop-Geraet ueber einer Sparse-Datei, das sich selbst wieder abbaut.
struct LoopDevice {
    device: PathBuf,
    backing: PathBuf,
}

impl LoopDevice {
    /// `None`, wenn die Voraussetzungen fehlen — dann wird der Test nicht
    /// stillschweigend auf etwas anderem ausgefuehrt, sondern uebersprungen
    /// und sagt warum.
    fn create(name: &str, size: u64) -> Option<Self> {
        if !is_root() {
            eprintln!("uebersprungen: braucht Root");
            return None;
        }
        let backing = std::env::temp_dir().join(format!("ferrite-loop-{name}.img"));
        let _ = std::fs::remove_file(&backing);
        let file = std::fs::File::create(&backing).ok()?;
        file.set_len(size).ok()?;
        drop(file);

        let output = Command::new("losetup")
            .args(["--show", "--find"])
            .arg(&backing)
            .output()
            .ok()?;
        if !output.status.success() {
            eprintln!(
                "uebersprungen: losetup fehlgeschlagen: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
            let _ = std::fs::remove_file(&backing);
            return None;
        }
        let device = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim().to_string());
        Some(LoopDevice { device, backing })
    }

    fn path(&self) -> &Path {
        &self.device
    }
}

impl Drop for LoopDevice {
    fn drop(&mut self) {
        let _ = Command::new("losetup").arg("-d").arg(&self.device).status();
        let _ = std::fs::remove_file(&self.backing);
    }
}

fn is_root() -> bool {
    // Ohne `libc`: `id -u` fragen. Ein Prozessaufruf pro Test ist nichts
    // gegen den Rest, und das Crate bleibt dependency-frei.
    Command::new("id")
        .arg("-u")
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).trim() == "0")
        .unwrap_or(false)
}

fn sample(role: Role, slot_index: u16) -> Superblock {
    let mut superblock = Superblock::new(
        Uuid::from_random_bytes([0x7A; 16]),
        Uuid::from_random_bytes([0x8B; 16]),
        role,
        4,
        PAYLOAD_BLOCKS * BLOCK,
    );
    superblock.slot_index = slot_index;
    superblock
}

#[test]
#[ignore = "braucht Linux und Root"]
fn the_size_of_a_block_device_comes_from_seeking_to_the_end() {
    // Der eine Punkt, an dem sich ein Blockgeraet anders verhaelt als eine
    // Datei: Seine Metadaten melden Laenge null. Wer `metadata().len()` nimmt,
    // haelt jede Platte fuer leer.
    let Some(loop_device) = LoopDevice::create("size", DEVICE_SIZE) else {
        return;
    };
    let metadata_len = std::fs::metadata(loop_device.path()).unwrap().len();
    let device = MemberDevice::open(loop_device.path()).unwrap();

    assert_eq!(device.size(), DEVICE_SIZE, "ueber seek");
    assert_eq!(metadata_len, 0, "Metadaten melden null — deshalb der seek");
}

#[test]
#[ignore = "braucht Linux und Root"]
fn superblocks_round_trip_over_a_real_block_device() {
    let Some(loop_device) = LoopDevice::create("superblock", DEVICE_SIZE) else {
        return;
    };
    let device = MemberDevice::open(loop_device.path()).unwrap();
    let superblock = sample(Role::Data, 1);

    write_superblock(&device, &superblock).unwrap();
    assert_eq!(read_superblock(&device).unwrap(), superblock);
}

#[test]
#[ignore = "braucht Linux und Root"]
fn the_backup_survives_a_destroyed_primary_on_a_real_device() {
    let Some(loop_device) = LoopDevice::create("torn", DEVICE_SIZE) else {
        return;
    };
    let device = MemberDevice::open(loop_device.path()).unwrap();
    let superblock = sample(Role::ParityP, 0);
    write_superblock(&device, &superblock).unwrap();

    device.write_at(65_536, &[0xFFu8; 4096]).unwrap();
    device.flush().unwrap();

    assert_eq!(read_superblock(&device).unwrap(), superblock);
}

#[test]
#[ignore = "braucht Linux und Root"]
fn data_survives_closing_and_reopening_the_device() {
    // Nach `flush` und Schliessen muss der Inhalt aus dem Geraet kommen und
    // nicht aus einem Puffer, den derselbe Prozess noch haelt.
    let Some(loop_device) = LoopDevice::create("persist", DEVICE_SIZE) else {
        return;
    };
    let pattern: Vec<u8> = (0u8..=255).cycle().take(16 * 1024).collect();

    {
        let device = MemberDevice::open(loop_device.path()).unwrap();
        device.write_at(1_048_576, &pattern).unwrap();
        device.flush().unwrap();
    }

    let device = MemberDevice::open(loop_device.path()).unwrap();
    let mut back = vec![0u8; pattern.len()];
    device.read_at(1_048_576, &mut back).unwrap();
    assert_eq!(back, pattern);
}

#[test]
#[ignore = "braucht Linux und Root"]
fn an_array_of_differently_sized_block_devices_opens_again() {
    // Der Durchstich von Meilenstein 2 auf echten Geraeten: vier verschieden
    // grosse Platten, ein Array darueber, danach neu geoeffnet.
    const BLOCK: u64 = 64 * 1024;
    let size = |blocks: u64| DEFAULT_PAYLOAD_OFFSET + blocks * BLOCK + SUPERBLOCK_BACKUP_FROM_END;

    let names = ["a", "b", "c", "p"];
    let blocks = [4u64, 9, 6, 9];
    let mut loops = Vec::new();
    for (name, blocks) in names.iter().zip(blocks) {
        let Some(loop_device) = LoopDevice::create(name, size(blocks)) else {
            return;
        };
        loops.push(loop_device);
    }

    let devices: Vec<MemberDevice> = loops
        .iter()
        .map(|loop_device| MemberDevice::open(loop_device.path()).unwrap())
        .collect();
    let specs = [
        MemberSpec {
            member_uuid: Uuid::from_random_bytes([1; 16]),
            role: Role::Data,
            slot_index: 0,
            label: "a".to_string(),
        },
        MemberSpec {
            member_uuid: Uuid::from_random_bytes([2; 16]),
            role: Role::Data,
            slot_index: 1,
            label: "b".to_string(),
        },
        MemberSpec {
            member_uuid: Uuid::from_random_bytes([3; 16]),
            role: Role::Data,
            slot_index: 2,
            label: "c".to_string(),
        },
        MemberSpec {
            member_uuid: Uuid::from_random_bytes([4; 16]),
            role: Role::ParityP,
            slot_index: 0,
            label: "p".to_string(),
        },
    ];
    let array_spec = ArraySpec {
        array_uuid: Uuid::from_random_bytes([0x5C; 16]),
        parity_block_size_log2: 16,
        created_unix: 1_767_225_600,
    };

    let written = create_array(&devices, &specs, &array_spec).unwrap();
    drop(devices);

    let reopened: Vec<MemberDevice> = loops
        .iter()
        .map(|loop_device| MemberDevice::open(loop_device.path()).unwrap())
        .collect();
    let array = open_array(&reopened).unwrap();

    assert_eq!(array.superblocks(), written);
    assert_eq!(array.data_member(0).unwrap().payload_size, 4 * BLOCK);
    assert_eq!(array.data_member(1).unwrap().payload_size, 9 * BLOCK);
    assert_eq!(array.data_member(2).unwrap().payload_size, 6 * BLOCK);
    assert_eq!(array.parity_p().payload_size, 9 * BLOCK);
}

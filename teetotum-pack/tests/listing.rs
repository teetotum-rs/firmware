//! The plugin list entry a sender reads over BLE, run on the host like `modules.rs`.

use teetotum_pack::PluginId;
use teetotum_pack::listing::{self, ENTRY, Entry, Listed};
use teetotum_pack::manifest::{Manifest, Version};

const NEARBY: &[u8] = include_bytes!("../../firmware/assets/plugins/nearby.wasm");

fn nearby(slot: Option<u8>, installed: bool) -> Entry<'static> {
    let manifest = Manifest::read(NEARBY).unwrap();
    Entry {
        slot,
        installed,
        id: PluginId::of(NEARBY).unwrap(),
        len: NEARBY.len() as u32,
        version: manifest.version(),
        name: manifest.name(),
        summary: manifest.summary(),
    }
}

/// The layout senders parse, assembled byte by byte rather than through `encode`.
#[test]
fn an_entry_lays_out_its_bytes_as_documented() {
    let entry = Entry {
        slot: Some(5),
        installed: true,
        id: PluginId::from_bytes([1, 2, 3, 4, 5, 6, 7, 8]),
        len: 0x0001_2345,
        version: Version {
            major: 1,
            minor: 0x0203,
            patch: 4,
        },
        name: "Dial",
        summary: "a knob",
    };
    let mut want = [0u8; ENTRY];
    want[..4].copy_from_slice(&[2, 7, 0x02, 5]);
    want[4..12].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
    want[12..16].copy_from_slice(&[0x45, 0x23, 0x01, 0x00]);
    want[16..22].copy_from_slice(&[1, 0, 0x03, 0x02, 4, 0]);
    want[22] = 4;
    want[23..27].copy_from_slice(b"Dial");
    want[43] = 6;
    want[44..50].copy_from_slice(b"a knob");
    assert_eq!(entry.encode(2, 7), want);
}

#[test]
fn a_bundled_plugin_has_no_slot() {
    let raw = nearby(None, false).encode(0, 3);
    assert_eq!(raw[2], listing::BUNDLED);
    assert_eq!(raw[3], listing::NO_SLOT);
}

#[test]
fn entries_round_trip() {
    for (slot, installed) in [
        (None, true),
        (None, false),
        (Some(0), true),
        (Some(15), false),
    ] {
        let entry = nearby(slot, installed);
        let raw = entry.encode(1, 4);
        assert_eq!(
            Entry::decode(&raw),
            Some(Listed {
                index: 1,
                count: 4,
                entry: Some(entry),
            })
        );
    }
}

#[test]
fn an_index_past_the_list_carries_only_index_and_count() {
    let raw = listing::past_end(3, 3);
    assert_eq!(raw[..2], [3, 3]);
    assert!(raw[2..].iter().all(|b| *b == 0));
    assert_eq!(
        Entry::decode(&raw),
        Some(Listed {
            index: 3,
            count: 3,
            entry: None,
        })
    );
    let mut dirty = raw;
    dirty[40] = 1;
    assert_eq!(Entry::decode(&dirty), None);
}

#[test]
fn a_long_name_is_cut_at_a_character() {
    let mut entry = nearby(Some(1), true);
    // 19 ASCII bytes and a two-byte character: the character does not fit into 20.
    entry.name = "abcdefghijklmnopqrsä";
    entry.summary = "";
    let raw = entry.encode(0, 1);
    assert_eq!(raw[22], 19);
    let read = Entry::decode(&raw).unwrap().entry.unwrap();
    assert_eq!(read.name, "abcdefghijklmnopqrs");
    assert_eq!(read.summary, "");
}

#[test]
fn inconsistent_entries_are_refused() {
    let raw = nearby(Some(2), true).encode(0, 2);
    let mut bundled_with_slot = raw;
    bundled_with_slot[2] |= listing::BUNDLED;
    assert_eq!(Entry::decode(&bundled_with_slot), None);
    let mut long_name = raw;
    long_name[22] = 21;
    assert_eq!(Entry::decode(&long_name), None);
    let mut unknown_flag = raw;
    unknown_flag[2] |= 0x80;
    assert_eq!(Entry::decode(&unknown_flag), None);
}

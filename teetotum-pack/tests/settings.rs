//! The settings a sender reads and writes over BLE.

use teetotum_pack::settings::{self, LEN, Settings};

const SETTINGS: Settings = Settings {
    theme: 6,
    brightness: 10,
    haptics: 9,
    orientation: 1,
};

#[test]
fn settings_come_back_as_encoded() {
    let raw = SETTINGS.encode();
    assert_eq!(raw, [settings::FORMAT, 6, 10, 9, 1]);
    assert_eq!(Settings::decode(&raw), Some(SETTINGS));
}

#[test]
fn the_ends_of_every_range_are_taken() {
    for raw in [
        [settings::FORMAT, 0, settings::BRIGHTNESS_MIN, 0, 0],
        [
            settings::FORMAT,
            settings::THEMES - 1,
            settings::BRIGHTNESS_MAX,
            settings::HAPTICS_MAX,
            settings::ORIENTATIONS - 1,
        ],
    ] {
        assert!(Settings::decode(&raw).is_some(), "{raw:?}");
    }
}

#[test]
fn a_step_past_its_range_is_refused() {
    let good = SETTINGS.encode();
    for (at, value) in [
        (1, settings::THEMES),
        (2, settings::BRIGHTNESS_MIN - 1),
        (2, settings::BRIGHTNESS_MAX + 1),
        (3, settings::HAPTICS_MAX + 1),
        (4, settings::ORIENTATIONS),
    ] {
        let mut raw = good;
        raw[at] = value;
        assert_eq!(Settings::decode(&raw), None, "byte {at} = {value}");
    }
}

#[test]
fn another_format_or_length_is_refused() {
    let mut raw = SETTINGS.encode();
    raw[0] = settings::FORMAT + 1;
    assert_eq!(Settings::decode(&raw), None);
    assert_eq!(Settings::decode(&SETTINGS.encode()[..LEN - 1]), None);
    assert_eq!(
        Settings::decode(&[SETTINGS.encode().as_slice(), &[0]].concat()),
        None
    );
}

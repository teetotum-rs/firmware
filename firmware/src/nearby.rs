//! What the radio hears nearby, kept for a face with `Rights::RADIO`.
//!
//! The Wi-Fi scans and the Bluetooth windows run whether or not a face listens, and publish
//! what each round heard here; a face reads it through `teetotum_face::nearby`, as records in
//! the layout `abi::SIGNAL_BYTES` describes. While such a face is on the glass, the main loop
//! says so with [`set_wanted`], and the two loops come round as fast as they go.
//!
//! **No address leaves this module.** A record carries a key in its place, made from the
//! address and [`set_salt`]'s number: the same device keeps its key while the firmware runs,
//! which is what a face needs to follow one, but the key cannot be looked up in a list of
//! addresses, and a restart gives every device a new one. The addresses themselves stay in the
//! loops that heard them -- where they are logged, and nowhere else.

use core::cell::RefCell;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use critical_section::Mutex;
use teetotum_face::{Radio, Signal, abi};

/// The most networks one Wi-Fi round keeps: what the scan is asked for at most.
const WIFI_MAX: usize = 20;
/// The most devices one Bluetooth round keeps: what one window remembers at most.
const BLUETOOTH_MAX: usize = 40;

type Record = [u8; abi::SIGNAL_BYTES];

/// One thing heard, as a loop hands it in: with its address, which goes no further than here.
pub struct Heard<'a> {
    pub address: [u8; 6],
    pub strength: i8,
    /// The Wi-Fi channel, 0 for Bluetooth.
    pub channel: u8,
    /// `""` for none.
    pub name: &'a str,
}

struct Lists {
    wifi: [Record; WIFI_MAX],
    wifi_len: usize,
    bluetooth: [Record; BLUETOOTH_MAX],
    bluetooth_len: usize,
}

impl Lists {
    fn of(&mut self, radio: Radio) -> (&mut [Record], &mut usize) {
        match radio {
            Radio::Wifi => (&mut self.wifi, &mut self.wifi_len),
            Radio::Bluetooth => (&mut self.bluetooth, &mut self.bluetooth_len),
        }
    }
}

static LISTS: Mutex<RefCell<Lists>> = Mutex::new(RefCell::new(Lists {
    wifi: [[0; abi::SIGNAL_BYTES]; WIFI_MAX],
    wifi_len: 0,
    bluetooth: [[0; abi::SIGNAL_BYTES]; BLUETOOTH_MAX],
    bluetooth_len: 0,
}));

/// Counted up by every round, of either radio: the main loop hands a face a new round when this
/// has moved.
static ROUND: AtomicU32 = AtomicU32::new(0);
static WANTED: AtomicBool = AtomicBool::new(false);
static SALT: AtomicU32 = AtomicU32::new(0);

/// Sets the number the keys are made with. Once, at boot, before the first round.
pub fn set_salt(salt: u32) {
    SALT.store(salt, Ordering::Relaxed);
}

/// Whether a face that listens is on the glass.
pub fn set_wanted(wanted: bool) {
    WANTED.store(wanted, Ordering::Relaxed);
}

pub fn wanted() -> bool {
    WANTED.load(Ordering::Relaxed)
}

/// How many rounds have come in since boot.
pub fn round() -> u32 {
    ROUND.load(Ordering::Relaxed)
}

/// Puts one round in place of the last round of the same radio, in the order given -- the loops
/// hand it over strongest first. Past the list's length the rest is left out. Answers how many
/// were kept.
pub fn publish<'a>(radio: Radio, heard: impl IntoIterator<Item = Heard<'a>>) -> usize {
    let salt = SALT.load(Ordering::Relaxed);
    let kept = critical_section::with(|cs| {
        let mut lists = LISTS.borrow_ref_mut(cs);
        let (records, len) = lists.of(radio);
        let mut n = 0;
        for heard in heard.into_iter().take(records.len()) {
            records[n] = Signal::record(
                key(salt, &heard.address),
                heard.strength,
                heard.channel,
                heard.name,
            );
            n += 1;
        }
        *len = n;
        n
    });
    ROUND.fetch_add(1, Ordering::Relaxed);
    kept
}

/// Copies the last round of `radio` into `out`, as many whole records as fit, and answers how
/// many that was.
pub fn copy(radio: Radio, out: &mut [u8]) -> usize {
    critical_section::with(|cs| {
        let mut lists = LISTS.borrow_ref_mut(cs);
        let (records, len) = lists.of(radio);
        let n = (*len).min(out.len() / abi::SIGNAL_BYTES);
        for (chunk, record) in out.chunks_exact_mut(abi::SIGNAL_BYTES).zip(&records[..n]) {
            chunk.copy_from_slice(record);
        }
        n
    })
}

/// FNV-1a over the salt and the address. Not a secret kept from someone who holds both -- only
/// a number that is not the address, and stays the same for the same one.
fn key(salt: u32, address: &[u8; 6]) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in salt.to_le_bytes().iter().chain(address) {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

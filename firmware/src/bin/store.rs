//! Does a setting survive a reboot, and does it survive a broken sector?
//!
//! `teetotum::store` writes settings into the `nvs` partition as one record in each of two
//! sectors, alternating, so that the older one is still whole while the newer one is being
//! erased and rewritten. Both halves of that claim are about what happens *between* two boots,
//! and neither can be shown inside one: a value that is still in RAM proves nothing about
//! flash, and a sector that was never erased proves nothing about the pair.
//!
//! So this run drives itself across four boots and resets itself between them. **It needs no
//! hand at all** -- which is rare enough here to be worth saying, and it is only true because
//! the thing under test is also the thing that remembers how far the run has got. The phase
//! number lives in the payload:
//!
//! 1. **Nothing in flash.** Write phase 1 with a pattern, reset.
//! 2. **Phase 1 comes back.** Check the pattern byte for byte -- a record that survived a reboot
//!    but came back altered is worse than one that vanished. Write phase 2, reset.
//! 3. **Phase 2 comes back, and out of the other sector.** That is the alternation: had the save
//!    gone back into the sector it was read from, the slot would be the same. Now damage it --
//!    four zero bytes over the checksum of the *newer* record, which a NOR flash accepts without
//!    an erase because zeroing bits is all a write ever does -- and load again. **Phase 1 must
//!    come back**, pattern and all. That is the power cut, staged: the newest record is gone and
//!    the settings are not. Write phase 3, reset.
//! 4. **Phase 3 comes back**, so the store kept working after the damage. Report, erase both
//!    sectors, and stop -- a clean partition, and the next reset runs the whole thing again.
//!
//! The pattern changes with the phase, so a stale record cannot pass for a fresh one: reading
//! phase 2's pattern where phase 3's was expected fails the check rather than passing it
//! quietly.
//!
//! Each boot waits a moment before it says anything. The run is meant to be read from a plain
//! `cat /dev/ttyACM0` -- it wants no terminal and no keyboard -- and that gives the reader time
//! to attach after the flash tool lets go of the port.

#![no_std]
#![no_main]

use embedded_storage::nor_flash::NorFlash;
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use esp_hal::system::software_reset;
use esp_storage::FlashStorage;
use log::{error, info};
use teetotum::store::{self, HEADER, Store};
use teetotum_firmware::flash::{self, TABLE_SCRATCH};

esp_bootloader_esp_idf::esp_app_desc!();

/// How many bytes of pattern ride along with the phase number.
const PATTERN: usize = 64;

/// Payload: the phase in byte 0, then a pattern that depends on it.
fn payload(phase: u8, buf: &mut [u8; 1 + PATTERN]) {
    buf[0] = phase;
    for (i, b) in buf[1..].iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(37) ^ phase;
    }
}

/// Does what came back match what phase `phase` would have written?
fn matches(phase: u8, got: &[u8]) -> bool {
    let mut want = [0u8; 1 + PATTERN];
    payload(phase, &mut want);
    got.len() == want.len() && got == want
}

#[esp_hal::main]
fn main() -> ! {
    esp_println::logger::init_logger_from_env();
    // Reading the partition table needs a heap: `esp-bootloader-esp-idf`'s partition types go
    // through `enumset`, and that crate is not `alloc`-free. Nothing else here allocates, so a
    // few kilobytes are plenty.
    esp_alloc::heap_allocator!(size: 8 * 1024);
    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
    let delay = Delay::new();

    // Long enough for a reader to attach to the port after the flash tool releases it.
    delay.delay_millis(1500);

    let mut flash = FlashStorage::new(peripherals.FLASH);
    let mut table = [0u8; TABLE_SCRATCH];
    let mut buf = [0u8; store::MAX_PAYLOAD];
    let mut record = [0u8; 1 + PATTERN];

    info!("--- store: settings across a reboot ---");

    let (phase, slot, seq, len) = {
        let region = match flash::nvs(&mut flash, &mut table) {
            Ok(r) => r,
            Err(e) => {
                error!("no nvs partition: {e:?}");
                loop {}
            }
        };
        let mut store = match Store::new(region) {
            Ok(s) => s,
            Err(e) => {
                error!("store refused the partition: {e:?}");
                loop {}
            }
        };
        match store.load(&mut buf) {
            Ok(Some(len)) => (buf[0], store.slot(), store.sequence(), len),
            Ok(None) => (0, None, 0, 0),
            Err(e) => {
                error!("load failed: {e:?}");
                loop {}
            }
        }
    };
    let payload_back = &buf[..len];

    match phase {
        0 => {
            info!("boot 1: nothing in flash, as expected on a fresh partition");
            payload(1, &mut record);
            save(&mut flash, &mut table, &record, 1);
            info!("boot 1: wrote phase 1, resetting");
            delay.delay_millis(200);
            software_reset();
        }

        1 => {
            info!("boot 2: phase 1 came back from slot {slot:?}, sequence {seq}");
            check(1, payload_back);
            payload(2, &mut record);
            save(&mut flash, &mut table, &record, 2);
            info!("boot 2: wrote phase 2, resetting");
            delay.delay_millis(200);
            software_reset();
        }

        2 => {
            info!("boot 3: phase 2 came back from slot {slot:?}, sequence {seq}");
            check(2, payload_back);

            let Some(newest) = slot else {
                error!("boot 3: no slot reported, cannot stage the damage");
                loop {}
            };
            if newest == 0 {
                error!("boot 3: phase 2 came out of the same slot as phase 1 -- no alternation");
            } else {
                info!("boot 3: alternation holds, phase 1 was slot 0 and phase 2 is slot 1");
            }

            // Zero the four checksum bytes of the newer record. A NOR flash write only ever
            // clears bits, so this needs no erase and leaves the other sector untouched.
            {
                let mut region = match flash::nvs(&mut flash, &mut table) {
                    Ok(r) => r,
                    Err(e) => {
                        error!("boot 3: lost the partition: {e:?}");
                        loop {}
                    }
                };
                let at = (newest * 4096 + 12) as u32;
                if let Err(e) = region.write(at, &[0u8; 4]) {
                    error!("boot 3: could not damage slot {newest}: {e:?}");
                    loop {}
                }
                info!("boot 3: zeroed the checksum of slot {newest} at 0x{at:x}");
            }

            {
                let region = flash::nvs(&mut flash, &mut table).unwrap();
                let mut store = Store::new(region).unwrap();
                match store.load(&mut buf) {
                    Ok(Some(len)) if buf[0] == 1 && matches(1, &buf[..len]) => {
                        info!(
                            "boot 3: phase 1 came back from slot {:?} -- the pair held",
                            store.slot()
                        );
                    }
                    Ok(Some(len)) => {
                        error!(
                            "boot 3: expected phase 1 after the damage, got phase {} ({len} bytes)",
                            buf[0]
                        );
                    }
                    Ok(None) => error!("boot 3: both sectors gone after damaging one"),
                    Err(e) => error!("boot 3: load failed after the damage: {e:?}"),
                }
            }

            payload(3, &mut record);
            save(&mut flash, &mut table, &record, 3);
            info!("boot 3: wrote phase 3, resetting");
            delay.delay_millis(200);
            software_reset();
        }

        3 => {
            info!("boot 4: phase 3 came back from slot {slot:?}, sequence {seq}");
            check(3, payload_back);
            info!("--- all four boots done: settings survive a reboot and a broken sector ---");

            let mut region = flash::nvs(&mut flash, &mut table).unwrap();
            let two = (2 * 4096) as u32;
            match region.erase(0, two) {
                Ok(()) => info!("both sectors erased -- the next reset runs the whole thing again"),
                Err(e) => error!("could not erase: {e:?}"),
            }
            loop {}
        }

        other => {
            error!("unknown phase {other} in flash; erasing and starting over");
            let mut region = flash::nvs(&mut flash, &mut table).unwrap();
            let _ = region.erase(0, (2 * 4096) as u32);
            delay.delay_millis(200);
            software_reset();
        }
    }
}

/// Write one record, reporting where it went.
fn save(flash: &mut FlashStorage<'_>, table: &mut [u8; TABLE_SCRATCH], record: &[u8], phase: u8) {
    let region = match flash::nvs(flash, table) {
        Ok(r) => r,
        Err(e) => {
            error!("save: lost the partition: {e:?}");
            return;
        }
    };
    let mut store = match Store::new(region) {
        Ok(s) => s,
        Err(e) => {
            error!("save: store refused the partition: {e:?}");
            return;
        }
    };
    // Load first, so the save knows which sector to avoid and what sequence to follow.
    let mut scratch = [0u8; store::MAX_PAYLOAD];
    let _ = store.load(&mut scratch);
    match store.save(record) {
        Ok(()) => info!(
            "save: phase {phase} went to slot {:?} as sequence {}, {} bytes plus {HEADER} of header",
            store.slot(),
            store.sequence(),
            record.len()
        ),
        Err(e) => error!("save: phase {phase} failed: {e:?}"),
    }
}

/// Compare a loaded payload against what the phase should have written.
fn check(phase: u8, got: &[u8]) {
    if matches(phase, got) {
        info!("  pattern intact, all {} bytes", got.len());
    } else {
        error!("  pattern differs from what phase {phase} wrote");
    }
}

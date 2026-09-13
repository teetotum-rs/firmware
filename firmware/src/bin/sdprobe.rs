//! Where is the TF card wired? Ask the card.
//!
//! The factory image drives the card with the SDMMC host, and on an ESP32-S3 that host runs
//! through the GPIO matrix, so the pin numbers exist only as immediates inside the factory code.
//! Nothing in any pin list names them, and the card sits inside the closed case, so it cannot be
//! pulled out to compare a board with and without it.
//!
//! What makes this answerable is that every SD card also speaks **SPI mode** on the same six
//! lines: CS is DAT3, MOSI is CMD, MISO is DAT0, SCK is CLK. A card in SPI mode answers CMD0
//! with exactly `0x01` -- idle, no error -- and answers nothing at all if the wires are wrong.
//! So a bit-banged CMD0 over each plausible assignment turns the question into a search, and a
//! single `0x01` names four pins at once.
//!
//! The candidates come from `pullscan`:
//!
//! - CMD and DAT0-DAT3 carry external pull-ups on every board that works, so CS, MOSI and MISO
//!   are looked for among the pins that scan found held high externally and that nothing else on
//!   this board has claimed: GPIO1, 2, 3, 5, 6, 39, 42.
//! - CLK is not pulled up, so it is looked for among the pins that came out floating and are
//!   free: GPIO4, 40, 41, 48. GPIO38 is the haptic enable, GPIO43 and 44 are the UART, and
//!   GPIO45 and 46 are strapping pins -- all left out.
//!
//! That is 4 x 7 x 6 x 5 = 840 assignments, about a second of clocking in total.
//!
//! One risk worth naming: this drives pins whose purpose is unknown. The pull scan reported
//! every candidate as held by a resistor rather than driven by a chip -- a driven line would have
//! changed between reads and been reported as "changing" -- so a short push-pull pulse on them is
//! a contest with a pull-up and not with another output stage. The clocking is slow (125 kHz) and
//! each candidate is released back to a floating input the moment its combination is done.

#![no_std]
#![no_main]

use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use esp_hal::gpio::{Flex, InputConfig, OutputConfig, Pull};
use log::info;

esp_bootloader_esp_idf::esp_app_desc!();

/// Half a clock period. 4 us makes 125 kHz, comfortably inside the 400 kHz an SD card must accept
/// before it is initialised.
const HALF_CLOCK_US: u32 = 4;

/// Pins that `pullscan` found held high externally and that nothing else claims -- the candidates
/// for CS, MOSI and MISO. Indices into `PINS`.
const PULLED: [usize; 7] = [0, 1, 2, 3, 4, 5, 6];
/// Pins that came out floating and are free -- the candidates for CLK.
const FLOATING: [usize; 4] = [7, 8, 9, 10];

/// GPIO numbers behind those indices, for the log.
const NUMBERS: [u8; 11] = [1, 2, 3, 5, 6, 39, 42, 4, 40, 41, 48];

type Pins<'d> = [Flex<'d>; 11];

/// Clock one byte out and one byte in, SPI mode 0: MOSI settles while the clock is low, MISO is
/// sampled on the rising edge.
fn transfer(pins: &mut Pins<'_>, clk: usize, mosi: usize, miso: usize, out: u8, delay: &Delay) -> u8 {
    let mut input = 0u8;
    for bit in (0..8).rev() {
        if out & (1 << bit) != 0 {
            pins[mosi].set_high();
        } else {
            pins[mosi].set_low();
        }
        pins[clk].set_low();
        delay.delay_micros(HALF_CLOCK_US);
        pins[clk].set_high();
        delay.delay_micros(HALF_CLOCK_US);
        if pins[miso].is_high() {
            input |= 1 << bit;
        }
    }
    input
}

/// Send one command and wait for its R1 response, which is the first byte with the top bit clear.
/// Returns `0xFF` if the card never answered.
fn command(
    pins: &mut Pins<'_>,
    clk: usize,
    mosi: usize,
    miso: usize,
    index: u8,
    argument: u32,
    crc: u8,
    delay: &Delay,
) -> u8 {
    transfer(pins, clk, mosi, miso, 0xFF, delay);
    transfer(pins, clk, mosi, miso, 0x40 | index, delay);
    for shift in [24, 16, 8, 0] {
        transfer(pins, clk, mosi, miso, (argument >> shift) as u8, delay);
    }
    transfer(pins, clk, mosi, miso, crc, delay);

    for _ in 0..10 {
        let response = transfer(pins, clk, mosi, miso, 0xFF, delay);
        if response & 0x80 == 0 {
            return response;
        }
    }
    0xFF
}

#[esp_hal::main]
fn main() -> ! {
    esp_println::logger::init_logger_from_env();

    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
    let delay = Delay::new();

    let mut pins: Pins<'_> = [
        Flex::new(peripherals.GPIO1),
        Flex::new(peripherals.GPIO2),
        Flex::new(peripherals.GPIO3),
        Flex::new(peripherals.GPIO5),
        Flex::new(peripherals.GPIO6),
        Flex::new(peripherals.GPIO39),
        Flex::new(peripherals.GPIO42),
        Flex::new(peripherals.GPIO4),
        Flex::new(peripherals.GPIO40),
        Flex::new(peripherals.GPIO41),
        Flex::new(peripherals.GPIO48),
    ];

    let idle = InputConfig::default().with_pull(Pull::None);
    let listening = InputConfig::default().with_pull(Pull::Up);
    let driving = OutputConfig::default();

    info!("SD: asking 840 pin assignments for a CMD0 answer");

    let mut found = 0u32;
    for &clk in FLOATING.iter() {
        for &cs in PULLED.iter() {
            for &mosi in PULLED.iter() {
                if mosi == cs {
                    continue;
                }
                for &miso in PULLED.iter() {
                    if miso == cs || miso == mosi {
                        continue;
                    }

                    // Everything back to a floating input, then only the four lines of this
                    // attempt take on a role.
                    for pin in pins.iter_mut() {
                        pin.set_output_enable(false);
                        pin.apply_input_config(&idle);
                        pin.set_input_enable(true);
                    }
                    for &driven in [clk, cs, mosi].iter() {
                        pins[driven].apply_output_config(&driving);
                        pins[driven].set_high();
                        pins[driven].set_output_enable(true);
                    }
                    pins[miso].apply_input_config(&listening);

                    // 80 clocks with CS high are what puts a card into SPI mode at all.
                    for _ in 0..10 {
                        transfer(&mut pins, clk, mosi, miso, 0xFF, &delay);
                    }

                    pins[cs].set_low();
                    let r1 = command(&mut pins, clk, mosi, miso, 0, 0, 0x95, &delay);
                    if r1 == 0x01 {
                        // CMD8 separates a v2 card from a stuck line that happens to read 0x01:
                        // a real card echoes the check pattern back in the last of four bytes.
                        let r8 = command(&mut pins, clk, mosi, miso, 8, 0x1AA, 0x87, &delay);
                        let mut trailer = [0u8; 4];
                        for byte in trailer.iter_mut() {
                            *byte = transfer(&mut pins, clk, mosi, miso, 0xFF, &delay);
                        }
                        info!(
                            "SD: CLK GPIO{} CS GPIO{} MOSI GPIO{} MISO GPIO{} -- CMD0 {r1:#04x}, \
                             CMD8 {r8:#04x} {:02x} {:02x} {:02x} {:02x}",
                            NUMBERS[clk],
                            NUMBERS[cs],
                            NUMBERS[mosi],
                            NUMBERS[miso],
                            trailer[0],
                            trailer[1],
                            trailer[2],
                            trailer[3],
                        );
                        found += 1;
                    }
                    pins[cs].set_high();
                }
            }
        }
    }

    for pin in pins.iter_mut() {
        pin.set_output_enable(false);
        pin.apply_input_config(&idle);
    }
    info!("SD: done, {found} assignments answered");

    loop {
        delay.delay_millis(1000);
    }
}

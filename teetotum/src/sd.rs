//! The TF card, in SPI mode.
//!
//! The card sits on GPIO4 (CLK), GPIO3 (CMD/MOSI), GPIO5 (DAT0/MISO) and GPIO2 (DAT3/CS),
//! measured by asking the card itself: `src/bin/sdprobe.rs` bit-banged CMD0 over
//! 840 pin assignments and exactly one answered. DAT1 and DAT2 are still unknown, and SPI mode
//! never uses them.
//!
//! The factory firmware drives the same card over the **SDMMC host** in 4-bit mode, which is
//! four times as wide and needs no CRC in software. esp-hal 1.1.2 has no SDMMC driver, so SPI
//! mode is the way in for now -- slower, but it is the same card and the same filesystem, and
//! every SD card is required to speak it.
//!
//! Two details of the protocol are worth knowing before reading the code:
//!
//! - **The card boots in a mode it has to be talked out of.** It comes up expecting SD bus
//!   traffic and only latches into SPI mode if CS is low when it sees CMD0. Before that it needs
//!   at least 74 clock edges with CS *high* to finish its own power-up, and all of it has to
//!   happen slowly -- 400 kHz at the outside. Once initialised it will take tens of megahertz.
//! - **Only two commands need a correct CRC**: CMD0, because the card is not yet in SPI mode and
//!   still checks, and CMD8, because the specification says so. Every other command is sent with
//!   a placeholder. [`crc7`] is here anyway; it is eight lines and removes a whole class of
//!   confusing failure.
//!
//! GPIO3 is a strapping pin on the ESP32-S3 (JTAG source select). It is only sampled at reset,
//! and driving it afterwards is what the factory firmware does too; the board has booted
//! hundreds of times with this driver attached.

use esp_hal::Blocking;
use esp_hal::delay::Delay;
use esp_hal::gpio::Output;
use esp_hal::spi::master::{Config as SpiConfig, Spi};
use esp_hal::time::Rate;

/// How fast the bus may run until the card is initialised. The specification allows 100-400 kHz
/// in this window and nothing above it.
pub const INIT_RATE: Rate = Rate::from_khz(400);
/// Where the bus is taken once the card has answered: **25 MHz, which is what the SD
/// specification allows in SPI mode**, and it is the specification that decides here and not
/// the measurement.
///
/// Measured with `src/bin/sdcard.rs`, 256 KiB read at each rate and checksummed:
/// 20 MHz gave 1615 KiB/s, 25 MHz 1950, and **40 MHz gave 2444 with the same checksum as the
/// other two**, twice over, across two boots. So this card reads perfectly at nearly twice its
/// rated clock -- and that is exactly the reason not to ship it. The panel runs at 80 MHz
/// because the panel is soldered to this board and can be measured once and for all; **the card
/// is the one part of this device the user swaps**, and the next one in the slot has not been
/// measured by anybody.
pub const FAST_RATE: Rate = Rate::from_mhz(25);

/// How many bytes of polling a command response is given before the card counts as absent.
const RESPONSE_TRIES: usize = 16;
/// How long to wait for a data token, in polls of one byte each.
const TOKEN_TRIES: usize = 4096;
/// How long the initialisation handshake may take, in milliseconds. The specification allows a
/// card up to a second to finish it.
const INIT_TIMEOUT_MS: u32 = 1500;
/// How long a card may hold the data line low after a transfer before it counts as stuck.
const BUSY_TIMEOUT_MS: u32 = 500;

/// The token that precedes a block of data the card sends.
const TOKEN_START_BLOCK: u8 = 0xFE;

/// What kind of card answered, and therefore how it wants to be addressed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Version 1, or a version 2 card of standard capacity: arguments are **byte** offsets.
    StandardCapacity,
    /// Version 2 high capacity (SDHC/SDXC): arguments are **block** numbers. Anything above
    /// 2 GB is one of these.
    HighCapacity,
}

/// Why a card operation did not finish.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// Nothing answered CMD0 -- no card, or the wrong pins.
    NoCard,
    /// The card answered, but not in a way this driver supports: a CMD8 pattern that came back
    /// changed, or a version 1 card where a version 2 was expected.
    Unsupported,
    /// The card acknowledged but never finished: initialisation ran past its timeout, or a data
    /// token never arrived.
    Timeout,
    /// A command came back with error bits set. The byte is the R1 response as received.
    Response(u8),
    /// The card sent an error token instead of data. The byte is the token.
    DataError(u8),
    /// The SPI peripheral itself refused the transfer.
    Bus(esp_hal::spi::Error),
}

impl From<esp_hal::spi::Error> for Error {
    fn from(error: esp_hal::spi::Error) -> Self {
        Error::Bus(error)
    }
}

/// The seven-bit CRC an SD command carries in its last byte, shifted up and terminated with the
/// end bit, which is how it goes on the wire.
///
/// Verified against the two constants every SD implementation contains: `0x95` for CMD0 with a
/// zero argument, `0x87` for CMD8 with `0x1AA`.
pub fn crc7(data: &[u8]) -> u8 {
    let mut crc: u8 = 0;
    for &byte in data {
        let mut bits = byte;
        for _ in 0..8 {
            let incoming = bits & 0x80 != 0;
            bits <<= 1;
            let outgoing = crc & 0x40 != 0;
            crc = (crc << 1) & 0x7F;
            if incoming != outgoing {
                crc ^= 0x09;
            }
        }
    }
    (crc << 1) | 1
}

/// An initialised card on a bus of its own.
///
/// The card is the only device on this SPI peripheral, so chip select is a plain output rather
/// than a bus-sharing arrangement; it is held low for a whole command and released after it.
pub struct SdCard<'d> {
    spi: Spi<'d, Blocking>,
    cs: Output<'d>,
    delay: Delay,
    kind: Kind,
}

impl<'d> SdCard<'d> {
    /// Bring a card up.
    ///
    /// The SPI peripheral must arrive configured at [`INIT_RATE`]; this raises it to
    /// [`FAST_RATE`] once the card has finished its handshake.
    pub fn new(spi: Spi<'d, Blocking>, cs: Output<'d>, delay: Delay) -> Result<SdCard<'d>, Error> {
        // `kind` is a placeholder until the handshake says otherwise; nothing reads it before.
        let mut card = SdCard {
            spi,
            cs,
            delay,
            kind: Kind::StandardCapacity,
        };
        card.cs.set_high();
        card.handshake()?;
        card.spi
            .apply_config(&SpiConfig::default().with_frequency(FAST_RATE))
            .map_err(|_| Error::Unsupported)?;
        Ok(card)
    }

    /// Whether the card counts in bytes or in blocks.
    pub fn kind(&self) -> Kind {
        self.kind
    }

    /// The whole power-up conversation, from idle clocks to a card that will read blocks.
    fn handshake(&mut self) -> Result<(), Error> {
        // At least 74 clocks with CS high and the data line high, so the card can finish its own
        // power-up. Ten bytes of 0xFF is eighty.
        self.cs.set_high();
        for _ in 0..10 {
            self.exchange(0xFF)?;
        }

        // CMD0 with CS low is what puts the card into SPI mode. A card that has just been power
        // cycled sometimes needs a second ask.
        let mut idle = false;
        for _ in 0..8 {
            if self.command(0, 0)? == 0x01 {
                idle = true;
                break;
            }
        }
        if !idle {
            return Err(Error::NoCard);
        }

        // CMD8 asks whether the card can run at this supply voltage and echoes a check pattern.
        // A version 1 card rejects the command outright, which is how the two generations are
        // told apart. This board's card answers version 2.
        let response = self.command(8, 0x0000_01AA)?;
        if response & 0x04 != 0 {
            // Illegal command: a version 1 card. Nothing here handles one, and no card of that
            // age is likely to be in this case, but say so rather than misread its addressing.
            return Err(Error::Unsupported);
        }
        if response != 0x01 {
            return Err(Error::Response(response));
        }
        let mut trailer = [0xFFu8; 4];
        self.read_bytes(&mut trailer)?;
        self.release();
        if trailer[2] != 0x01 || trailer[3] != 0xAA {
            return Err(Error::Unsupported);
        }

        // ACMD41 is the actual start-up command, and the card answers 0x01 -- still busy -- for
        // as long as it needs. The high capacity support bit in the argument is what allows it to
        // come back as an SDHC card rather than pretending to be small.
        let mut elapsed = 0;
        loop {
            self.command(55, 0)?;
            if self.command(41, 0x4000_0000)? == 0x00 {
                break;
            }
            if elapsed >= INIT_TIMEOUT_MS {
                return Err(Error::Timeout);
            }
            self.delay.delay_millis(10);
            elapsed += 10;
        }

        // CMD58 reads the operating conditions register, whose bit 30 says whether the card
        // counts in blocks. Reading it only means anything after the handshake has finished.
        let response = self.command(58, 0)?;
        if response != 0x00 {
            return Err(Error::Response(response));
        }
        let mut ocr = [0xFFu8; 4];
        self.read_bytes(&mut ocr)?;
        self.release();
        self.kind = if ocr[0] & 0x40 != 0 {
            Kind::HighCapacity
        } else {
            Kind::StandardCapacity
        };

        // A standard capacity card may have come up with some other block length; a high capacity
        // one is fixed at 512 and ignores this. Sending it to both costs one command.
        let response = self.command(16, 512)?;
        self.release();
        if response != 0x00 {
            return Err(Error::Response(response));
        }
        Ok(())
    }

    /// Read one 512-byte block. `lba` is a block number regardless of the card's addressing --
    /// the byte offset a standard capacity card wants is worked out here.
    pub fn read_block(&mut self, lba: u32, buffer: &mut [u8; 512]) -> Result<(), Error> {
        let address = match self.kind {
            Kind::HighCapacity => lba,
            Kind::StandardCapacity => lba * 512,
        };
        let response = self.command(17, address)?;
        if response != 0x00 {
            self.release();
            return Err(Error::Response(response));
        }
        let result = self.read_data(buffer);
        self.release();
        result
    }

    /// Read a run of consecutive blocks in one command.
    ///
    /// `buffer` is filled block by block and its length must be a whole number of them. This is
    /// CMD18, which streams until it is told to stop: one command and one address for a whole
    /// run, where [`SdCard::read_block`] pays both per block. The card also knows what is coming
    /// and can read ahead, which single-block reads never let it do.
    ///
    /// A failure part-way through still ends the stream -- a card left streaming would answer
    /// the next command with data.
    pub fn read_blocks(&mut self, lba: u32, buffer: &mut [u8]) -> Result<(), Error> {
        if buffer.len() % 512 != 0 {
            return Err(Error::Unsupported);
        }
        if buffer.is_empty() {
            return Ok(());
        }
        let address = match self.kind {
            Kind::HighCapacity => lba,
            Kind::StandardCapacity => lba * 512,
        };
        let response = self.command(18, address)?;
        if response != 0x00 {
            self.release();
            return Err(Error::Response(response));
        }
        for block in buffer.chunks_exact_mut(512) {
            if let Err(err) = self.read_data(block) {
                let _ = self.stop_transmission();
                self.release();
                return Err(err);
            }
        }
        let result = self.stop_transmission();
        self.release();
        result
    }

    /// Change the bus clock of an initialised card.
    ///
    /// The handshake happens at [`INIT_RATE`] and [`SdCard::new`] leaves the bus at
    /// [`FAST_RATE`]; this exists so a measurement can ask what a different rate does to the
    /// same read.
    pub fn set_rate(&mut self, rate: Rate) -> Result<(), Error> {
        self.spi
            .apply_config(&SpiConfig::default().with_frequency(rate))
            .map_err(|_| Error::Unsupported)
    }

    /// The card identification register: manufacturer, product name, serial, date.
    pub fn cid(&mut self) -> Result<[u8; 16], Error> {
        self.read_register(10)
    }

    /// The card specific data register, which carries the capacity among much else.
    pub fn csd(&mut self) -> Result<[u8; 16], Error> {
        self.read_register(9)
    }

    /// How many 512-byte blocks the card holds, from its CSD.
    ///
    /// The two CSD versions state it quite differently, and **which one a card uses is not the
    /// same question as which protocol it speaks**: the card in this board answers CMD8 like a
    /// version 2 card but is a 512 MB standard capacity card underneath, so it carries a version
    /// 1 CSD. A reader that assumes the newer form because the handshake
    /// looked modern reports nothing at all for it.
    ///
    /// - Version 1 multiplies three fields: a device size, a size multiplier, and the read block
    ///   length the card would use if asked.
    /// - Version 2 states a plain count of 512 KB units, and nothing else.
    pub fn capacity_blocks(&mut self) -> Result<u32, Error> {
        let csd = self.csd()?;
        match csd[0] >> 6 {
            0 => {
                let size = (u32::from(csd[6] & 0x03) << 10)
                    | (u32::from(csd[7]) << 2)
                    | u32::from(csd[8] >> 6);
                let multiplier = (u32::from(csd[9] & 0x03) << 1) | u32::from(csd[10] >> 7);
                let block_len = u32::from(csd[5] & 0x0F);
                // (size + 1) * 2^(multiplier + 2) blocks of 2^block_len bytes, restated in the
                // 512-byte blocks everything else here counts in.
                Ok((size + 1) << (multiplier + 2 + block_len - 9))
            }
            1 => {
                let size =
                    (u32::from(csd[7] & 0x3F) << 16) | (u32::from(csd[8]) << 8) | u32::from(csd[9]);
                Ok((size + 1) * 1024)
            }
            _ => Err(Error::Unsupported),
        }
    }

    /// CMD9 and CMD10 both answer with a sixteen-byte register wrapped in a data block.
    fn read_register(&mut self, command: u8) -> Result<[u8; 16], Error> {
        let response = self.command(command, 0)?;
        if response != 0x00 {
            self.release();
            return Err(Error::Response(response));
        }
        let mut register = [0u8; 16];
        let result = self.read_data(&mut register);
        self.release();
        result?;
        Ok(register)
    }

    /// Wait for the data token, then take the payload and the two CRC bytes that follow it.
    fn read_data(&mut self, buffer: &mut [u8]) -> Result<(), Error> {
        for _ in 0..TOKEN_TRIES {
            let token = self.exchange(0xFF)?;
            if token == TOKEN_START_BLOCK {
                self.read_bytes(buffer)?;
                let mut crc = [0xFFu8; 2];
                self.read_bytes(&mut crc)?;
                return Ok(());
            }
            // Anything else with the top bits clear is an error token: the low nibble names the
            // fault. 0xFF means the card is still thinking.
            if token != 0xFF && token & 0xF0 == 0 {
                return Err(Error::DataError(token));
            }
        }
        Err(Error::Timeout)
    }

    /// Send a command and return its one-byte response, leaving chip select **low** so a caller
    /// that expects more bytes can read them. Callers that expect nothing further call
    /// [`SdCard::release`].
    fn command(&mut self, command: u8, argument: u32) -> Result<u8, Error> {
        self.cs.set_high();
        self.exchange(0xFF)?;
        self.cs.set_low();
        // A byte of idle after selecting the card; some cards want the gap.
        self.exchange(0xFF)?;
        self.send_frame(command, argument)?;
        match self.poll_response()? {
            Some(response) => Ok(response),
            None => {
                self.release();
                Err(Error::NoCard)
            }
        }
    }

    /// The six bytes a command is, put on the wire without touching chip select.
    ///
    /// Separate from [`SdCard::command`] because CMD12 has to be sent **inside** a transfer that
    /// is already running: deselecting the card first, as an ordinary command does, would end the
    /// stream in the middle of a block.
    fn send_frame(&mut self, command: u8, argument: u32) -> Result<(), Error> {
        let mut frame = [
            0x40 | command,
            (argument >> 24) as u8,
            (argument >> 16) as u8,
            (argument >> 8) as u8,
            argument as u8,
            0,
        ];
        frame[5] = crc7(&frame[..5]);
        self.spi.write(&frame)?;
        Ok(())
    }

    /// Read bytes until one of them has its top bit clear, which is the R1 response.
    ///
    /// `None` means the card said nothing in [`RESPONSE_TRIES`] bytes.
    fn poll_response(&mut self) -> Result<Option<u8>, Error> {
        for _ in 0..RESPONSE_TRIES {
            let response = self.exchange(0xFF)?;
            if response & 0x80 == 0 {
                return Ok(Some(response));
            }
        }
        Ok(None)
    }

    /// End a multi-block read.
    ///
    /// CMD12 is the one command whose answer does not begin where the others do: the card is
    /// still shifting out the transfer when it arrives, so a **stuff byte** comes back before the
    /// response, and afterwards the card holds the data line low for as long as it is busy.
    fn stop_transmission(&mut self) -> Result<(), Error> {
        self.send_frame(12, 0)?;
        self.exchange(0xFF)?;
        match self.poll_response()? {
            Some(_) => {}
            None => return Err(Error::NoCard),
        }
        self.wait_ready()
    }

    /// Wait out a card that is holding the data line low.
    fn wait_ready(&mut self) -> Result<(), Error> {
        let mut elapsed = 0;
        loop {
            if self.exchange(0xFF)? == 0xFF {
                return Ok(());
            }
            if elapsed >= BUSY_TIMEOUT_MS {
                return Err(Error::Timeout);
            }
            self.delay.delay_millis(1);
            elapsed += 1;
        }
    }

    /// Let the card go, with the trailing clocks it needs to finish its own bookkeeping.
    fn release(&mut self) {
        self.cs.set_high();
        let _ = self.exchange(0xFF);
    }

    /// One byte out, one byte in.
    fn exchange(&mut self, out: u8) -> Result<u8, Error> {
        let mut buffer = [out];
        self.spi.transfer(&mut buffer)?;
        Ok(buffer[0])
    }

    /// Fill a buffer from the card, holding the data line high throughout.
    fn read_bytes(&mut self, buffer: &mut [u8]) -> Result<(), Error> {
        buffer.fill(0xFF);
        self.spi.transfer(buffer)?;
        Ok(())
    }
}

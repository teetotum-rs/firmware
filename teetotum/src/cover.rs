//! Pulling one cover art picture off the other chip, and putting it behind the screen.
//!
//! This is the only thing on the link that moves more than a few dozen bytes, and it is
//! **pull-driven**: the other chip offers a picture with `BD 01`, and every packet after that
//! arrives because we asked for it by number. So what is in here is mostly timing, and every
//! rule of it was paid for at the device:
//!
//! * **the first request waits** [`FIRST_PACKET_DELAY`] -- the other chip writes the offer and
//!   only *then* sets the send state its own `A3 01` handler insists on;
//! * **a refusal with reason 2 is worth repeating** and the other three are not, because they
//!   are about this request being wrong, and a wrong request repeated repeats the answer;
//! * **a request that goes unanswered is the one failure the other chip does not report**, so
//!   it is counted here instead and the transfer is given up after [`MAX_STALLS`] of them;
//! * **nothing slow may run while a request is out.** The receive FIFO holds 128 bytes and
//!   fills in 1.4 ms at 921600 baud; a packet is 1016 bytes and a redrawn screen costs 14. A
//!   caller that draws on the way to an answer eats the answer -- which is what happened when
//!   34 packets were offered and none arrived. [`Cover::busy`] is that rule,
//!   and a whole transfer is under half a second, so holding still for it costs nothing.
//!
//! A finished transfer goes through [`show`], which decodes it once and keeps it as the
//! backdrop: scaling a cover costs up to 587 ms and copying it back costs 21, so it is made
//! once per picture and not once per frame.

use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;
use esp_hal::time::{Duration, Instant};
use log::{error, info};

use crate::companion::{COVER_PACKET_STRIDE, Companion, Event};
use crate::image::{self, Scaler};
use crate::screen::Screen;

/// How long to wait after `BD 01` before asking for the first packet.
///
/// The factory S3 firmware could not lose the race this covers: its reply went through a UART
/// task and an event queue. Ours can, and this is the pause that gives the other side its
/// instruction or two.
const FIRST_PACKET_DELAY: Duration = Duration::from_millis(10);
/// How long a requested packet may take before it is asked for again. The other chip answers in
/// well under a millisecond; this is only there so a lost frame does not end the transfer.
const PACKET_TIMEOUT: Duration = Duration::from_millis(400);
/// How often a transfer refused with "not in sending" is offered a second chance, and how long
/// after the refusal. The image stays in the other chip's buffer, so asking again costs nothing.
const NOT_SENDING_RETRIES: u8 = 4;
const RETRY_DELAY: Duration = Duration::from_millis(40);
/// How many unanswered requests are enough to call a transfer dead.
pub const MAX_STALLS: u8 = 8;
/// The reason reported when nothing came back at all.
///
/// It is deliberately none of the other chip's four -- those all arrive as a `BD 03` and are
/// numbered 1 to 4 -- so that a silence cannot be mistaken for an answer.
pub const NO_ANSWER: u8 = 0xFF;

/// One thing that happened to a transfer, for a caller that keeps a log or a line on the glass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// A picture is on offer, and the first packet has been asked for.
    Offered { id: u8, packets: u16 },
    /// One more packet is in.
    Packet { packet: u16, of: u16 },
    /// The last packet arrived and the other chip has been told the transfer is done.
    Complete { bytes: usize },
    /// The other chip refused the request. Reasons 1 to 4 are its own; see
    /// [`Companion::request_cover_packet`].
    Refused { reason: u8 },
    /// [`MAX_STALLS`] requests went unanswered.
    Silent { bytes: usize },
}

/// Where a transfer stands, for a caller that shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Progress {
    /// How many packets the offer named.
    pub packets: u16,
    /// How many of them are in.
    pub got: u16,
    /// How many bytes that is.
    pub bytes: usize,
    /// Whether the last one has arrived.
    pub done: bool,
    /// Why it stopped, if it did. [`NO_ANSWER`] is ours, the rest are the other chip's.
    pub aborted: Option<u8>,
}

/// A cover that was decoded, scaled and put in the backdrop.
///
/// The picture itself is not here -- it went into [`Screen::stash`] the moment it was made.
/// What is kept is what decides how it was made: **how big it arrived**, which says whether it
/// was scaled up or down and therefore which filter it got.
#[derive(Clone, Copy, Debug)]
pub struct Art {
    /// The picture's own width, as the phone sent it. Measured: 200 pixels, so a
    /// cover is scaled **up** by 1.8 and every scaler judgement made on the way down is about
    /// a different operation.
    pub width: usize,
    pub height: usize,
    /// Which filter drew it.
    pub scaler: Scaler,
    /// Whether that filter was the picture's own choice or pressed on it by the caller.
    pub forced: bool,
    /// How long decoding and scaling took.
    pub millis: u32,
}

/// The transfer in progress, or the last one that finished.
struct Transfer {
    id: u8,
    packets: u16,
    /// The packet [`Transfer::asked`] was last spent on, numbered from one as the protocol does.
    wanted: u16,
    asked: Instant,
    bytes: usize,
    done: bool,
    aborted: Option<u8>,
    /// When the next request may go out, or `None` if one is already outstanding.
    due: Option<Instant>,
    /// How many refusals with reason 2 are still worth another try.
    retries: u8,
    /// How many requests have gone unanswered.
    stalls: u8,
}

/// The cover art side of the link: hand it every frame, and it does the asking.
///
/// The bytes land in a buffer the caller owns, because the only memory on this board big enough
/// to hold a picture is the external RAM behind the screen -- see [`Screen::take_spare`].
pub struct Cover<'a> {
    buffer: &'a mut [u8],
    transfer: Option<Transfer>,
}

impl<'a> Cover<'a> {
    /// A cover receiver writing into `buffer`.
    ///
    /// [`crate::companion::COVER_MAX_BYTES`] is what both firmwares allocate and therefore the
    /// largest picture that can arrive; a smaller buffer truncates rather than refuses, which
    /// leaves a half picture that will not decode.
    pub fn new(buffer: &'a mut [u8]) -> Self {
        Self {
            buffer,
            transfer: None,
        }
    }

    /// Takes one frame off the link, and answers it if it belongs to a transfer.
    ///
    /// Everything that is not cover art is ignored and returns `None`, so a caller can hand
    /// this every event it polls and deal with the rest afterwards.
    pub fn feed(&mut self, event: Event, link: &mut Companion<'_>) -> Option<Step> {
        match event {
            Event::CoverBegin { id, packets } => {
                // A picture that does not fill the buffer must not be read over the last one:
                // the tail would be the previous cover's bytes, and a JPEG decoder follows them.
                let room = self
                    .buffer
                    .len()
                    .min(usize::from(packets) * COVER_PACKET_STRIDE);
                self.buffer[..room].fill(0);
                let now = Instant::now();
                self.transfer = Some(Transfer {
                    id,
                    packets,
                    wanted: 1,
                    asked: now,
                    bytes: 0,
                    done: false,
                    aborted: None,
                    due: Some(now + FIRST_PACKET_DELAY),
                    retries: NOT_SENDING_RETRIES,
                    stalls: 0,
                });
                Some(Step::Offered { id, packets })
            }
            Event::CoverPacket { id, packet, bytes } => {
                let payload = link.payload();
                let state = self.transfer.as_mut()?;
                if state.id != id || packet < 1 || payload.len() < 4 + bytes {
                    return None;
                }
                let at = usize::from(packet - 1) * COVER_PACKET_STRIDE;
                let end = (at + bytes).min(self.buffer.len());
                if at < end {
                    self.buffer[at..end].copy_from_slice(&payload[4..4 + (end - at)]);
                    state.bytes = state.bytes.max(end);
                }
                state.stalls = 0;
                if packet >= state.packets {
                    state.done = true;
                    link.cover_complete();
                    Some(Step::Complete { bytes: state.bytes })
                } else {
                    state.wanted = packet + 1;
                    state.asked = Instant::now();
                    state.aborted = None;
                    link.request_cover_packet(state.id, state.wanted);
                    Some(Step::Packet {
                        packet,
                        of: state.packets,
                    })
                }
            }
            Event::CoverAborted { reason } => {
                let state = self.transfer.as_mut()?;
                state.aborted = Some(reason);
                // Reason 2 is "not in sending": the offer is out but the other chip has not
                // finished making itself ready. That is worth waiting out.
                if reason == 2 && state.retries > 0 && !state.done {
                    state.retries -= 1;
                    state.due = Some(Instant::now() + RETRY_DELAY);
                }
                Some(Step::Refused { reason })
            }
            _ => None,
        }
    }

    /// Sends whatever request is due, and gives up on a transfer nothing is answering.
    ///
    /// Every request leaves through here and nowhere else, which is what makes them holdable:
    /// the first by [`FIRST_PACKET_DELAY`], a retry by [`RETRY_DELAY`], and a packet that never
    /// came by [`PACKET_TIMEOUT`]. The other chip keeps the image until it is told the transfer
    /// is complete, so asking twice is harmless.
    pub fn tick(&mut self, link: &mut Companion<'_>) -> Option<Step> {
        let state = self.transfer.as_mut()?;
        if state.done {
            return None;
        }
        let now = Instant::now();
        let timed_out = state.aborted.is_none() && state.asked.elapsed() > PACKET_TIMEOUT;
        let send = match state.due {
            Some(due) => now >= due,
            None => timed_out,
        };
        if timed_out {
            state.stalls += 1;
        }
        if state.stalls >= MAX_STALLS {
            state.aborted = Some(NO_ANSWER);
            return Some(Step::Silent { bytes: state.bytes });
        }
        if send {
            state.due = None;
            state.asked = now;
            // A request that is on its way is not a refused transfer any more, and the
            // difference matters: it is what decides whether the caller holds still for the
            // answer or goes off to redraw the screen over it.
            state.aborted = None;
            link.request_cover_packet(state.id, state.wanted);
        }
        None
    }

    /// Whether a request is out and the link must be left alone.
    ///
    /// See the module's fourth rule: a caller that draws while this is true loses packets, not
    /// frames.
    pub fn busy(&self) -> bool {
        self.transfer
            .as_ref()
            .is_some_and(|state| !state.done && state.aborted.is_none())
    }

    /// Where the transfer stands, or `None` if none has been offered yet.
    pub fn progress(&self) -> Option<Progress> {
        let state = self.transfer.as_ref()?;
        Some(Progress {
            packets: state.packets,
            got: state.wanted.saturating_sub(1),
            bytes: state.bytes,
            done: state.done,
            aborted: state.aborted,
        })
    }

    /// The bytes that have arrived, finished or not. Empty until one has.
    pub fn received(&self) -> &[u8] {
        match self.transfer.as_ref() {
            Some(state) => &self.buffer[..state.bytes],
            None => &[],
        }
    }

    /// The finished picture, or `None` while one is still coming or none has come.
    pub fn image(&self) -> Option<&[u8]> {
        let state = self.transfer.as_ref()?;
        (state.done && state.bytes > 0).then(|| self.received())
    }
}

/// How big a cover is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoverSize {
    /// As big as the glass allows: a 200x200 cover comes up by 1.8.
    Glass,
    /// At its own size in the middle, as long as it fits: sharp, and smaller.
    Native,
    /// Brought up to fill a round window of this diameter in the middle, black outside it: for
    /// a cover that should end where something round begins, such as an arc around it.
    Disc(usize),
}

/// Decodes a finished cover, draws it at `size` and keeps it as the backdrop.
///
/// `pixels` is where it is unpacked, three bytes to a pixel of the picture's own size -- so a
/// 200x200 cover wants 120 KiB and there is nowhere but the external RAM to put it. `forced`
/// overrules [`crate::image::Picture::scaler_for`], for a run that is comparing the filters at
/// the glass rather than trusting the judgement in the module.
///
/// The frame is left holding the picture and the backdrop holding a copy of it. `None`, with a
/// line in the log, if the bytes are not a picture, if `pixels` is too small for it, or if
/// there is no backdrop to keep one in.
pub fn show(
    screen: &mut Screen<'_>,
    jpeg: &[u8],
    pixels: &mut [u8],
    size: CoverSize,
    forced: Option<Scaler>,
) -> Option<Art> {
    let started = Instant::now();
    let picture = match image::decode(jpeg, pixels) {
        Ok(picture) => picture,
        Err(err) => {
            error!("Cover: it did not decode: {err:?}");
            return None;
        }
    };

    let (width, height) = (picture.width, picture.height);
    let fit = match size {
        CoverSize::Glass => picture.fit(),
        CoverSize::Native => picture.native(),
        CoverSize::Disc(diameter) => picture.fit_within(diameter),
    };
    let scaler = forced.unwrap_or_else(|| picture.scaler_for(fit));
    // The bands a picture that is not square leaves behind are cleared here, not left as
    // whatever the last frame drew.
    screen.frame().clear(Rgb565::BLACK).ok();
    picture.draw_at(screen.frame(), fit, scaler);
    if let CoverSize::Disc(diameter) = size {
        black_outside(screen.frame(), diameter as i32 / 2);
    }
    if !screen.stash() {
        error!("Cover: there is no backdrop, so it cannot be kept");
        return None;
    }

    let millis = started.elapsed().as_millis() as u32;
    info!("Cover: {width}x{height} with {} in {millis} ms", scaler.name());
    Some(Art {
        width,
        height,
        scaler,
        forced: forced.is_some(),
        millis,
    })
}

/// Blacks out everything `radius` or further from the middle of the glass, a row at a time.
///
/// What makes [`CoverSize::Disc`] round. The middle of the glass lies between pixels 179 and
/// 180, so distances are taken in half pixels from the middles of pixels, the way the menu ring
/// takes them.
fn black_outside(frame: &mut crate::framebuffer::Framebuffer, radius: i32) {
    use crate::framebuffer::{HEIGHT, WIDTH};
    use embedded_graphics::prelude::*;
    use embedded_graphics::primitives::Rectangle;

    let (width, centre) = (WIDTH as i32, WIDTH as i32 / 2);
    for y in 0..HEIGHT as i32 {
        let dy = 2 * y + 1 - 2 * centre;
        let reach = 4 * radius * radius - dy * dy;
        // Half the kept span, in whole pixels either side of the middle.
        let half = if reach > 0 { reach.isqrt() / 2 } else { 0 };
        let (left, right) = (centre - half, centre + half);
        for (x, run) in [(0, left), (right, width - right)] {
            if run > 0 {
                let row = Rectangle::new(Point::new(x, y), Size::new(run as u32, 1));
                let _ = frame.fill_solid(&row, Rgb565::BLACK);
            }
        }
    }
}

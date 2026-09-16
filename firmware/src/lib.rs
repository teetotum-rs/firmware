//! What the firmware's binaries share and the SDK does not carry.
//!
//! `teetotum` is the SDK, so what goes into it is meant to be handed on. Finding a partition is
//! not: the partition table names the application image as well, and whatever can search it
//! can find that too. So the lookup lives here, on the firmware's side of the line, and hands
//! the SDK a region to work inside. The plugin loader is here for a reason of the same kind: it
//! decides what a face may do, and that is the firmware's decision.

#![no_std]

extern crate alloc;

/// What this build calls itself, on the About screen and under the shared card's listing: the
/// release version and the commit it was built from.
pub const VERSION: &str = concat!("v", env!("CARGO_PKG_VERSION"), " ", env!("TEETOTUM_COMMIT"));

pub mod backlight;
pub mod flash;
pub mod nearby;
pub mod plugin;
pub mod qr;
pub mod settings;
pub mod share;
pub mod shot;
pub mod slots;
pub mod storage;
pub mod upload;

//! Panel initialisation for the ST77916 as wired on this board.
//!
//! The table holds the register values the factory firmware programs into the panel, read out
//! of its image (app0.bin offset 0x0006bc, 0x0106bc in the full image) and converted entry for
//! entry. It differs from Espressif's `vendor_specific_init_default` in esp-iot-solution in
//! exactly the registers that decide whether the screen keeps its picture: B2h VCOM, B5h
//! AVDD/AVCL pump steps, B6h VGH/VGL pump steps, and the whole gamma and GIP block. AVDD, VGH,
//! VGL and VCOM come from the controller's own charge pumps and the panel supply is plain 3V3,
//! so these registers are the only thing between a picture that holds and one that sags away.
//!
//! The table is a page state machine. Manufacturer registers live behind command pages that
//! F0h..F3h open and close, and a write made with the wrong page open is **discarded in
//! silence**. Reordering or dropping any F0/F1/F2/F3 write breaks the whole sequence.
//!
//! MADCTL and COLMOD are prepended here because ESP-IDF sends them from struct fields before the
//! table. Unlike Espressif's, this table has no OTP dump loop, RAM auto-fill or window commands.

/// One initialisation step: register, parameters, and milliseconds to wait afterwards.
pub type InitCommand = (u8, &'static [u8], u16);

/// How the panel sits in its case on this board, as MADCTL (36h) bits.
///
/// This is a property of the hardware, not a preference. The panel is fitted turned by 180
/// degrees against the controller's native scan order, so the driver's default of `0x00` puts
/// row 0 at the bottom of the screen and column 0 on the right. Bits 7 and 6 mirror Y and X, and
/// setting both undoes that mount. Bit 3 stays clear -- the colours are already RGB. Measured
/// with `src/bin/orientation.rs`: an F drawn with this value reads upright with
/// the USB socket pointing away from the viewer.
///
/// A board that mounts the same panel the other way up corrects it here, and only here.
///
/// **Which way the user wants to hold the device is a different question and does not belong in
/// this constant.** MADCTL can only express the eight combinations of mirror-X, mirror-Y and
/// axis exchange -- 0, 90, 180, 270 degrees plus mirrors -- so a freely chosen viewing angle
/// cannot come from the controller at all. That is a job for the rendering layer, settable at
/// run time and stored in NVS, on top of the correction made here.
pub const PANEL_MOUNT_MADCTL: u8 = 0xC0;

/// The factory initialisation sequence, in order.
pub const INIT_COMMANDS: &[InitCommand] = &[
    // Prepended, as `esp_lcd_panel_init` does: memory access control. This corrects the
    // mounting of the panel and nothing else -- see `PANEL_MOUNT_MADCTL`.
    (0x36, &[PANEL_MOUNT_MADCTL], 0),
    // Prepended likewise: 16 bits per pixel, RGB565.
    (0x3A, &[0x55], 0),
    // Test page open. The factory writes 0x28 here where Espressif writes 0x08, and it
    // sets four different registers behind it.
    (0xF0, &[0x28], 0),
    (0xF2, &[0x28], 0),
    (0x73, &[0xF0], 0),
    (0x7C, &[0xD1], 0),
    (0x83, &[0xE0], 0),
    (0x84, &[0x61], 0),
    (0xF2, &[0x82], 0),
    (0xF0, &[0x00], 0),
    // Command2 page open. Everything from here -- charge pumps, VCOM, gamma rails, frame
    // rate -- is only writable while F0=0x01 and F1=0x01 stand.
    (0xF0, &[0x01], 0),
    (0xF1, &[0x01], 0),
    (0xB0, &[0x56], 0),
    (0xB1, &[0x4D], 0),
    (0xB2, &[0x24], 0),
    (0xB4, &[0x87], 0),
    (0xB5, &[0x44], 0),
    (0xB6, &[0x8B], 0),
    (0xB7, &[0x40], 0),
    (0xB8, &[0x86], 0),
    (0xBA, &[0x00], 0),
    (0xBB, &[0x08], 0),
    (0xBC, &[0x08], 0),
    (0xBD, &[0x00], 0),
    (0xC0, &[0x80], 0),
    (0xC1, &[0x10], 0),
    (0xC2, &[0x37], 0),
    (0xC3, &[0x80], 0),
    (0xC4, &[0x10], 0),
    (0xC5, &[0x37], 0),
    (0xC6, &[0xA9], 0),
    (0xC7, &[0x41], 0),
    (0xC8, &[0x01], 0),
    (0xC9, &[0xA9], 0),
    (0xCA, &[0x41], 0),
    (0xCB, &[0x01], 0),
    (0xD0, &[0x91], 0),
    (0xD1, &[0x68], 0),
    (0xD2, &[0x68], 0),
    (0xF5, &[0x00, 0xA5], 0),
    (0xDD, &[0x4F], 0),
    // Command2 page closed.
    (0xDE, &[0x4F], 0),
    (0xF1, &[0x10], 0),
    // Gamma page open.
    (0xF0, &[0x00], 0),
    (0xF0, &[0x02], 0),
    (
        0xE0,
        &[
            0xF0, 0x0A, 0x10, 0x09, 0x09, 0x36, 0x35, 0x33, 0x4A, 0x29, 0x15, 0x15, 0x2E, 0x34,
        ],
        0,
    ),
    // GIP page open: gate-in-panel, the driving of the screen rows themselves.
    (
        0xE1,
        &[
            0xF0, 0x0A, 0x0F, 0x08, 0x08, 0x05, 0x34, 0x33, 0x4A, 0x39, 0x15, 0x15, 0x2D, 0x33,
        ],
        0,
    ),
    (0xF0, &[0x10], 0),
    (0xF3, &[0x10], 0),
    (0xE0, &[0x07], 0),
    (0xE1, &[0x00], 0),
    (0xE2, &[0x00], 0),
    (0xE3, &[0x00], 0),
    (0xE4, &[0xE0], 0),
    (0xE5, &[0x06], 0),
    (0xE6, &[0x21], 0),
    (0xE7, &[0x01], 0),
    (0xE8, &[0x05], 0),
    (0xE9, &[0x02], 0),
    (0xEA, &[0xDA], 0),
    (0xEB, &[0x00], 0),
    (0xEC, &[0x00], 0),
    (0xED, &[0x0F], 0),
    (0xEE, &[0x00], 0),
    (0xEF, &[0x00], 0),
    (0xF8, &[0x00], 0),
    (0xF9, &[0x00], 0),
    (0xFA, &[0x00], 0),
    (0xFB, &[0x00], 0),
    (0xFC, &[0x00], 0),
    (0xFD, &[0x00], 0),
    (0xFE, &[0x00], 0),
    (0xFF, &[0x00], 0),
    (0x60, &[0x40], 0),
    (0x61, &[0x04], 0),
    (0x62, &[0x00], 0),
    (0x63, &[0x42], 0),
    (0x64, &[0xD9], 0),
    (0x65, &[0x00], 0),
    (0x66, &[0x00], 0),
    (0x67, &[0x00], 0),
    (0x68, &[0x00], 0),
    (0x69, &[0x00], 0),
    (0x6A, &[0x00], 0),
    (0x6B, &[0x00], 0),
    (0x70, &[0x40], 0),
    (0x71, &[0x03], 0),
    (0x72, &[0x00], 0),
    (0x73, &[0x42], 0),
    (0x74, &[0xD8], 0),
    (0x75, &[0x00], 0),
    (0x76, &[0x00], 0),
    (0x77, &[0x00], 0),
    (0x78, &[0x00], 0),
    (0x79, &[0x00], 0),
    (0x7A, &[0x00], 0),
    (0x7B, &[0x00], 0),
    (0x80, &[0x48], 0),
    (0x81, &[0x00], 0),
    (0x82, &[0x06], 0),
    (0x83, &[0x02], 0),
    (0x84, &[0xD6], 0),
    (0x85, &[0x04], 0),
    (0x86, &[0x00], 0),
    (0x87, &[0x00], 0),
    (0x88, &[0x48], 0),
    (0x89, &[0x00], 0),
    (0x8A, &[0x08], 0),
    (0x8B, &[0x02], 0),
    (0x8C, &[0xD8], 0),
    (0x8D, &[0x04], 0),
    (0x8E, &[0x00], 0),
    (0x8F, &[0x00], 0),
    (0x90, &[0x48], 0),
    (0x91, &[0x00], 0),
    (0x92, &[0x0A], 0),
    (0x93, &[0x02], 0),
    (0x94, &[0xDA], 0),
    (0x95, &[0x04], 0),
    (0x96, &[0x00], 0),
    (0x97, &[0x00], 0),
    (0x98, &[0x48], 0),
    (0x99, &[0x00], 0),
    (0x9A, &[0x0C], 0),
    (0x9B, &[0x02], 0),
    (0x9C, &[0xDC], 0),
    (0x9D, &[0x04], 0),
    (0x9E, &[0x00], 0),
    (0x9F, &[0x00], 0),
    (0xA0, &[0x48], 0),
    (0xA1, &[0x00], 0),
    (0xA2, &[0x05], 0),
    (0xA3, &[0x02], 0),
    (0xA4, &[0xD5], 0),
    (0xA5, &[0x04], 0),
    (0xA6, &[0x00], 0),
    (0xA7, &[0x00], 0),
    (0xA8, &[0x48], 0),
    (0xA9, &[0x00], 0),
    (0xAA, &[0x07], 0),
    (0xAB, &[0x02], 0),
    (0xAC, &[0xD7], 0),
    (0xAD, &[0x04], 0),
    (0xAE, &[0x00], 0),
    (0xAF, &[0x00], 0),
    (0xB0, &[0x48], 0),
    (0xB1, &[0x00], 0),
    (0xB2, &[0x09], 0),
    (0xB3, &[0x02], 0),
    (0xB4, &[0xD9], 0),
    (0xB5, &[0x04], 0),
    (0xB6, &[0x00], 0),
    (0xB7, &[0x00], 0),
    (0xB8, &[0x48], 0),
    (0xB9, &[0x00], 0),
    (0xBA, &[0x0B], 0),
    (0xBB, &[0x02], 0),
    (0xBC, &[0xDB], 0),
    (0xBD, &[0x04], 0),
    (0xBE, &[0x00], 0),
    (0xBF, &[0x00], 0),
    (0xC0, &[0x10], 0),
    (0xC1, &[0x47], 0),
    (0xC2, &[0x56], 0),
    (0xC3, &[0x65], 0),
    (0xC4, &[0x74], 0),
    (0xC5, &[0x88], 0),
    (0xC6, &[0x99], 0),
    (0xC7, &[0x01], 0),
    (0xC8, &[0xBB], 0),
    (0xC9, &[0xAA], 0),
    (0xD0, &[0x10], 0),
    (0xD1, &[0x47], 0),
    (0xD2, &[0x56], 0),
    (0xD3, &[0x65], 0),
    (0xD4, &[0x74], 0),
    (0xD5, &[0x88], 0),
    (0xD6, &[0x99], 0),
    (0xD7, &[0x01], 0),
    (0xD8, &[0xBB], 0),
    (0xD9, &[0xAA], 0),
    // GIP page closed.
    (0xF3, &[0x01], 0),
    (0xF0, &[0x00], 0),
    // The tail. Inversion on, out of sleep, display on -- and unlike Espressif's table
    // there is no OTP dump, no RAM clear and no window setup.
    (0x21, &[0x00], 0),
    (0x11, &[0x00], 120),
    (0x29, &[0x00], 0),
];

/// What the factory application sends after the table has run.
///
/// `ESP_PanelLcd::invertColor(true)` and `displayOn()` follow the table immediately, so INVON and
/// DISPON go out a second time -- the table already carries both. Harmless, and replayed here
/// because the aim is to match a sequence that demonstrably works, not to tidy it up.
pub const POST_INIT_COMMANDS: &[InitCommand] = &[
    // Inversion on, a second time.
    (0x21, &[], 0),
    // Display on, a second time, then the factory's own 100 ms before it draws.
    (0x29, &[], 100),
];

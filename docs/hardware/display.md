# Display

The panel, what a frame costs, and where the time goes when the picture turns.
Part of the [hardware documentation](README.md).

## Display: the picture holds

The panel shows a red screen and keeps it. Getting there took one change, and it was not the one
the symptom suggested.

For a while the fill appeared and faded to black within a second or two; redrawing it fifty times
over five seconds did not help, so it was never a matter of feeding the panel often enough. The
sequence being sent was Espressif's `vendor_specific_init_default` from
`esp_lcd_st77916` — the same driver the factory firmware links against, which seemed like reason
enough to trust it.

**It is not the table the factory uses.** Disassembling the app in `backup/` shows the factory
installing its own 184-entry table through `ESP_PanelLcd::configVendorCommands`; Espressif's
216-entry default is referenced exactly once, in a fallback branch that never runs. The two
disagree in precisely the registers that decide whether the glass stays driven:

| Register | Espressif | factory | what it sets |
|---|---|---|---|
| B2h | `0x2A` | `0x24` | VCOM |
| B5h | `0x34` | `0x44` | AVDD / AVCL charge-pump steps |
| B6h | `0xD5` | `0x8B` | VGH / VGL charge-pump steps |
| E0h/E1h | Espressif's | different throughout | gamma |
| GIP block | Espressif's | different throughout | gate-in-panel timing |

AVDD, VGH, VGL and VCOM never appear on the display connector: the schematic runs the panel
supply straight from the board's 3V3 rail, with no enable pin, no load switch and no boost
converter. The controller makes those rails itself, from its own charge pumps, configured by
exactly those registers. Too weak a VGH means the pixels charge and then relax back — an image
that *fades* over a second rather than snapping to black, which is what was on the glass.

Replaying the factory's table verbatim fixed it in one go. `teetotum/src/panel.rs` now holds that table,
converted entry for entry, with the page structure marked in comments.

Two details of that sequence are worth stating, because both are silent when wrong:

- **The table is a page state machine.** Manufacturer registers are only writable while the right
  command page is open — `F0=0x01` plus `F1=0x01` for the analog block, `F0=0x02` for gamma,
  `F0=0x10` plus `F3=0x10` for GIP. A write with the wrong page open is discarded without an
  error, and the initialisation "succeeds" while the panel runs on its power-on defaults.
- **Inversion stays on.** The factory table ends INVON, SLPOUT, DISPON, and the application then
  sends INVON and DISPON a second time. Under Espressif's table this firmware had to send INVOFF
  afterwards or every colour arrived as its own complement; under the factory's table the colours
  are correct with inversion left on.

Three things this settled along the way:

**The init timings were why nothing reproduced.** They sat at the datasheet minimum — a 1 ms
reset pulse and 120 ms after it — and the panel came up differently from boot to boot with
identical code, which is an expensive thing to debug: every change appears to do something.
Twenty times the required pulse, 200 ms after it and 150 ms to settle, and four power cycles
produce the same picture four times.

**DMA replaces the 64-byte FIFO.** A screen is 12 transfers instead of 4050, and the noise drops
visibly. This section used to claim that anything larger than one row put nothing on the panel and
reported no error. That was wrong, and the way it was measured is why: one constant, `SPI_CHUNK`,
sized the transfer, the DMA buffer and the staging array together, so raising it changed three
things at once and the result could not speak about any of them. Separated — buffer and staging
held at the largest size, only the transfer length varying — every size tried arrives.
`firmware/src/bin/chunktest.rs` is the measurement: it paints six bands of the panel, each with a
different transfer size, so one look at the glass reads out all six results at once. 720 bytes
through 21600 all painted their band, the 4092-byte DMA descriptor boundary included. The upper
bound is unknown; 21600 is the largest tried, not the largest that works.

One trail is recorded because it looked convincing and was wrong. The `st77916` crate documents
RAMWR for the first chunk of pixels and RAMWRC for later ones, which reads like a wire protocol
but describes chunks at the driver level. With CS held down the pixel stream is a single
transaction and bare continuation data is correct; framing every chunk as its own command ended
the write after the first one and put nothing on the screen.

**The panel is mounted upside down.** MADCTL (36h) is prepended to the table, and the driver's
default of `0x00` puts row 0 at the bottom of the glass and column 0 on the right: what is drawn
first arrives last, turned by 180 degrees. Stripes cannot show that on their own -- a full-width
band looks identical mirrored in X -- so `firmware/src/bin/orientation.rs` draws a letter F, which is
asymmetric in both axes, with a red square marking the corner the controller calls (0,0). With
`0xC0`, both mirror bits set, the F reads upright and the right way round with the USB socket
pointing away from the viewer, and `teetotum/src/panel.rs` now sends that as `PANEL_MOUNT_MADCTL`. Worth
knowing for the next panel: the controller's RAM is 360x390 against 360 rows of glass, so
mirroring in Y can shift the visible window by the difference. Here it does not.

That constant describes how the glass is fitted, and a board that mounts the same panel the other
way up changes it there. It is **not** where a viewing orientation belongs. MADCTL can only
express the eight combinations of mirror-X, mirror-Y and axis exchange -- 0, 90, 180, 270 degrees
plus mirrors -- so an angle the user picks freely cannot come from the controller at all. It is
a rotation in the rendering layer on top of the mount correction, chosen on the device and kept
in the settings.

**Neither a host refresh nor TE is needed.** The ST77916 carries a full frame memory and scans
the glass out of it on its own oscillator, so pixels are written once and stay. The tearing-effect
line is a convenience for avoiding tearing, and on this board it is not wired to the ESP32-S3 at
all — the net exists on the display connector and ends there. Advice for the ST77903, a RAM-less
controller where the host really must stream every frame or the image dies, does not apply here
and cost time before the schematic settled it.

**Reading a register back** has never worked and now has a documented recipe that was never tried:
`0x03` is not an ST77916 opcode at all, it is the SPI-NOR read opcode. The datasheet asks for
`0x0B`, a 24-bit address `00 <cmd> 00`, **8 dummy bits and everything on one line** — only D0 is
bidirectional on this controller, D1–D3 are inputs, so a four-line answer is impossible. Reads of
the manufacturer registers are additionally gated behind `0xF4` (SPIOR), which toggles them on and
off. The factory firmware never reads the panel at all, so it offered no template.

## A frame: what it costs to build one and show it

Measured on 2026-09-07 with `firmware/src/bin/render.rs`, which assembles one scene in a framebuffer and
times every part of getting it onto the glass.

The picture is a 360x360 RGB565 framebuffer — **253 KiB** — and it lives in the external PSRAM,
because the internal SRAM is 512 KiB and the Wi-Fi and Bluetooth stacks already have a large
part of it. `teetotum/src/framebuffer.rs` is that buffer as an `embedded-graphics` draw target; it stores
its pixels **big-endian**, which is the order the panel wants, so the drawing swaps two bytes
and the blit swaps none.

**The PSRAM is 8 MB and Octal.** The mode was read out of the factory image before it was tried:
that image carries `octal_psram`, the log tag of ESP-IDF's octal implementation, and not
`quad_psram`. The chip names itself as AP Memory, 64 Mbit, 3 V, 32-byte burst.

### The panel takes 80 MHz, and that is the largest lever in this project

Every run before this used 10 MHz because it was the first value that worked. All four clocks
below were sent and all four looked clean on the glass — checked by eye, because a corrupt
transfer shows as stripes and nothing in the log would say so.

| clock | per frame | frames per second | 259200 bytes over 4 lines | difference |
|---|---|---|---|---|
| 10 MHz | 59.80 ms | 16 | 51.84 ms | 7.96 ms |
| 20 MHz | 33.80 ms | 29 | 25.92 ms | 7.88 ms |
| 40 MHz | 20.84 ms | 47 | 12.96 ms | 7.88 ms |
| 80 MHz | 14.36 ms | 69 | 6.48 ms | 7.88 ms |

The difference is the same at every clock, and that makes it a constant rather than a
coincidence: it is the CPU copying the framebuffer out of PSRAM into the SPI bus's own DMA
buffer. **Reading PSRAM costs 7.9 ms per screen, writing it 13.2 ms** — 32 MB/s and 19 MB/s.
At 80 MHz the copy is more than half the frame time.

#### The copy cannot be removed, because the PSRAM cannot feed this bus

Giving the DMA the PSRAM address directly is 6.6 ms a frame instead of 14.4 — and it puts thick
bands of one colour on the glass. An SPI transfer does not wait for its DMA: once the
transaction is running the clock runs, and a dry transmit FIFO sends whatever stood in it last.
At 80 MHz over four lines the bus takes **40 MB/s** and the PSRAM gives about **32**.

Measured on 2026-09-12 with `firmware/src/bin/psramdma.rs`, by holding the path at `Path::Direct`
and swapping the *clock* every two seconds: **clean at 40 MHz, striped at 80.** Neither the
address of the picture, the descriptor alignment, the burst size nor the cache writeback has
anything to do with it — all of those were tried at 80 MHz, where it fails whatever they say,
and `firmware/src/bin/psramreach.rs` showed with a `Mem2Mem` sweep that the DMA reads every
address of the 8 MB window correctly when nothing is waiting for the data.

At 40 MHz the direct path is 13.0 ms of bus with no copy, against 14.4 ms copying at 80 MHz:
eight per cent, for half the clock. So `Screen` sends the copying way, and the 7.9 ms is the
price of a memory that cannot keep up with this bus.

The write figure is not a matter of instruction count. Clearing the screen with two byte stores
per pixel took 13.47 ms; the same loop writing 32-bit words, a quarter of the stores, took
13.19 ms. The cache fills and writes back lines either way.

### Turning the picture costs more than sending it

The panel controller turns a picture in quarters only — MADCTL has three geometry bits — and the
glass is round, so any angle would be usable. `teetotum/src/rotate.rs` supplies the eight angles in
between by turning the picture band by band into a staging buffer on its way out, at 80 MHz:

| case | turning alone | turned and sent | frames per second |
|---|---|---|---|
| not turned | — | 14.4 ms | 69 |
| 90 degrees, nearest | 38.5 ms | 45.6 ms | 21 |
| 30 degrees, nearest | 34.8 ms | 42.1 ms | 23 |
| 30 degrees, bilinear | 138.2 ms | 145.4 ms | 6 |

Per pixel that is about **65 cycles** for nearest and **256** for bilinear at 240 MHz — bilinear
is four times nearest, the number of samples, so the interpolation is free next to the reads.

Sixty-five cycles to move one pixel is a lot, and the reason is that **the rotation is
memory-bound**: a turned output row walks the source diagonally, so nearly every sample pulls a
fresh 32-byte PSRAM cache line to use two bytes of it. Which is why exact arithmetic does not
help — 90 degrees is the *slowest* nearest case, because consecutive output pixels step down a
source column and miss every time, while at 30 degrees they still mostly walk along a source
row. And the case that costs the most is the one nobody has to pay: 0, 90, 180 and 270 degrees
are three bits in the controller.

The direction was settled the only way a direction can be. A full turn in twelve steps ended on
the eleventh, and the mark that starts at twelve o'clock stood at eleven, so `rotate_rows(step)`
turns the picture **clockwise** by `step * 30` degrees. The module comment had said anticlockwise,
from the sign of the matrix on paper.

### Nearest wins, and darker is not a bug

`firmware/src/bin/turn.rs` puts the two filters under one finger: the knob turns the picture, a tap swaps
the filter, and the name of the running filter is drawn *into* the picture, so the word is
rendered by the filter it names. Judged that way the answer was immediate — **nearest is clearly
better at every angle that is not a multiple of 90 degrees**, and bilinear is not merely softer
but visibly darker.

The darkness is the filter doing what it is defined to do. The scene is one-pixel white strokes
on black. A stroke falling between two output pixels is given to both at half strength: it stays
continuous, which is the point of bilinear, and it goes grey. Nearest gives one pixel the whole
stroke and drops the other — brighter, and broken. For thin strokes and small text, which is
what an interface is made of, brighter and broken reads better than dim and whole.

At multiples of 90 degrees the two are identical by construction — every output pixel lands
exactly on a source pixel — so tapping there changes nothing, which is a free check that the
arithmetic sits on the grid.

So the default is nearest, and it is also the cheap one: 42 ms against 145, 23 frames a second
against 7. Bilinear stays in the module, because the judgement was about this content. A
photograph has no one-pixel strokes to smear, and cover art resampled with nearest is exactly
the case bilinear exists for. **The filter belongs to what is being drawn, not to the device.**

### The quarters are free, and the glass confirmed the bits

Four of the twelve detents never need the arithmetic: 0, 90, 180 and 270 degrees are mirror-X,
mirror-Y and exchange-axes in MADCTL (36h), so they cost one register write. Which value belongs
to which quarter follows from how the controller maps the pixel stream onto the panel — writing
`(i, j)` for a pixel's place in the stream and `(px, py)` for where it lands:

```text
MV clear:  px = MX ? W-1-i : i      py = MY ? H-1-j : j
MV set:    px = MX ? W-1-j : j      py = MY ? H-1-i : i
```

The glass is fitted upside down, so the viewer's frame is the panel's turned by 180 — that is
`PANEL_MOUNT_MADCTL`, and it is the entry for zero. A quarter turn clockwise wants the viewer to
see `(W-1-j, i)`, which on the panel is `(j, H-1-i)`: axes exchanged, Y mirrored, X not. So the
four values are `0xC0`, `0xA0`, `0x00`, `0x60`.

That is arithmetic on paper, and the last piece of arithmetic on paper here had the rotation
going the wrong way round. So `firmware/src/bin/turn.rs` can switch the free path off and draw the same
angle through `rotate_rows` instead: at a multiple of 90 degrees the two must be
indistinguishable. Checked on 2026-09-08 at 90 and 270 degrees — **the picture does not move**,
while the frame time in the log drops from 45.6 ms to 14.4. The switch is Enter in the monitor;
it was a double-tap first, and the glass never reported one.

### One screen, one place for the numbers

`teetotum/src/screen.rs` is the whole way from external RAM to the glass in one object: the PSRAM
framebuffer, the QSPI bus, the vendor initialisation sequence, the orientation and the blit that
suits it. A caller draws into `screen.frame()` with `embedded-graphics`, says how the device is
being held with `set_orientation`, and calls `present`.

The reason is not tidiness. Until 2026-09-08 that bring-up was copied into five binaries, and
copied code is survivable where **copied constants are not**: the 80 MHz above had been measured
and looked at on the glass, and the firmware in `firmware/src/bin/main.rs` was still driving the panel at
the 10 MHz that was never a decision, only the first value that worked. A number that has been
measured belongs in one place.


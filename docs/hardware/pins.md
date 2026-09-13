# Pins and the I²C bus

Which pin goes where on the ESP32-S3, and what answers on the one bus that carries more than
one device. Part of the [hardware documentation](README.md).

## Pin assignments

Waveshare does not publish these on the product page, and the first version of this table came
from a third party's ESPHome configuration for this board —
[KrX3D/WaveShare-Knob-Esp32S3](https://github.com/KrX3D/WaveShare-Knob-Esp32S3). Two of
Waveshare's own downloads do carry them, and were found later: the **schematic** and the **demo
sources** behind the [wiki page](https://www.waveshare.com/wiki/ESP32-S3-Knob-Touch-LCD-1.8).
They agree with each other and with every pin measured here, which is why the rows below now
include lines nothing on this board has yet been asked to confirm.

Treat the untested rows as hearsay until something proves them. The audio rows are the reason
that warning is not a formality: they are Waveshare's own numbers, they are almost certainly
right, and following them still produces silence — see [Audio](audio.md).

| Function | GPIO | Verified |
|---|---|---|
| Backlight | 47 | **yes — the panel lights up** |
| Display CS | 14 | **yes — pixels arrive bit-exact, see [Display](display.md)** |
| Display CLK | 13 | **yes** |
| Display D0–D3 | 15, 16, 17, 18 | **yes** |
| Display RESET | 21 | **yes — the reset pulse length changes the outcome** |
| Touch INT | 9 | **yes — it pulses while a finger is down** |
| Touch RESET | 10 | no — the controller answers whether it is pulsed or not |
| Touch I²C SDA / SCL | 11 / 12 | **yes — two chips answer, and identify themselves** |
| Knob direction lines | 8 / 7 | **yes — but not the A/B they were labelled, see [Input](input.md)** |
| Haptic driver enable | 38 | **yes — the chip's own diagnostic turns from `0xE9` to `0xE0`**; the schematic also calls this pin the UART TX, and that turned out to be wrong |
| TF card CLK / CMD / DAT0 / DAT3 | 4 / 3 / 5 / 2 | **yes — the card answered on them itself** |
| TF card DAT1 / DAT2 | 6 / 42 | no — SPI mode never touches them; they carry the two remaining pull-ups |
| I²S to the DAC: BCK / WS / DIN | 39 / 40 / 41 | **no — and 39/40 are the UART to the other chip**, so this row is the schematic's, not the board's |
| PDM microphone CLK / DATA | 45 / 46 | no |
| DAC switch select | 0 | **no — driving it either way changes nothing audible** |
| UART to the classic ESP32, TX / RX | **40 / 39** | **yes — RX carries frames at 921600 baud**, and GPIO39 is held high from outside even when it is silent |
| Battery voltage divider | 1 | no |

Driving GPIO47 high was the cheapest possible test of that source: one pin, one line of code,
and an answer visible from across the room. It lit, which was not proof of the other rows but
did mean the list describes this board and not a different one.

The six display rows have since earned their marks the same way — by producing an effect that
only the right pin could produce. Two colours arriving as their exact bit complements (see
[Display](display.md)) cannot happen unless clock, chip select and all four data lines are correct, and the
length of the reset pulse changes whether the panel initialises at all.

The remaining four rows earned their marks on 2026-09-05 with `firmware/src/bin/probe.rs`, which scans
the I²C bus and then logs the touch controller and the knob. Every row held, though the last
one held only as a pin number: what is wired to GPIO8 and GPIO7 is not what "encoder A and B"
suggests.

## What is on the I²C bus

Scanning GPIO11 and GPIO12 as SDA and SCL — the wiring the pin table claimed — got two answers
on the first pass, so no swap was needed and the two bus rows are measured rather than copied.
Each device was then asked for an identifying register, which turns "something acknowledged"
into "this chip is here":

| Address | Register | Reads | Chip |
|---|---|---|---|
| `0x15` | `0xA7` chip id | `0xB6` | CST816**D** touch controller, firmware `0x01` |
| `0x5A` | `0x00` status | `0xE0` | DRV2605**L** haptic driver (device id 7 in the top bits) |

The haptic driver answers whether or not its output stage is switched on, which cost an
afternoon — see [Haptics](haptics.md).

Nothing else answers. The factory image's `cst816s` is the driver's name, not the chip's; the
register map is the same across the family, which is why the factory demo works anyway.

A finger on the glass produces coordinates and pulls GPIO9 low, so the touch interrupt row is
real. GPIO10 is a different matter, and the table above has been corrected: `probe` pulses it
before scanning, but a later run that never touched the pin at all read the identity and
contact registers just as well. So the controller does not need it, and nothing so far shows
that GPIO10 is the touch reset rather than an unconnected pin. It is still pulsed at start-up,
because sixty milliseconds is a cheap way to reach a known state after a warm boot.


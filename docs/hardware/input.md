# Input: touch, the knob, and the second encoder

Three ways to touch this device, in three different frames of reference.
Part of the [hardware documentation](README.md).

## Touch: where the finger is, and in whose frame

The controller reports coordinates, but in its own frame, and this panel is mounted upside down
(`PANEL_MOUNT_MADCTL` is `0xC0`). `firmware/src/bin/touch.rs` settles the relation the only way that is
not an argument: it draws two landmarks in the picture — a white bar at twelve o'clock, an
orange one at three — and answers every touch with two markers, one at the raw coordinates and
one at `359 - x, 359 - y`. Whichever appears under the fingertip is the answer.

It is the turned one, and the log agrees with the eye:

| touched | reported | turned | where the landmark is drawn |
|---|---|---|---|
| white bar, twelve o'clock | 167,351 | 192,8 | x 174–186, y 10–44 |
| orange bar, three o'clock | 16,167 | 343,192 | x 316–350, y 174–186 |

**Touch reports in the panel's mounting frame.** The half turn that `PANEL_MOUNT_MADCTL`
applies to the pixels has to be applied to the coordinates too — the first concrete instance of
the rule the orientation question predicted, that the pixels are the cheap half of a rotation
and the coordinates are the work.

The gestures are turned the same way, and they are reported while the finger is still down, not
on release:

| swipe, as the viewer makes it | code | name in the mounting frame |
|---|---|---|
| left to right across the picture | `0x03` | slide left |
| right to left | `0x04` | slide right |
| top to bottom | `0x01` | slide up |
| bottom to top | `0x02` | slide down |

A tap is `0x05` and a double tap `0x0B`. The horizontal pair was counted over a run of
continuous swiping: 13 from the picture's left as `0x03`, 13 from the right as `0x04`, no
exceptions. That also settles which of the circulating CST816 datasheets to believe about
`0x01` and `0x02`, since they disagree. `0x0C` for a long press is the one name still taken on
trust.

The motion mask at register `0xEC` rests at `0x00` and is documented as the gate for slides —
it is not. Held at zero through thirty seconds of continuous swiping and then written to `0x07`
with the hand still going, it made no difference that could be seen: gestures arrived
throughout. Whether the double click needs it is untested, which is why the driver still writes
it.

The interrupt on GPIO9 pulses rather than staying asserted for the length of a touch, so
polling the six contact registers is the reliable read and the interrupt is at best a hint
about when to poll.

## The knob is not a quadrature encoder

The pin table calls GPIO8 and GPIO7 "encoder A and B", and decoding them as a quadrature pair
produces a count that steps one out and one back forever. A scan of every pin that was free to
listen on — GPIO0–10, 38–46 and 48 — found edges on those two and on nothing else, but never in
the same second: four seconds of GPIO8 alone, then four of GPIO7 alone.

Tracing the raw states with `firmware/src/bin/encoder.rs` settled it. Over 134 steps in both directions:

- `00` never occurs — the two lines are never low together.
- One direction is `11 → 01 → 11`, repeated: GPIO7 pulses low while GPIO8 stays high.
- The other is `11 → 10 → 11`: GPIO8 pulses low while GPIO7 stays high.

Two channels a quarter cycle apart cannot do that. These are **one pulse line per direction**: a
low pulse on one pin is a step one way, a pulse on the other is a step the other way. Something
turns the encoder's phases into direction before these pins reach the ESP32-S3 — plausibly the
second microcontroller on the board, though that is a guess and not a measurement.

The pulses are slow: 15–30 ms low, 40–60 ms high at a hand's turning speed, and only two
transitions in 134 pulses landed inside the same millisecond. So `teetotum/src/encoder.rs` counts
falling edges and debounces each line for 5 ms — measured against a turn that ran the count to
101 without a single step backwards. It catches each edge in the GPIO interrupt and adds it to a
counter that `Encoder::poll` collects: a polled pin is only as good as the slowest pass of the
loop that reads it, and one slow pass steps straight over a pulse.

Which pin is clockwise, and how many pulses make a revolution, are not things a pin can say —
they need the hand and the screen in one picture. `firmware/src/bin/knob.rs` draws a dot on a circle with
a fixed index mark at twelve o'clock and moves the dot with the knob:

- **GPIO8 is clockwise.** Turning the knob clockwise counts up on GPIO8, and the dot follows the
  finger rather than running away from it.
- **A revolution is 37 to 41 pulses, depending on speed.** With a marker dot on the knob and the
  index mark to return it to, runs over marked revolutions counted 203, 200 and 207 pulses turned
  slowly and 190 and 187 turned fast. Polling the pins instead counted exactly 30 a revolution,
  300 over ten; which of the two counts the true detents is open.

`PULSES_PER_REVOLUTION` in `teetotum/src/encoder.rs` holds 40, the slow end, and nothing in the
firmware depends on it. `knob.rs` takes its step from it, 9° per pulse, so after a full turn the
dot lands within a few pulses of the mark rather than exactly on it, falling further short the
faster the knob turns.

## The second rotary encoder can be borrowed from the other chip

The datasheet's second encoder is wired to the classic ESP32 -- `EC2_A` / `EC2_B` on its IO19 and
IO22 -- so nothing the ESP32-S3 does can read it directly. It does not have to: the other chip
will forward it, and asking costs one byte.

Read out of that chip's factory image and then measured on the wire:

- `iot_knob_create` is called with a four-byte `knob_config_t` constant in DROM,
  `default_direction = 0`, `gpio_encoder_a = 19`, `gpio_encoder_b = 22` -- the schematic's pins.
- Both of its callbacks load one state byte first and **return unless bit 0 is set**. Then they
  branch on **bits 1..3**: on `1` the turn goes into the chip's own event queue, on `2` it is sent
  over the link as **`BD 07`** (clockwise here) or **`BD 08`** (anticlockwise).
- That state byte is what the S3 writes with `A3 09`, and it comes back as `data[0]` of the `BD 05`
  status -- so a write can be checked rather than hoped for.

```
A3 09 with data[0] = 0b0000_0101   bit 0 set, page 2  ->  every detent arrives as BD 07 / BD 08
A3 09 with data[0] = 0b0000_0011   bit 0 set, page 1  ->  every detent changes the phone's volume
```

Measured with `firmware/src/bin/ec2.rs`: under `0x00`, turning for 52 seconds produced no frame at all;
under `0x05`, five detents one way produced five `BD 08` and nothing else, five the other way five
`BD 07` and nothing else; under `0x03` no frame arrived and the volume byte of the status fell from
26 to 6 while the knob turned.

**Which frame is which way round took a second measurement.** That run recorded `BD 08` under an
instruction to turn clockwise, and the direction went into this file that way; a deliberate
re-check with `firmware/src/bin/companion.rs`, three slow detents clockwise and the raw
command byte logged beside the name, produced **three `BD 07`**. A prompt cannot check the hand
that answers it, so **clockwise is `BD 07`**.

**One frame per detent** -- which also says the two encoders sit on the same shaft, since the S3's
own encoder counts those same detents one pulse at a time. The knob clicks twice per step to the
ear, and the two chips agree that it is one step.

**This is the general shape of what the second microcontroller is good for.** It owns the DAC, the
classic Bluetooth stack and the second encoder, and this project does not have to take any of them
away from it to use them.


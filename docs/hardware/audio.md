# Audio

The loudspeaker belongs to the other microcontroller, and the S3 cannot reach the DAC at all.
Part of the [hardware documentation](README.md).

## Audio: the loudspeaker belongs to the other microcontroller

The board has a **PCM5100A** stereo DAC and a 3.5 mm jack, and the ESP32-S3 can drive it over
plain I²S: BCK on GPIO39, WS on GPIO40, DIN on GPIO41, no master clock, because the DAC's `SCK`
pin is grounded and it runs its own PLL from the bit clock. `firmware/src/bin/audio.rs` configures exactly
that, plays four rising sine notes through it, and is heard by nobody.

The reason is on the schematic sheet the S3 does not appear on:

```
ESP32_IO32 ---------------- XSMT          (the DAC's soft mute, active low)
ESP32_IO25/26/27 ---------- the other side of the CH445P switch
ESP32_IO19 / ESP32_IO22 --- EC2_A / EC2_B (a second encoder, not ours)
ESP32_IO23 / ESP32_IO18 --- the UART to the S3; its own two pins are right, ours are not
```

**`XSMT`, the mute, is wired to the classic ESP32 and to nothing else.** The S3 can clock a
perfectly good signal into a DAC that is holding its outputs quiet, and nothing in the S3's world
reports it. That is the whole of the first symptom.

A **CH445P** analogue switch decides which of the two microcontrollers reaches the DAC's three
data lines, and the schematic gives its select input as the S3's **GPIO0**; Waveshare's own
`audio_bsp.c` drives that pin high before anything else, commented "give control of the PCM5100A
to the ESP32-S3". On this board it does nothing. With Bluetooth music from a phone playing out of
the jack — which proves jack, DAC, analogue supply and mute all work when the other chip drives
them — GPIO0 was held either way for eight and twenty seconds at a time without disturbing a note
of it.

So `firmware/src/bin/switchhunt.rs` asked the board instead, the way the haptic enable was found: every
remaining free pin driven low and high in turn, in a rhythm slow enough to hear, with a steady
441 Hz note on our own I²S pins. **GPIO1, GPIO43, GPIO44 and GPIO45 change nothing.** Three pins
are still untested and each for a reason: GPIO48 and GPIO46 were taken for outputs of the other
chip and of the microphone, and driving them back would be two outputs on one wire; GPIO38 was
needed to hold the haptics enabled. GPIO48 has since been measured as floating and is an output
of nothing — the pin that is an output of the other chip is **GPIO39**, which this hunt drove as
an I²S bit clock. Nothing broke, and nothing should drive it again.

The conclusion is a fact about the architecture and not about a pin:

> **Audio is not the S3's to take.** The mute belongs to the classic ESP32, and no line the S3 can
> reach moves the switch. Sound from our own firmware needs the second microcontroller's
> cooperation, over the UART the factory firmware's `UART1` task already uses. That link has
> since been read out of both firmware images and then heard on the wire: it is **GPIO40 and
> GPIO39**, not the pins the schematic names.

Two smaller things fell out of the same investigation. The microphone is an **MSM261D4030H1CPM**, a
PDM part with its `L/R` pin grounded, on GPIO45 and GPIO46 — it is an *input*, and inputs may
well be ours alone. And the second of the datasheet's "two rotary encoders" is real: it is wired
to the classic ESP32, which is why only one of them has ever answered here — see [Input](input.md), which is how it answers anyway.

### The conflict on GPIO38, and why it was never one

The same schematic shows `HAPTIC_EN` tied to **3V3** and GPIO38 as `ESP32S3_TX`, the UART line to
the classic ESP32. This project measured the opposite: holding GPIO38 high is what turns the
DRV2605L's diagnostic from `0xE9` to `0xE0`. The two readings looked mutually exclusive, and with
the way to the loudspeaker running over that same UART, one of them had to go before a byte could
be sent.

`firmware/src/bin/pin38.rs` put all the states into a single boot, twice, and the two runs agree line for
line:

| state of GPIO38 | actuator diagnostic |
|---|---|
| never configured by this firmware | `0xE9`, `0xE9`, `0xE9` |
| input, internal pull-down / pull-up | follows the pull, so nothing on the board drives it |
| driven high | `0xE0`, `0xE0`, `0xE0` |
| driven low again | `0xE9`, `0xE9`, `0xE9` |
| driven high once more | `0xE0`, `0xE0`, `0xE0` |
| handed to UART1 as TX, line idle | `0xE0`, `0xE0`, `0xE0` |
| UART1 TX sending `0x00` without a gap | `0xE9`, `0xE0`, `0xE0` |

So the schematic is wrong about the 3V3 and right about the UART, and **both uses of the pin fit
on it at once** — because a UART transmit line idles high, and high is the enabled state. The
enable is a level and not an edge: dropping the line brings `0xE9` straight back, immediately and
every time.

What it costs is small and real. Under a solid stream of `0x00` — the worst pattern a UART can
produce, nine of every ten bit times low — **the clicks are shorter and a little quieter to the
hand**, and one diagnostic in three comes back `0xE9`. Nothing in a real conversation looks like
that: a command of a handful of bytes holds the line down for well under a millisecond against a
click of tens of milliseconds. The rule that follows is worth keeping anyway: **do not stream to
the other chip while an effect is playing**, and expect a weaker click if you do.

The conflict dissolved a second time, and more cheaply: **GPIO38 is not the UART line at all.**
Reading both factory firmwares showed the link on GPIO40 and GPIO39, and a pull probe then found
GPIO38 floating, driven by nothing and connected to nothing that talks. So the pin is the haptic
enable and only that, the measurements above still stand as measurements, and the rule about not
streaming during a click is now a rule about a pin that will never stream.


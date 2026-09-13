# Haptics

An LRA behind an enable pin that no pin list mentions, and one register write that must never
be sent. Part of the [hardware documentation](README.md).

## Haptics: an LRA at 161 Hz, behind an enable pin nobody lists

The DRV2605L answers on the bus from the first scan, and that is what makes this one expensive
to find: **with its enable line low the chip is fully conversational and completely deaf.** It
reports its device id, accepts writes, and runs its own actuator diagnostic — which comes back
`0xE9`, "open or shorted", because from behind a disabled output stage an attached motor looks
exactly like no motor at all. Every drive setting was tried against that: ERM and LRA, closed
loop and open loop, ROM effects and raw amplitude, up to full clamp. Nothing could be felt.

The pin is not in the ESPHome list, so it was found by asking the chip. Each free pin was held
high in turn and the diagnostic run again; fourteen of them left it at `0xE9` and **GPIO38**
turned it to `0xE0`, with a buzz from the case at the same moment. Held high, everything else
follows:

| what | with GPIO38 low | with GPIO38 high |
|---|---|---|
| actuator diagnostic | `0xE9`, open or shorted | `0xE0`, connected |
| ERM calibration | compensation `0x0c`, back-EMF `0x6c` — the defaults, unchanged | `0x1a` / `0x20` |
| LRA calibration | `0x0c` / `0x6c`, unchanged | `0x15` / `0x78` |
| resonance period | 21571 µs = 46 Hz, i.e. the top of the range | **6205 µs = 161 Hz** |
| supply, as the chip measures it | 5355 mV | 3171 mV |
| anything felt | no | yes |

So **the actuator is a linear resonant actuator resonating at about 161 Hz**, which is where
small LRAs live, and the two readings that looked like hardware facts before — 46 Hz and a
supply above the chip's operating range — were both artefacts of the disabled output stage.

### One write that must not be sent

`MODE = 0x80`, the DRV2605L's device reset, is refused on this board: the write comes back as a
NACK on the data byte, which is what a chip resetting itself mid-transfer looks like. That much
would be harmless. What is not harmless is the state it leaves behind — the chip stops
acknowledging `0x5A` and answers at `0x40` instead, reporting device id 1, with the register
block from `0x16` to `0x1D` reading zeros. It is unmistakably the same chip: the rest of the
register map still carries the DRV2605L's defaults exactly (`0x11 = 0x05`, `0x12/0x13 =
0x19/0xFF`, `0x1E = 0x20`, `0x1F = 0x80`, `0x20 = 0x33`).

A fresh boot brings it back to `0x5A`. Sixteen recovery clocks on SCL followed by a stop
condition do so sometimes and not reliably. `Haptic::reset` is kept in the driver, documented
as harmful here; the registers are configured by hand instead.


# The link between the two microcontrollers

One UART, two magic bytes, and the command vocabulary that lets this firmware drive the phone
without speaking Bluetooth itself. Part of the [hardware documentation](README.md).

## The link between the two microcontrollers

Everything the S3 cannot do alone — the loudspeaker, the second encoder, classic Bluetooth — goes
through one serial link, and this project spent a day looking for it on the wrong two pins.
Reading it out of the two factory images in `backup/` settled it in an afternoon, because **both
ends of the link were on disk**: the S3's `src/driver/uart1.cpp` and the classic ESP32's
`../main/uart1.c` describe the same protocol from opposite sides, and they agree.

| | ESP32-S3 | classic ESP32 |
|---|---|---|
| port | `UART_NUM_1` | `UART_NUM_1` |
| baud, frame | **921600**, 8N1, no flow control | **921600**, 8N1, no flow control |
| TX / RX | **GPIO40 / GPIO39** | IO23 / IO18 |
| cover-art buffer | 48 KiB in PSRAM | 48 KiB |

**The schematic says GPIO38 and GPIO48, and the board says otherwise.** The pull scan had already
recorded the answer without anyone reading it that way: GPIO39 was "held high by something nobody
has named", which is what the other chip's idle transmit line looks like from here, and GPIO40 was
floating, which is what our own unconfigured TX looks like. GPIO38 and GPIO48 float. The S3's
factory firmware also never sets up an I²S *transmitter* — only the PDM microphone — so it never
wanted GPIO39/40 for the DAC either.

### The frame, and what came over it

```
 0   magic   0xA3  S3 -> classic ESP32       0xBD  classic ESP32 -> S3
 1   cmd     u8
 2   len     u16 little endian, bytes following the header
 4   data    len bytes
```

`firmware/src/bin/uartframes.rs` drives nothing: it probes which of the four candidate pins are held from
outside, then listens on GPIO39 at 921600 and reassembles frames. Changing the track on a phone
paired to the *other* chip produced this, with **no framing errors and no dropped bytes**:

```
ESP32 -> S3   cmd 6 (metadata text), 50 bytes:
    11 0E 0F 00  "<title: 16 characters>\0<artist: 13>\0<album: 14>\0"
```

The four bytes before the text are the lengths of the strings that follow, terminators included —
`0x11 = 17` for the title, `0x0E = 14` for the artist, `0x0F = 15` for the album, and a
fourth slot left empty. Three frames from three different tracks add up the same way; the strings themselves are not reproduced here.

The rest of the vocabulary is read out of the images rather than heard: album art moves in packets
of 1016 bytes that **the S3 asks for one at a time**, `A3 04` carries USB HID consumer codes
(`0xB5` next track, `0xB6` previous, `0xCD` play/pause), `A3 08` asks for status and is answered
with `BD 05`.

### Talking back: which command reaches the phone, and why two of them did not

Sending works, and the other chip obeys. `A3 08` is answered with a `BD 05` immediately, `A3 09`
writes the state byte that decides who gets the second encoder, and a transport command changes
the track on the phone — watched happening on 2026-09-10, with the proof arriving on the same
wire: when the track changes, the other chip announces the new one unprompted as a `BD 06`, so
the run needs no look at the phone to know it worked.

**Which command, though, took three sessions to settle, and the answer is that the chip has two
different radios for what looks like one job.**

* **`A3 03` is the one that works.** Its value is handed to `vcsTask` (`0x400dbdf8`) — not through
  a queue, as first read, but as `xTaskNotify(..., eSetValueWithOverwrite)` — and that task turns
  it into an AVRCP passthrough key on the link the phone is already holding: **3** is `0x4B`
  forward, **4** is `0x4C` backward, **6** is `0x45` stop, each sent as a press and a release two
  ticks apart. None of the three sits behind a test of any kind. **1** and **2** are the phone's
  volume and are guarded.
* **`A3 04` is not.** It carries USB HID consumer codes (`0xB5` next, `0xB6` previous, `0xCD`
  play/pause), and they leave the chip over **BLE HID** — the task draining that queue is named in
  the image, `ble_hid_task` at `0x400da514`. They reach a phone only while something is paired
  with the chip's own HID device, and nothing was: `TAIJI_KNOB_HID` advertised in all twelve BLE
  scans of the run that found this.
* **`A3 03` with 5 is play/pause, and it is the one value that can be swallowed.** `0x400dbdb8`
  reads the AVRCP playback status and sends pause when it is 1, play when it is 0 or 2, and
  **nothing at all** for any other value — no key, no log line.

Two things worth keeping from that. **A command that does nothing and a command that is never sent
look identical from this side**, so the branch that decides the sending has to be read, not
guessed. And **`eSetValueWithOverwrite` holds exactly one pending value**: two commands closer
together than the task's next wakeup, and the first one is gone.


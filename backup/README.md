# Factory firmware backups

The board carries **two microcontrollers**, and each has its own SPI flash. This folder holds a
complete image of both, and neither is a partial dump: every byte of both flashes is here.

| Chip | Flash | Image | Read on |
|---|---|---|---|
| ESP32-S3 | 16 MB | `factory-flash-esp32s3-fc012cxxxxd8-2026-09-04.bin` | 2026-09-04, before anything of our own was written |
| ESP32 (classic) | 4 MB | `factory-flash-esp32-d4d4daxxxxe4-2026-09-06.bin` | 2026-09-06, still factory-fresh |

The images are not in Git (`backup/*.bin` in `.gitignore`); this file describes them so that a
checkout without them still says what they were.

Which chip is reachable over USB depends on **which way round the Type-C plug is inserted**: the
S3 appears natively as `303a:1001` on `/dev/ttyACM0`, the classic ESP32 through a CH340 bridge on
`/dev/ttyUSB0`.

## The ESP32-S3, 16 MB

A complete image of the SPI flash, read on **2026-09-04** from the Knob Display's ESP32-S3,
before anything of our own was written to it.

```
File     factory-flash-esp32s3-fc012cxxxxd8-2026-09-04.bin   (not in Git, see .gitignore)
Size     16777216 bytes (16 MB, the entire flash)
SHA-256  6294b3a73073c1e984a8f67aea24c390962b7ea73511a7cb1a74d5a848f54061
Chip     esp32s3 revision v0.2, 40 MHz crystal, MAC fc:01:2c:xx:xx:d8
         Secure Boot: off · Flash encryption: off → the image is plaintext and can be
         written back unchanged
```

Read with:

```
espflash read-flash --port /dev/ttyACM0 -B 921600 0 0x1000000 <file>
```

The baud rate is not a detail: at the default, reading 16 MB aborted twice with
`Timeout while running command`, and a single megabyte took 93 s. With `-B 921600` it takes 14 s
per megabyte, so a little over four minutes for the whole flash.

### What is in it

Partition table at `0x8000`, read back out of the image:

| Label | Type | Offset | Size | Contents |
|---|---|---|---|---|
| `nvs` | data | `0x009000` | 20 KB | written |
| `otadata` | data | `0x00e000` | 8 KB | written |
| `app0` | app (ota_0) | `0x010000` | 3 MB | **the demo firmware**, ESP image (`0xe9`) |
| `app1` | app (ota_1) | `0x310000` | 3 MB | erased (`0xff`) |
| `spiffs` | data | `0x610000` | 9.875 MB | erased (`0xff`) |
| `coredump` | data | `0xff0000` | 64 KB | written |

**The SPIFFS partition is empty.** The demo's MJPEG files therefore do not live in flash but on
the TF card — consistent with the startup message `[IO] Play: /night7/boot.mjpeg`. This image
thus contains the complete firmware but no media; the card is untouched by flashing anyway.

### Restoring — done, and it works

**Tried on 2026-09-06: the factory firmware came back and ran.** The boot log ended in
`[I] Boot finished.`, the demo started `/night7/boot.mjpeg` from the TF card, and every task the
original log names — LVGL, IO, WiFi AP, FFT, UART1, haptics, the rotary knob — started again.

The obvious command is **not** the one that works:

```
espflash write-bin --port /dev/ttyACM0 -B 921600 0 <file>     # times out
```

Sixteen megabytes in one command dies with `Timeout while running FlashDeflData command`, before
a byte reaches the flash. Two things make it easy instead:

- **Only 2 MB of this image is data.** Everything outside `0x000000..0x1f0000` and the 64 KiB of
  `coredump` at `0xff0000` is erased `0xFF`, so those two slices are the whole restore.
- **A written sector does not need a prior chip erase**; `write-bin` erases what it writes, and
  nothing this project ever flashed reached beyond `0x1f0000`.

```
dd if=<file> of=part-000000.bin bs=64K count=31
dd if=<file> of=part-ff0000.bin bs=64K skip=255 count=1
espflash write-bin --port /dev/ttyACM0 -B 921600 0        part-000000.bin
espflash write-bin --port /dev/ttyACM0 -B 921600 0xff0000 part-ff0000.bin
espflash monitor
```

Both writes take seconds. The boot log must start with `ESP-ROM:esp32s3-20210327` and end in
`[I] Boot finished.`

Should the flash ever hold something that reaches past `0x1f0000`, erase it first —
`espflash erase-flash` — or the region between the slices keeps whatever was there.

### The device no longer matches this image

On 2026-09-04 the S3 was flashed with the project firmware. That replaced the factory partition
table above with the one espflash writes by default, read out of the boot log:

| Label | Type | Offset | Size |
|---|---|---|---|
| `nvs` | WiFi data | `0x009000` | 24 KB |
| `phy_init` | RF data | `0x00f000` | 4 KB |
| `factory` | factory app | `0x010000` | 15.625 MB |

So `otadata`, `app1`, `spiffs` and `coredump` are no longer described by the table on the
device, and the demo firmware that sat in `app0` has been overwritten — the new application
occupies the same offset. Nothing was lost that is not in this image, and the TF card was never
involved. The restore path is unchanged: the full image goes back to offset 0 and brings its own
partition table with it.


## The classic ESP32, 4 MB

```
File     factory-flash-esp32-d4d4daxxxxe4-2026-09-06.bin   (not in Git, see .gitignore)
Size     4194304 bytes (4 MB, the entire flash)
SHA-256  c5a565f92a291dab7ce0b98aaaa66b9e928014cda7646c44431dbedca118e4d1
Chip     esp32 revision v3.0, 40 MHz crystal, MAC d4:d4:da:xx:xx:e4
         Embedded flash · Secure Boot: off · Flash encryption: off → plaintext, writable back
```

Read with:

```
espflash read-flash --port /dev/ttyUSB0 -B 921600 0 0x400000 <file>
```

One minute for the whole 4 MB. Note the **`/dev/ttyUSB0`**: this chip is only reachable with the
plug the other way round, which is also why this image is two days younger than the S3's.

**This chip has never been flashed by us, and this is the only copy of what it runs.** The S3's
factory firmware was overwritten on 2026-09-04 and its image is a way back; this one is a way
back that has not been needed yet, and it should stay that way until it has been.

### Read twice, and the difference is the point

The flash was read twice in a row and the two images compared. They are **not** identical — and
what differs says something worth keeping:

| Region | Offset | Result |
|---|---|---|
| bootloader | `0x000000` | identical |
| partition table | `0x008000` | identical |
| `nvs` | `0x009000` | **1985 bytes differ**, in 16 runs |
| `phy_init` | `0x00f000` | identical |
| `factory` | `0x010000` | identical, all 1600 KiB |
| `storage` | `0x1a0000` | identical |
| beyond the table | `0x300000` | identical |

Everything that is firmware read back byte for byte across two independent passes. The only
region that moved is `nvs`, which the chip rewrites itself: reading resets it, so it booted
between the two passes and left its own tracks. That is live data, not a read error — and it
means a restore of `nvs` puts back one particular moment of the device's memory, not a canonical
value.

### What is in it

Partition table at `0x8000`, read back out of the image:

| Label | Type | Offset | Size | Contents |
|---|---|---|---|---|
| `nvs` | data | `0x009000` | 24 KB | written, and moving (see above) |
| `phy_init` | data (RF) | `0x00f000` | 4 KB | written |
| `factory` | app | `0x010000` | 1600 KB | **the firmware**, ESP image (`0xe9`) |
| `storage` | data (FAT, `0x82`) | `0x1a0000` | 1408 KB | erased |

Nothing is written above `0x180000`: 1536 KiB of the 4096 KiB flash carry data, the rest is
erased. The `storage` FAT partition being empty puts this chip alongside the S3 — **neither of
the two holds the demo's media**, which is on the TF card.

The application names itself in its descriptor:

```
name   TAIJI_KNOB_32
ver    1
built  Apr 18 2025 09:24:52
idf    v5.4-727-g5cbd2a3877
```

and it contains the strings `TAIJI_KNOB_AUDIO` and `TAIJI_KNOB_HID` besides, plus Espressif's
`iot_knob` component. That is the other half of the device talking about itself, and it is the
starting point for addressing it over the serial link on GPIO38/48 rather than guessing.

### Restoring

```
espflash write-bin --port /dev/ttyUSB0 -B 921600 0 <file>
```

**Untested, and there is no second chance here.** Unlike the S3, this chip still runs its factory
firmware, so nothing has ever forced a restore. Whoever needs it first tests it first.

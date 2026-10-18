# The hardware, as measured

This is what the board turned out to be when it was asked directly. Every claim here was read
off the device — a ROM banner, a register, a logic level, a byte on a wire, a frame time — or
read out of the factory firmware image and then checked against the device. Where something is
still only a datasheet claim, it says so.

| Topic | What it covers |
|---|---|
| [Pins and the I²C bus](pins.md) | every pin measured, and the two chips that answer on I²C |
| [Display](display.md) | the ST77916 over QSPI, what a frame costs, orientation, scaling |
| [Input](input.md) | touch in the mounting frame, the knob, the encoder on the other chip |
| [Haptics](haptics.md) | an LRA at 161 Hz, an undocumented enable pin, one forbidden write |
| [Audio](audio.md) | why the loudspeaker is out of the S3's reach |
| [The companion link](companion-link.md) | the UART between the two chips, command by command |
| [The TF card](storage.md) | read rates at three clocks, and the demo's background format |
| [Wi-Fi and Bluetooth](radio.md) | what the radio costs, and what the air carries |

## The board has two microcontrollers

This is the single most important thing to know about it, and it costs you half an hour the
first time:

| | ESP32-S3 | ESP32 (classic) |
|---|---|---|
| Purpose | display, touch, rotary encoders, LVGL | Classic Bluetooth (A2DP/AVRC), audio |
| USB | native USB-Serial-JTAG, VID:PID `303a:1001` | CH340 bridge, VID:PID `1a86:7523` |
| Port | `/dev/ttyACM0` | `/dev/ttyUSB0` |
| MAC | `fc:01:2c:xx:xx:d8` | `d4:d4:da:xx:xx:e4` |

MAC addresses in this documentation keep the vendor prefix and the last byte; the two bytes in
between are masked. That is enough to follow how the S3 derives its other addresses from the base.

**Which of the two appears on USB depends on which way round the Type-C plug is inserted.**
Flipping the plug by 180° switches between "S3 USB" and "ESP32 UART".
If `lsusb` shows a CH340 instead of `303a:`, the plug is the wrong way round.

Stable device paths, independent of enumeration order:

```
/dev/serial/by-id/usb-Espressif_USB_JTAG_serial_debug_unit_FC:01:2C:XX:XX:D8-if00   # S3
/dev/serial/by-id/usb-1a86_USB_Serial-if00-port0                                    # ESP32
```

## Measured, not copied from the datasheet

Read directly off the device on 2026-09-04:

- S3 ROM banner `ESP-ROM:esp32s3-20210327`, chip magic `0x40001000 = 0x00000009` → ESP32-S3,
  revision v0.2, 40 MHz crystal, 16 MB flash. Secure Boot and flash encryption are both off.
- ESP32 ROM banner `ets Jul 29 2019 12:21:46`, chip magic `0x00f01d83` → classic ESP32.
- The S3's factory firmware reports on startup: LVGL, ~5.7 MB of PSRAM in use, a 3.6 MB MJPEG
  cache, an `SDSC 480MB` card, and tasks for LVGL, IO, WiFi AP, FFT, UART1, haptics and the
  rotary encoder.

The factory firmware in `backup/` names its own drivers, which confirms several datasheet claims
from the device rather than the data sheet. Strings in the image include
`esp_lcd_new_panel_st77916`, `ESP_PanelBus_QSPI`, `esp_lcd_touch_new_i2c_cst816s`, a DRV2605
error message, and the path `/lib/ESP32_Display_Panel-0.2.2/src/lcd/base/esp_lcd_st77916.c`. So
the panel is an **ST77916 driven over QSPI** — not plain SPI — with a CST816-family touch
controller on I²C and a **DRV2605** for haptics. The product name inside the image is "TAIJI
KNOB".

Both I²C chips have since answered for themselves (see [Pins and the I²C bus](pins.md)), and one
of them corrects the image: the firmware calls `esp_lcd_touch_new_i2c_cst816s`, but the silicon
reports **CST816D**.

Still from the datasheet and **not** verified here: 1.8" IPS 360×360, PCM5100A DAC, microphone,
3.5 mm jack, microSD, 16 MB flash, 8 MB PSRAM. The datasheet's "two rotary encoders" is not
what the pins show — see [Input](input.md). The knob does not press: there is no button under it.


## First bring-up

Flashed and verified on 2026-09-04. The boot log starts at `ESP-ROM:esp32s3-20210327`, the
ESP-IDF second stage bootloader loads five segments from `factory` at `0x10000`, and then:

```
I (231) boot: Loaded app from partition at offset 0x10000
INFO - Embassy initialized!
INFO - Hello world!
```

Both radios initialise: `esp_radio::wifi::new()` and the TrouBLE host are constructed before
that loop is reached, so its first line is the proof they returned. Over 40 seconds of monitoring
the device logged 37 lines with **no reboot, no panic and no watchdog reset**.


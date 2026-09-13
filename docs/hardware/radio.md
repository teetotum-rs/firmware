# Wi-Fi and Bluetooth

What the radio stack costs before a single connection exists, and what the air actually carries.
Part of the [hardware documentation](README.md).

## What the radio costs

Measured on 2026-09-04 by building both variants, `xtensa-esp32s3-elf-size` on the ELF and
`cargo espflash save-image` for the image:

| | without radio | Wi-Fi + BLE | delta |
|---|---:|---:|---:|
| `.text` + `.data` | 61,429 B | 530,221 B | ×8.6 |
| flash image | 98,768 B | 530,336 B | ×5.4 |
| RAM (all NOBITS sections) | 477,008 B | 533,656 B | +56,648 B |

The image is larger than `.text` + `.data` because of the padding between segments, and by a
different amount in each build — worth measuring rather than deriving.

On the device the application occupies **3.24 % of its 15.6 MB partition**. Flash is not the
constraint. RAM is the one to watch: `.rwtext.wifi` (49,544 B) and `.rodata.wifi` (28,244 B) are
the radio stack claiming internal memory outright, before a single connection exists, and the
64 KiB coexistence heap comes on top of that.

That heap is not optional: enabling BLE alongside Wi-Fi turns on esp-radio's `coex` feature, and
coexistence wants more heap than the reclaimed region has, so the firmware adds a second 64 KiB
allocator on top of it.

### The factory firmware's own network

Measured on 2026-09-04, before the S3 was flashed: the factory demo brings up an access point of
its own, SSID `My-Ap`, BSSID `fe:01:2c:xx:xx:d8` — the SoftAP address derived from the S3's base
MAC. WPA2, password unknown. This firmware brings up no access point.


## What the radios actually see

Both stacks scan on a timer and log what they find. Measured on 2026-09-04.

**Wi-Fi.** The scan finds two networks, both on channel 1, at −75 to −81 dBm. A laptop in the
same room sees eight networks on 2.4 GHz — the rest of its list is on 5 GHz, which the S3 cannot
reach at all. The two the board does find are exactly the two strongest on the laptop; everything
weaker is missing.

Two things were measured rather than guessed about that gap:

- The default dwell time of 10–20 ms per channel found only *one* network. Beacons arrive roughly
  every 100 ms, so a channel is often left before anything on it has spoken. At 100/300 ms the
  second one appears. That is why the scan is configured explicitly rather than left at
  `ScanConfig::default()`.
- A passive scan finds exactly the same two. Were only our probe requests too weak to reach the
  access points, passive listening would have found more — it only listens. Both directions are
  attenuated equally, so the limit is the antenna path, not transmit power.

**BLE.** A five second window finds five advertisers between −71 and −101 dBm, all with random
private addresses and none carrying a name. That the BLE receiver reports −101 dBm while Wi-Fi
tops out around −81 dBm is not a contradiction: BLE is the more sensitive of the two by roughly
that margin.

**Scanning cannot find a phone by name, and that is not a bug.** Opening the Bluetooth settings
screen makes an Android phone discoverable over *classic* Bluetooth — which this chip cannot
receive at all; that radio sits on the board's second microcontroller. Over BLE a phone
advertises to strangers with a rotating random address and no name, deliberately.

A phone can still be identified, just by behaviour rather than by name. Moving one onto the board
took the strongest advertiser from −70 dBm through −63 to −58, and it later switched address
while staying the strongest in the room. A random private address that rotates every quarter hour
is exactly what a phone with privacy enabled looks like.

So the knob advertises as well, under the name `Knob Display`, connectable. That is the direction
that works: the phone lists the knob. It is also the direction the project needs later, if plugins
are ever to arrive over BLE.

Verified from a laptop on 2026-09-04. `bluetoothctl scan le` lists

```
[NEW] Device FE:01:2C:XX:XX:D9 Knob Display
```

and that address identifies the board beyond doubt: it is the S3's base MAC `fc:01:2c:xx:xx:d8`
with the Espressif offset, the same systematic as the factory firmware's SoftAP BSSID.
Connecting to it produces the whole cycle on the device:

```
INFO - BLE: a device connected
INFO - BLE: the device disconnected
INFO - BLE: advertising as "Knob Display"
```

**A phone will not show this in its Bluetooth settings.** Android lists classic devices and BLE
peripherals offering known services; a bare advertiser appears in a BLE scanner app and nowhere
else. That is Android's behaviour, not a fault in the advertisement — the same advertisement is
found immediately by `bluetoothctl` and by a scanner app on the phone.

A phone connecting from a BLE scanner app holds the link rather than dropping it the way
`bluetoothctl` does. It connects twice, which is the app's doing rather than the firmware's:
opening a device's detail page already establishes a connection, and the Connect button then
makes a second one.

```
BLE: a device connected           <- detail page opened
BLE: 4 advertisers, 0 with a name    BLE scan still running
Scan: 2 networks                     Wi-Fi scan still running
BLE: the device disconnected
BLE: advertising as "Knob Display"
BLE: a device connected           <- Connect pressed, two scan rounds later
Scan: 2 networks                     both scans still running
BLE: 4 advertisers, 0 with a name
BLE: the device disconnected
BLE: advertising as "Knob Display"
```

That is coexistence actually working, not merely compiled in: a BLE peripheral connection, a BLE
central scan and a Wi-Fi scan at the same time on one radio, with no reboot and no panic. It is
also what the 64 KiB coexistence heap is being spent on. The scan rounds double as a clock — they
are 30 s apart, which is how the gap between the two connections can be read off the log.

### Connections end after 30 s, and the scan is not why

Every connection so far has ended on its own after roughly a minute by the scan-round clock. The
obvious suspect was our own BLE scan: scanning and a connection share one radio, and a five
second scan window could make the controller miss enough connection events to trip the
supervision timeout.

It was tested by standing the scan down whenever a peer is connected, and measuring the duration
directly instead of counting scan rounds. The connection still ended — after exactly 30 s.

That number is the finding. Missed connection events and supervision timeouts produce awkward
values; a round 30 s is a timer somewhere.

The obvious next guess was the scanner app on the phone. It is not: a connection from
`bluetoothctl` on a laptop ends after 30 s as well, measured the same way. Two unrelated peers,
Android and BlueZ, cannot plausibly have picked the same round timeout by chance.

30 s is the ATT transaction timeout in the Bluetooth specification. A peer connects, starts
discovering GATT services, gets no answer from a device that has no ATT server, and is required
to tear the connection down. So this is not a fault and not a setting — it is what every correct
client does to a device offering nothing. The fix is a service.

(The 30 s measurements are ours; that the specification's ATT timeout is the reason is the
obvious reading of them, not something verified here.)

The scan stand-down stays in the code, documented as what it is: not the fix for this, but
sensible once a connection carries actual data.

### A service fixes it

The knob now presents a GATT service with two readable, notifiable characteristics — seconds
since boot, and the number of Wi-Fi networks found in the most recent scan, so a phone reading
the second one is seeing the other radio's work. With it in place:

```
bluetoothctl:  Connection successful
               ServicesResolved: yes
Board:         BLE: a device connected
               Scan: 2 networks     <- 30 s, previously the moment it died
               Scan: 2 networks     <- 60 s
               Scan: 2 networks     <- 90 s
```

The connection held for the full 90 s until the laptop ended it. `ServicesResolved: yes` shows
the service is not merely present but discovered. That confirms the diagnosis: the 30 s
disconnect was the absence of an ATT server, exactly as the specification requires.

One build detail worth knowing: the `#[gatt_server]` macro generates code naming `embassy-sync`
without depending on it, so it has to be a direct dependency — and at exactly the version
trouble-host resolves to (0.7), or the `RawMutex` traits of two parallel versions fail to match.

What none of this shows is a paired or authenticated link. Anyone in range can connect and read.


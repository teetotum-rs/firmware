# Nearby

A face for teetotum: the Wi-Fi networks and Bluetooth devices around the knob, and a way to find
one of them.

- **One dot for each network or device, and the stronger its signal, the nearer the middle.**
  Where a dot stands around the middle says nothing about direction, because a single antenna
  cannot tell where a signal comes from. The angle only keeps each dot in its place from one
  round to the next.
- **Wipe** to change between Wi-Fi and Bluetooth, **turn the knob** to choose one, strongest
  first. The middle shows its name, its strength in dBm and, for Wi-Fi, its channel.
- **Tap** to find the chosen one. The motor pulses faster the stronger its signal gets, and
  stops while a round does not hear it at all. Carry the knob about and follow the pulse. Tap
  again to stop.

## Rights

`knob`, `radio`, `haptic`. The firmware refuses to load a face that uses anything its manifest
does not ask for.

## What it sees

Names, signal strengths, Wi-Fi channels, and a key for each entry. **It never sees an address.**
The firmware makes the key from the address and a number it draws at every boot and keeps to
itself. A face can therefore follow one device while the knob runs, but cannot look a key up in
a list of addresses. A Bluetooth device that changes its own address, as phones do, turns up
again under a new key.

## The law where you use it

Receiving radio signals and showing what is heard is regulated differently from country to
country, by telecommunications and data protection law among others. **Observe the rules that
apply where you use this face.** This note is not legal advice.

## Building

`./build.sh` builds `firmware/assets/plugins/nearby.wasm`, which the firmware embeds.

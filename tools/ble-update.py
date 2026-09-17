#!/usr/bin/env python3
"""Send a signed firmware image to the knob over BLE.

Sign the image first (`tools/teetotum-pack firmware <image>` writes `<image>.tfw`), then open
Settings > Receive plugin on the knob. The knob writes the image into the partition it is not
running from, checks the signature, selects that partition and restarts.

Needs bleak (`pip install bleak`).
"""

import argparse
import asyncio
import struct
import sys
import time
from pathlib import Path

from bleak import BleakClient, BleakScanner
from bleak.exc import BleakError

NAME = "TeeToTum"
CONTROL = "19792d5c-9458-40ba-b233-c82b87d3dd4e"
DATA = "81bcd10c-d2eb-4f6a-b4db-e196026f9f7c"
STATUS = "1236b81e-8a6e-49bf-817a-210b74ac7990"

COMMIT, ABORT, UPDATE = 2, 3, 5
READY, WRITTEN = 1, 2
FAILURES = {
    0x82: "the knob took it for no update",
    0x83: "a piece went missing",
    0x84: "the image is not what was announced, or its signature does not hold",
    0x85: "the knob's flash failed",
}
SIGNATURE = 64
MAGIC = 0xE9
OFFSET = 4
PIECE_MAX = 240


class UpdateError(Exception):
    pass


def check(status: bytes) -> None:
    if status and status[0] in FAILURES:
        raise UpdateError(FAILURES[status[0]])


async def expect(statuses: asyncio.Queue, code: int, timeout: float) -> bytes:
    while True:
        status = await asyncio.wait_for(statuses.get(), timeout)
        check(status)
        if status[0] == code:
            return status


def drain(statuses: asyncio.Queue) -> None:
    while not statuses.empty():
        check(statuses.get_nowait())


async def update(
    signed: Path, address: str | None, timeout: float, wait: float, stop_at: int | None
) -> None:
    raw = signed.read_bytes()
    image, signature = raw[:-SIGNATURE], raw[-SIGNATURE:]
    if len(raw) <= SIGNATURE or image[0] != MAGIC:
        raise UpdateError(f"{signed}: not a signed ESP image")

    if address:
        device = await BleakScanner.find_device_by_address(address, timeout=timeout)
    else:
        device = await BleakScanner.find_device_by_name(NAME, timeout=timeout)
    if device is None:
        raise UpdateError(f"no {address or NAME} in range")

    statuses: asyncio.Queue = asyncio.Queue()
    async with BleakClient(device, timeout=timeout) as client:
        # BlueZ reports the default MTU until it is asked for the negotiated one.
        acquire = getattr(client._backend, "_acquire_mtu", None)
        if acquire is not None:
            await acquire()
        piece = min(PIECE_MAX, client.mtu_size - 3 - OFFSET)

        await client.start_notify(STATUS, lambda _, data: statuses.put_nowait(bytes(data)))
        begin = bytes([UPDATE]) + struct.pack("<I", len(image)) + signature
        # The knob ignores the command while its receive dialog is closed; ask again until `wait`.
        deadline = time.monotonic() + wait
        while True:
            try:
                await client.write_gatt_char(CONTROL, begin, response=True)
                status = await expect(statuses, READY, timeout)
                break
            except (BleakError, TimeoutError) as e:
                if time.monotonic() >= deadline:
                    raise UpdateError(
                        f"{e or type(e).__name__} -- is Settings > Receive plugin open on the knob?"
                    ) from e
                print("waiting for the receive dialog", flush=True)
        print(f"{device.name}: ota_{status[1]}, {len(image)} bytes in pieces of {piece}")

        started = time.monotonic()
        try:
            for offset in range(0, len(image), piece):
                if stop_at is not None and offset >= stop_at:
                    print(f"\nstopped at {offset} bytes, as asked")
                    return
                chunk = image[offset : offset + piece]
                await client.write_gatt_char(
                    DATA, struct.pack("<I", offset) + chunk, response=True
                )
                drain(statuses)
                done = offset + len(chunk)
                rate = done / max(time.monotonic() - started, 1e-3) / 1024
                print(f"\r{done} of {len(image)} bytes, {rate:.1f} KiB/s", end="", flush=True)
            print(f"\n{time.monotonic() - started:.1f} s")
            await client.write_gatt_char(CONTROL, bytes([COMMIT]), response=True)
            await expect(statuses, WRITTEN, timeout)
        except (UpdateError, BleakError, TimeoutError):
            print()
            if client.is_connected:
                await client.write_gatt_char(CONTROL, bytes([ABORT]), response=True)
            raise
    print("written; the knob restarts into the new firmware")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("signed", type=Path, help="signed firmware image, .tfw")
    parser.add_argument("--address", help="the knob's BLE address, instead of its name")
    parser.add_argument("--timeout", type=float, default=20.0, help="seconds per step")
    parser.add_argument(
        "--wait", type=float, default=0.0, help="seconds to keep asking while the dialog is closed"
    )
    parser.add_argument(
        "--stop-at", type=int, help="disconnect after this many bytes, to test a broken upload"
    )
    args = parser.parse_args()
    try:
        asyncio.run(update(args.signed, args.address, args.timeout, args.wait, args.stop_at))
    except (UpdateError, BleakError, TimeoutError) as e:
        print(f"ble-update: {e or type(e).__name__}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())

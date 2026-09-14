#!/usr/bin/env python3
"""Upload a signed plugin to the knob over BLE.

Open Settings > Receive plugin on the knob first. The knob writes the module into a free slot,
restarts, and asks for it in the install dialog.

Needs bleak (`pip install bleak`) and builds tools/teetotum-pack for the slot header.
"""

import argparse
import asyncio
import shutil
import struct
import subprocess
import sys
import tempfile
from pathlib import Path

from bleak import BleakClient, BleakScanner
from bleak.exc import BleakError

NAME = "TeeToTum"
CONTROL = "19792d5c-9458-40ba-b233-c82b87d3dd4e"
DATA = "81bcd10c-d2eb-4f6a-b4db-e196026f9f7c"
STATUS = "1236b81e-8a6e-49bf-817a-210b74ac7990"

BEGIN, COMMIT, ABORT = 1, 2, 3
READY, WRITTEN = 1, 2
FAILURES = {
    0x81: "every slot is taken",
    0x82: "the knob took it for no upload",
    0x83: "a piece went missing",
    0x84: "the module does not match its header",
    0x85: "the knob's flash failed",
}
HEADER = 64
OFFSET = 4
PIECE_MAX = 240

REPO = Path(__file__).resolve().parent.parent


class UploadError(Exception):
    pass


def slot_header(wasm: Path) -> bytes:
    """The header teetotum-pack writes in front of the module; it names no slot."""
    with tempfile.TemporaryDirectory() as tmp:
        copy = Path(tmp) / wasm.name
        shutil.copyfile(wasm, copy)
        subprocess.run(
            [REPO / "tools" / "teetotum-pack", "pack", str(copy), "--slot", "0"],
            check=True,
            stdout=subprocess.DEVNULL,
        )
        return copy.with_suffix(".slot").read_bytes()[:HEADER]


def check(status: bytes) -> None:
    if status and status[0] in FAILURES:
        raise UploadError(FAILURES[status[0]])


async def expect(statuses: asyncio.Queue, code: int, timeout: float) -> bytes:
    while True:
        status = await asyncio.wait_for(statuses.get(), timeout)
        check(status)
        if status[0] == code:
            return status


def drain(statuses: asyncio.Queue) -> None:
    while not statuses.empty():
        check(statuses.get_nowait())


async def upload(wasm: Path, address: str | None, timeout: float) -> None:
    module = wasm.read_bytes()
    header = slot_header(wasm)

    if address:
        device = await BleakScanner.find_device_by_address(address, timeout=timeout)
    else:
        device = await BleakScanner.find_device_by_name(NAME, timeout=timeout)
    if device is None:
        raise UploadError(f"no {address or NAME} in range")

    statuses: asyncio.Queue = asyncio.Queue()
    async with BleakClient(device, timeout=timeout) as client:
        # BlueZ reports the default MTU until it is asked for the negotiated one.
        acquire = getattr(client._backend, "_acquire_mtu", None)
        if acquire is not None:
            await acquire()
        piece = min(PIECE_MAX, client.mtu_size - 3 - OFFSET)

        await client.start_notify(STATUS, lambda _, data: statuses.put_nowait(bytes(data)))
        try:
            await client.write_gatt_char(CONTROL, bytes([BEGIN]) + header, response=True)
        except BleakError as e:
            raise UploadError(f"{e} -- is Settings > Receive plugin open on the knob?") from e
        status = await expect(statuses, READY, timeout)
        print(f"{device.name}: slot {status[1]}, {len(module)} bytes in pieces of {piece}")

        try:
            for offset in range(0, len(module), piece):
                chunk = module[offset : offset + piece]
                await client.write_gatt_char(
                    DATA, struct.pack("<I", offset) + chunk, response=True
                )
                drain(statuses)
                print(f"\r{offset + len(chunk)} of {len(module)} bytes", end="", flush=True)
            print()
            await client.write_gatt_char(CONTROL, bytes([COMMIT]), response=True)
            await expect(statuses, WRITTEN, timeout)
        except (UploadError, BleakError, TimeoutError):
            print()
            if client.is_connected:
                await client.write_gatt_char(CONTROL, bytes([ABORT]), response=True)
            raise
    print("written; the knob restarts and asks for it")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("wasm", type=Path, help="signed plugin module")
    parser.add_argument("--address", help="the knob's BLE address, instead of its name")
    parser.add_argument("--timeout", type=float, default=20.0, help="seconds per step")
    args = parser.parse_args()
    try:
        asyncio.run(upload(args.wasm, args.address, args.timeout))
    except (UploadError, BleakError, TimeoutError, subprocess.CalledProcessError) as e:
        print(f"ble-upload: {e or type(e).__name__}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())

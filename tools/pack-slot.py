#!/usr/bin/env python3
"""Pack a signed face into a slot of the `plugins` partition and write it with espflash.

    tools/pack-slot.py my-face.wasm --slot 0
    tools/pack-slot.py my-face.wasm --slot 0 --write

Writes `<module>.slot` next to the module: the slot header, then the module, in the format of
firmware/src/slots.rs. With `--write` it hands that file to `espflash write-bin` at the slot's
address in partitions.csv; espflash resets the board, and the firmware picks the slot up at boot.
A slot holding a bundled face's id takes that face's place. Sign first with tools/sign-face.py.
"""

import argparse
import hashlib
import importlib.util
import struct
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PARTITIONS = ROOT / "partitions.csv"
PARTITION = "plugins"

# firmware/src/slots.rs
SLOT = 64 * 1024
HEADER = 64
MAGIC = b"TTPS"
FORMAT = 1
HASH = 32

# teetotum-face/src/manifest.rs
MANIFEST = b"teetotum.manifest"
MANIFEST_VERSION = 3
ID_LEN = 8

_spec = importlib.util.spec_from_file_location("sign_face", Path(__file__).with_name("sign-face.py"))
sign_face = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(sign_face)


def custom_section(wasm: bytes, want: bytes) -> bytes:
    at = len(sign_face.HEADER)
    while at < len(wasm):
        kind = wasm[at]
        size, at = sign_face.read_leb128(wasm, at + 1)
        end = at + size
        if kind == 0:
            length, name = sign_face.read_leb128(wasm, at)
            if wasm[name:name + length] == want:
                return wasm[name + length:end]
        at = end
    raise ValueError(f"no {want.decode()} section")


def plugin_id(wasm: bytes) -> bytes:
    """Key and name hashed together, as `PluginId::new` does."""
    _, signature = sign_face.split(wasm)
    if signature is None:
        raise ValueError("not signed -- run tools/sign-face.py first")
    manifest = custom_section(wasm, MANIFEST)
    if manifest[0] != MANIFEST_VERSION:
        raise ValueError(f"manifest format {manifest[0]}, the firmware reads {MANIFEST_VERSION}")
    name = manifest[6:6 + manifest[5]]
    return hashlib.sha512(signature[:sign_face.KEY_LEN] + name).digest()[:ID_LEN]


def pack(wasm: bytes) -> tuple[bytes, bytes]:
    if len(wasm) > SLOT - HEADER:
        raise ValueError(f"{len(wasm)} bytes, a slot holds {SLOT - HEADER}")
    id = plugin_id(wasm)
    header = MAGIC + bytes([FORMAT, 0, 0, 0]) + id + struct.pack("<I", len(wasm))
    header += hashlib.sha512(wasm).digest()[:HASH]
    return header.ljust(HEADER, b"\0") + wasm, id


def partition() -> tuple[int, int]:
    """Offset and size of the plugins partition."""
    for line in PARTITIONS.read_text().splitlines():
        fields = [f.strip() for f in line.split(",")]
        if not line.startswith("#") and fields[0] == PARTITION:
            return int(fields[3], 0), int(fields[4], 0)
    raise ValueError(f"no {PARTITION} partition in {PARTITIONS}")


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("module", type=Path)
    ap.add_argument("--slot", type=int, required=True)
    ap.add_argument("--write", action="store_true", help="write the slot with espflash")
    args = ap.parse_args()
    try:
        offset, size = partition()
        if not 0 <= args.slot < size // SLOT:
            raise ValueError(f"slot {args.slot}, the partition has {size // SLOT}")
        if not sign_face.check(args.module):
            raise ValueError("the signature does not hold")
        image, id = pack(args.module.read_bytes())
        out = args.module.with_suffix(".slot")
        out.write_bytes(image)
        address = offset + args.slot * SLOT
        print(f"{out}: {len(image)} bytes, id {id.hex()}, slot {args.slot} at {address:#x}")
        command = ["espflash", "write-bin", "-B", "921600", f"{address:#x}", str(out)]
        if args.write:
            subprocess.run(command, check=True)
        else:
            print(" ".join(command))
    except (ValueError, OSError, subprocess.CalledProcessError) as e:
        print(f"pack-slot: {e}", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()

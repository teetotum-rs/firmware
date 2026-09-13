#!/usr/bin/env python3
"""Sign a face: append the author's Ed25519 key and signature as the module's last section.

    tools/sign-face.py firmware/assets/plugins/my-face.wasm
    tools/sign-face.py --check firmware/assets/plugins/*.wasm

The key is an OpenSSL PEM file, `--key`, else $TEETOTUM_KEY, else
~/.config/teetotum/face-key.pem; it is created on first use. Keep it: key and name are the
face's identity, and an update signed with another key is another face. A module that is
already signed is signed anew. Needs `openssl` 3.0 or later.
"""

import argparse
import os
import subprocess
import sys
import tempfile
from pathlib import Path

SECTION = b"teetotum.signature"
HEADER = b"\0asm\x01\0\0\0"
KEY_LEN = 32
SIGNATURE_LEN = 64
# DER prefix of an Ed25519 SubjectPublicKeyInfo; the 32 key bytes follow.
SPKI_PREFIX = bytes.fromhex("302a300506032b6570032100")


def leb128(n: int) -> bytes:
    out = bytearray()
    while True:
        byte, n = n & 0x7F, n >> 7
        out.append(byte | (0x80 if n else 0))
        if not n:
            return bytes(out)


def read_leb128(data: bytes, at: int) -> tuple[int, int]:
    value = shift = 0
    while True:
        if at >= len(data) or shift > 28:
            raise ValueError("sections do not add up")
        byte = data[at]
        value |= (byte & 0x7F) << shift
        at, shift = at + 1, shift + 7
        if not byte & 0x80:
            return value, at


def split(wasm: bytes) -> tuple[bytes, bytes | None]:
    """The module without its signature section, and that section's contents if it had one."""
    if not wasm.startswith(HEADER):
        raise ValueError("not a WebAssembly module")
    at, found = len(HEADER), None
    while at < len(wasm):
        start, kind = at, wasm[at]
        size, at = read_leb128(wasm, at + 1)
        end = at + size
        if end > len(wasm):
            raise ValueError("sections do not add up")
        if kind == 0:
            length, name = read_leb128(wasm, at)
            if wasm[name:name + length] == SECTION:
                if found:
                    raise ValueError("two signature sections")
                found = (start, end, wasm[name + length:end])
        at = end
    if not found:
        return wasm, None
    start, end, contents = found
    if end != len(wasm) or len(contents) != KEY_LEN + SIGNATURE_LEN:
        raise ValueError("signature section malformed or not last")
    return wasm[:start], contents


def openssl(*args: str) -> bytes:
    return subprocess.run(["openssl", *args], capture_output=True, check=True).stdout


def key_path(given: str | None) -> Path:
    if given:
        return Path(given)
    if os.environ.get("TEETOTUM_KEY"):
        return Path(os.environ["TEETOTUM_KEY"])
    return Path.home() / ".config" / "teetotum" / "face-key.pem"


def ensure_key(path: Path) -> None:
    if path.exists():
        return
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    openssl("genpkey", "-algorithm", "ed25519", "-out", str(path))
    path.chmod(0o600)
    print(f"created a new signing key at {path} -- back it up, it is the faces' identity",
          file=sys.stderr)


def sign(path: Path, key: Path) -> None:
    message, _ = split(path.read_bytes())
    public = openssl("pkey", "-in", str(key), "-pubout", "-outform", "DER")[-KEY_LEN:]
    with tempfile.TemporaryDirectory() as tmp:
        msg = Path(tmp) / "message"
        msg.write_bytes(message)
        signature = openssl("pkeyutl", "-sign", "-inkey", str(key), "-rawin", "-in", str(msg))
    if len(signature) != SIGNATURE_LEN:
        raise ValueError("openssl did not return an Ed25519 signature")
    name = leb128(len(SECTION)) + SECTION
    body = name + public + signature
    path.write_bytes(message + b"\0" + leb128(len(body)) + body)
    print(f"{path}: {path.stat().st_size} bytes, signed by {public.hex()[:16]}")


def check(path: Path) -> bool:
    message, contents = split(path.read_bytes())
    if contents is None:
        print(f"{path}: not signed")
        return False
    public, signature = contents[:KEY_LEN], contents[KEY_LEN:]
    with tempfile.TemporaryDirectory() as tmp:
        files = {name: Path(tmp) / name for name in ("message", "key", "signature")}
        files["message"].write_bytes(message)
        files["key"].write_bytes(SPKI_PREFIX + public)
        files["signature"].write_bytes(signature)
        result = subprocess.run(
            ["openssl", "pkeyutl", "-verify", "-pubin", "-keyform", "DER",
             "-inkey", str(files["key"]), "-rawin", "-in", str(files["message"]),
             "-sigfile", str(files["signature"])],
            capture_output=True)
    holds = result.returncode == 0
    print(f"{path}: signed by {public.hex()[:16]}, {'holds' if holds else 'does NOT hold'}")
    return holds


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("modules", nargs="+", type=Path)
    ap.add_argument("--key", help="PEM file of the author's Ed25519 key")
    ap.add_argument("--check", action="store_true", help="verify instead of signing")
    args = ap.parse_args()
    try:
        if args.check:
            sys.exit(0 if all([check(m) for m in args.modules]) else 1)
        key = key_path(args.key)
        ensure_key(key)
        for module in args.modules:
            sign(module, key)
    except (ValueError, subprocess.CalledProcessError) as e:
        detail = getattr(e, "stderr", b"") or b""
        print(f"sign-face: {e} {detail.decode(errors='replace').strip()}", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()

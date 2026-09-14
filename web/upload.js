// Sending a signed plugin to the knob over Web Bluetooth: the slot header, the module's own
// claims, and the upload protocol of firmware/src/upload.rs. No DOM here.

export const NAME = "TeeToTum";
export const SERVICE = "4a729af2-063c-451a-8c73-60e5fab61ccb";
const CONTROL = "19792d5c-9458-40ba-b233-c82b87d3dd4e";
const DATA = "81bcd10c-d2eb-4f6a-b4db-e196026f9f7c";
const STATUS = "1236b81e-8a6e-49bf-817a-210b74ac7990";

const BEGIN = 1, COMMIT = 2, ABORT = 3;
const READY = 1, WRITTEN = 2;
const FAILURES = {
  0x81: "every slot on the knob is taken",
  0x82: "the knob took it for no upload",
  0x83: "a piece went missing",
  0x84: "the module does not match its header",
  0x85: "the knob's flash failed",
};

const SLOT = 64 * 1024;
const HEADER = 64;
export const MODULE_MAX = SLOT - HEADER;
const OFFSET = 4;
const PIECE_MAX = 240;
// What fits the smallest ATT MTU, 23 bytes, after the offset.
const PIECE_MIN = 16;

const MANIFEST = "teetotum.manifest";
const SIGNATURE = "teetotum.signature";
const MANIFEST_VERSION = 3;
const KEY_LEN = 32;
const SIGNATURE_LEN = 64;
const NAME_MAX = 20;
const ICON_SIZE = 24;
const SUMMARY_AT = 6 + NAME_MAX + 4 * ICON_SIZE;
const SUMMARY_MAX = 32;
const ABI_AT = SUMMARY_AT + 1 + SUMMARY_MAX;
const VERSION_AT = ABI_AT + 2;
const RIGHTS = ["HID", "KNOB", "RANDOM", "RADIO", "HAPTIC"];

export class UploadError extends Error {}

export const hex = (bytes) => Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");

async function sha(algorithm, ...parts) {
  const all = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
  let at = 0;
  for (const p of parts) {
    all.set(p, at);
    at += p.length;
  }
  return new Uint8Array(await crypto.subtle.digest(algorithm, all));
}

export const sha256 = async (bytes) => hex(await sha("SHA-256", bytes));

function leb128(bytes, at) {
  let value = 0;
  for (let i = 0; i < 5 && at + i < bytes.length; i++) {
    const byte = bytes[at + i];
    value += (byte & 0x7f) * 2 ** (7 * i);
    if ((byte & 0x80) === 0) return [value, at + i + 1];
  }
  throw new UploadError("not a WebAssembly module");
}

// Every custom section as {name, start, end, contents}, as teetotum-face's `walk` reads them.
function customSections(wasm) {
  const magic = [0, 0x61, 0x73, 0x6d, 1, 0, 0, 0];
  if (wasm.length < 8 || magic.some((b, i) => wasm[i] !== b)) {
    throw new UploadError("not a WebAssembly module");
  }
  const sections = [];
  let at = 8;
  while (at < wasm.length) {
    const start = at;
    const id = wasm[at];
    const [size, body] = leb128(wasm, at + 1);
    const end = body + size;
    if (end > wasm.length) throw new UploadError("not a WebAssembly module");
    if (id === 0) {
      const [len, name] = leb128(wasm, body);
      if (name + len > end) throw new UploadError("not a WebAssembly module");
      sections.push({
        name: new TextDecoder().decode(wasm.subarray(name, name + len)),
        start,
        end,
        contents: wasm.subarray(name + len, end),
      });
    }
    at = end;
  }
  return sections;
}

// What a module says about itself. The signature is not checked: the knob does that before it
// asks the user.
export async function describe(wasm) {
  if (wasm.length > MODULE_MAX) throw new UploadError(`longer than ${MODULE_MAX} bytes`);
  const sections = customSections(wasm);
  const manifests = sections.filter((s) => s.name === MANIFEST);
  const signatures = sections.filter((s) => s.name === SIGNATURE);
  if (manifests.length !== 1) throw new UploadError("no TeeToTum manifest");
  if (signatures.length !== 1) throw new UploadError("not signed");
  const signature = signatures[0];
  if (signature.end !== wasm.length || signature.contents.length !== KEY_LEN + SIGNATURE_LEN) {
    throw new UploadError("signature section malformed or not last");
  }
  const m = manifests[0].contents;
  if (m[0] !== MANIFEST_VERSION || m.length < VERSION_AT + 6) {
    throw new UploadError(`manifest format ${m[0]}, not ${MANIFEST_VERSION}`);
  }
  const text = (at, len) => new TextDecoder("utf-8", { fatal: true }).decode(m.subarray(at, at + len));
  const number = (at) => m[at] | (m[at + 1] << 8);
  const bits = m[1] | (m[2] << 8) | (m[3] << 16) | (m[4] << 24);
  const nameLen = m[5];
  const summaryLen = m[SUMMARY_AT];
  if (nameLen === 0 || nameLen > NAME_MAX || summaryLen > SUMMARY_MAX) {
    throw new UploadError("manifest malformed");
  }
  const name = text(6, nameLen);
  const key = signature.contents.subarray(0, KEY_LEN);
  const id = (await sha("SHA-512", key, new TextEncoder().encode(name))).subarray(0, 8);
  return {
    name,
    summary: text(SUMMARY_AT + 1, summaryLen),
    rights: RIGHTS.filter((_, i) => bits & (1 << i)),
    abi: number(ABI_AT),
    version: [0, 2, 4].map((i) => number(VERSION_AT + i)).join("."),
    key: hex(key),
    id,
    size: wasm.length,
  };
}

// The header teetotum-pack writes in front of a module, waiting to be accepted.
export async function header(wasm, id) {
  const raw = new Uint8Array(HEADER);
  raw.set([0x54, 0x54, 0x50, 0x53, 1, 0xff]);
  raw.set(id, 8);
  new DataView(raw.buffer).setUint32(16, wasm.length, true);
  raw.set((await sha("SHA-512", wasm)).subarray(0, 32), 20);
  return raw;
}

export function supported() {
  return typeof navigator !== "undefined" && "bluetooth" in navigator;
}

function within(promise, seconds, what) {
  let timer;
  const late = new Promise((_, reject) => {
    timer = setTimeout(() => reject(new UploadError(`no answer from the knob ${what}`)), seconds * 1000);
  });
  return Promise.race([promise, late]).finally(() => clearTimeout(timer));
}

// Sends `wasm` to a knob the user picks. `progress(text, fraction)` hears how it goes.
export async function upload(wasm, progress, seconds = 20) {
  const about = await describe(wasm);
  const head = await header(wasm, about.id);

  const device = await navigator.bluetooth.requestDevice({
    filters: [{ name: NAME }],
    optionalServices: [SERVICE],
  });
  progress(`connecting to ${device.name ?? NAME}`, 0);
  const server = await within(device.gatt.connect(), seconds, "while connecting");
  try {
    const service = await server.getPrimaryService(SERVICE);
    const [control, data, status] = await Promise.all(
      [CONTROL, DATA, STATUS].map((uuid) => service.getCharacteristic(uuid)),
    );

    const waiting = [];
    let failure = null;
    status.addEventListener("characteristicvaluechanged", (event) => {
      const code = event.target.value.getUint8(0);
      if (FAILURES[code]) failure = new UploadError(FAILURES[code]);
      const slot = event.target.value.getUint8(1);
      for (const w of waiting.splice(0)) w(code, slot);
    });
    await status.startNotifications();
    const expect = (code, what) =>
      within(
        new Promise((resolve, reject) => {
          const listen = (got, slot) => {
            if (failure) reject(failure);
            else if (got === code) resolve(slot);
            else waiting.push(listen);
          };
          waiting.push(listen);
        }),
        seconds,
        what,
      );

    const ready = expect(READY, "after the header");
    try {
      await control.writeValueWithResponse(Uint8Array.of(BEGIN, ...head));
    } catch (e) {
      throw new UploadError(`${e.message} -- is Settings > Receive open on the knob?`);
    }
    const slot = await ready;

    let piece = PIECE_MAX;
    try {
      for (let offset = 0; offset < wasm.length; ) {
        if (failure) throw failure;
        const chunk = wasm.subarray(offset, offset + piece);
        const write = new Uint8Array(OFFSET + chunk.length);
        new DataView(write.buffer).setUint32(0, offset, true);
        write.set(chunk, OFFSET);
        try {
          await data.writeValueWithResponse(write);
        } catch (e) {
          // A link with a small MTU refuses long pieces; the same offset goes again, shorter.
          if (piece === PIECE_MIN || !device.gatt.connected) throw e;
          piece = PIECE_MIN;
          continue;
        }
        offset += chunk.length;
        progress(`slot ${slot}: ${offset} of ${wasm.length} bytes`, offset / wasm.length);
      }
      const written = expect(WRITTEN, "after the last piece");
      await control.writeValueWithResponse(Uint8Array.of(COMMIT));
      await written;
    } catch (e) {
      if (device.gatt.connected) await control.writeValueWithResponse(Uint8Array.of(ABORT)).catch(() => {});
      throw e;
    }
    progress("written; the knob restarts and asks you to install it", 1);
  } finally {
    if (device.gatt.connected) device.gatt.disconnect();
  }
  return about;
}

"""How much bilinear differs from nearest when upscaling 200x200 cover art to 360x360.

Reproduces `teetotum/src/image.rs::draw_sampled` (16.16 fixed point, sample at the pixel centre)
and passes the source through JPEG and the result through RGB565, as on the device.

Typical result: mean difference 3/255; maximum 116 at lettering edges, 24 on a photograph.
"""
import os
import tempfile

import numpy as np
from PIL import Image, ImageDraw, ImageFilter, ImageFont

DST = 360


def sample(src, dst, blend):
    h, w, _ = src.shape
    step = (w << 16) // dst
    out = np.zeros((dst, dst, 3), np.uint8)
    for dy in range(dst):
        sy = (dy * 2 + 1) * step // 2 - 32768
        for dx in range(dst):
            sx = (dx * 2 + 1) * step // 2 - 32768
            if not blend:
                x = min(max((sx + 32768) >> 16, 0), w - 1)
                y = min(max((sy + 32768) >> 16, 0), h - 1)
                out[dy, dx] = src[y, x]
                continue
            x0 = min(max(sx >> 16, 0), w - 1)
            y0 = min(max(sy >> 16, 0), h - 1)
            x1 = min(x0 + 1, w - 1)
            y1 = min(y0 + 1, h - 1)
            fx = 0 if sx < 0 else sx & 0xFFFF
            fy = 0 if sy < 0 else sy & 0xFFFF
            tl = src[y0, x0].astype(np.int64)
            tr = src[y0, x1].astype(np.int64)
            bl = src[y1, x0].astype(np.int64)
            br = src[y1, x1].astype(np.int64)
            top = tl + ((tr - tl) * fx >> 16)
            bot = bl + ((br - bl) * fx >> 16)
            out[dy, dx] = top + ((bot - top) * fy >> 16)
    return out


def quantise(a):
    """RGB565, the way the panel stores it: the difference has to survive that."""
    a = a.astype(np.uint16)
    return np.stack([a[..., 0] & 0xF8, a[..., 1] & 0xFC, a[..., 2] & 0xF8], -1).astype(np.int64)


def cover(size):
    """A cover-shaped picture: big letters, a thin border, a photograph-ish gradient."""
    img = Image.new("RGB", (size, size))
    d = ImageDraw.Draw(img)
    for y in range(size):
        d.line([(0, y), (size, y)], fill=(30 + y // 3, 20, 90 - y // 5))
    d.rectangle([4, 4, size - 5, size - 5], outline=(240, 240, 230), width=2)
    try:
        font = ImageFont.truetype("/usr/share/fonts/dejavu-sans-fonts/DejaVuSans-Bold.ttf", size // 5)
        small = ImageFont.truetype("/usr/share/fonts/dejavu-sans-fonts/DejaVuSans.ttf", size // 14)
    except OSError:
        font = small = ImageFont.load_default()
    d.text((size // 8, size // 3), "ATLAS", fill=(255, 240, 200), font=font)
    d.text((size // 8, size // 3 + size // 4), "the quiet hours", fill=(220, 220, 255), font=small)
    return img


for name, src_img in [("cover200", cover(200)), ("photo200", None)]:
    if src_img is None:
        # A real photograph's worth of detail: high-frequency noise smoothed a little.
        rng = np.random.default_rng(7)
        a = rng.integers(0, 255, (200, 200, 3), dtype=np.uint8)
        src_img = Image.fromarray(a).filter(ImageFilter.GaussianBlur(1.2))
    # Through a JPEG, because that is what actually arrives.
    with tempfile.TemporaryDirectory() as tmp:
        path = os.path.join(tmp, f"{name}.jpg")
        src_img.save(path, quality=90)
        src = np.asarray(Image.open(path).convert("RGB"))
    near = quantise(sample(src, DST, False))
    bili = quantise(sample(src, DST, True))
    diff = np.abs(near - bili)
    print(f"{name}: {src.shape[1]}x{src.shape[0]} -> {DST}  mean {diff.mean():.1f}  max {diff.max()}  "
          f"pixels differing {100 * (diff.max(-1) > 0).mean():.1f}%")

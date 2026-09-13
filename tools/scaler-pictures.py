#!/usr/bin/env python3
"""Build the scaler test pictures that `firmware/src/bin/jpegshow.rs` compiles in.

Pictures at the panel's 360x360 cannot tell scalers apart, so these are larger:

* `zone`: a zone plate, sin(a*r^2), reaching 0.28 cycles/px at the rim. Downscaled, an
  area-averaging scaler turns the high frequencies grey; a point sampler shows false rings.
* `fine`: one-pixel rings, spokes, a frequency sweep and text in four sizes, like cover art.

Sizes 1024 (downscale 2.8x) and 480 (1.33x). JPEG quality 92 without chroma subsampling, so
compression artefacts do not mask the scaler.
"""

import math
import os

import numpy as np
from PIL import Image, ImageDraw, ImageFont

OUT = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "firmware", "assets", "scaler")

FONTS = (
    "/usr/share/fonts/dejavu-sans-fonts/DejaVuSans-Bold.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
)


def font(size):
    for path in FONTS:
        try:
            return ImageFont.truetype(path, size)
        except OSError:
            continue
    return ImageFont.load_default()


def zoneplate(n, fmax=0.28):
    """phase = a*r^2, so the local frequency reaches `fmax` cycles per pixel at the rim."""
    c = n / 2.0
    a = fmax * math.pi / c
    y, x = np.mgrid[0:n, 0:n]
    v = np.sin(a * ((x - c) ** 2 + (y - c) ** 2))
    g = ((v + 1.0) * 127.5).astype(np.uint8)
    return Image.fromarray(np.dstack([g, g, g]))


def fine(n):
    """Thin lines at several pitches and angles, plus text down to small sizes."""
    img = Image.new("RGB", (n, n), (16, 16, 20))
    d = ImageDraw.Draw(img)
    c = n / 2
    r, step = 6, 3.0
    while r < n * 0.72:
        d.ellipse([c - r, c - r, c + r, c + r], outline=(235, 235, 245), width=1)
        r += step
        step += 0.35
    for i in range(72):
        a = i * math.pi / 36
        d.line(
            [c + math.cos(a) * n * 0.74, c + math.sin(a) * n * 0.74,
             c + math.cos(a) * n * 0.98, c + math.sin(a) * n * 0.98],
            fill=(255, 200, 60), width=1,
        )
    top, bot = int(n * 0.40), int(n * 0.60)
    for x in range(n):
        f = 1 + (x / n) * 14
        v = 255 if int(x / f) % 2 == 0 else 20
        d.line([x, top, x, bot], fill=(v, v, v))
    for i, size in enumerate((int(n * 0.075), int(n * 0.045), int(n * 0.028), int(n * 0.018))):
        d.text((int(n * 0.06), int(n * 0.66) + i * int(n * 0.075)),
               "Manual 1984", font=font(size), fill=(120, 255, 140))
    return img


def main():
    os.makedirs(OUT, exist_ok=True)
    for n in (1024, 480):
        for name, make in (("zone", zoneplate), ("fine", fine)):
            path = os.path.join(OUT, f"{name}{n}.jpg")
            make(n).save(path, quality=92, subsampling=0)
            print(f"{path}: {os.path.getsize(path) // 1024} KiB")


if __name__ == "__main__":
    main()

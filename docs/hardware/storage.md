# The TF card

What a read costs at three clock rates, and why the factory demo's backgrounds need nothing
done to them. Part of the [hardware documentation](README.md).

## The card, from a path to a picture

The four pins were found by asking the card itself (`firmware/src/bin/sdprobe.rs`, 2026-09-05), and a
first walk of the filesystem on 2026-09-06 showed what is on it: **119 files in 10 directories,
400 MiB of demo media** on a 480 MiB FAT32 card, one folder per feature of the factory demo.
That walk was a measurement and printed a tree; it could not open anything.

`teetotum/src/fat.rs` is the same knowledge as a library -- read-only FAT16/FAT32, `Volume::mount`,
`open("/PIC/1.JPG")`, `File::read`, and a listing that is now just one of its callers. Writing
is deliberately absent: it is the half that can destroy a card, and nothing needs it yet.

### What a read costs

256 KiB from the same sectors at each bus clock, every run checksummed, reproduced across two
boots (`firmware/src/bin/sdcard.rs`, 2026-09-08):

| Bus clock | One block per command | Eight blocks per command | Checksum |
|---|---|---|---|
| 20 MHz | 1287 KiB/s | **1615 KiB/s** | `c0fdebe2` |
| 25 MHz | 1498 KiB/s | **1950 KiB/s** | `c0fdebe2` |
| 40 MHz | 1779 KiB/s | **2444 KiB/s** | `c0fdebe2` |

CMD18 -- one command and one address for a whole run, which the card can also read ahead of --
is worth **25 % at every clock** and costs nothing to have. The checksum never moves, so this
card reads correctly at 40 MHz, well past the 25 MHz the SD specification allows in SPI mode.

`sd::FAST_RATE` is nevertheless **25 MHz**. The panel runs at 80 MHz because the panel is
soldered to this board and can be measured once and for all; **the card is the one part of this
device the user swaps**, and the next card in the slot has not been measured by anybody.

Above 25 MHz the bus stops being the limit: throughput is 66 % of the clock at 20 MHz, 64 % at
25 and 49 % at 40. What is left is the per-byte cost of the blocking SPI driver, which shifts a
byte at a time through the FIFO. DMA is the next lever and is unmeasured.

Through the filesystem it costs almost nothing extra: `/CLOCKBG/star_bg_360.bin`, 259204 bytes,
read whole in **159 ms** against 1615 KiB/s for raw runs at the same clock -- the chain walk, the
header and the copy out of the scratch sector are about 2 % together.

### The picture needs nothing done to it

The demo's `*_360.bin` backgrounds are **259204 bytes = 360 x 360 x 2 + 4**: one screen of this
panel's RGB565 behind a four-byte header that reads as 360 by 360. That is the shape of the
framebuffer, so `firmware/src/bin/sdshow.rs` reads the file straight into it -- no decoder, no scaling,
no drawing -- and a tap on the glass swaps the two bytes of every pixel, because no file format
here declares its byte order.

Judged on 2026-09-08: **the picture is right as it is stored**, and the tap turns it **grey**.
The files are high byte first, the same way round as the panel and as `teetotum/src/framebuffer.rs`, so a
background costs one read and no pass over the pixels. The grey is worth keeping as a rule:
swapping the bytes of an RGB565 pixel cuts across the three channels rather than permuting them,
so neighbouring values of a photograph come out unrelated -- **a wrong byte order looks like fog,
not like a colour error.** The file name is drawn over the picture by `embedded-graphics`, in the
order we know is right; that it stayed white and legible over both is what rules out the panel,
the bus and the blit.


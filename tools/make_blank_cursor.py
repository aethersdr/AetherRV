#!/usr/bin/env python3
"""Write an invisible Xcursor theme for the touch-only kiosk (no xcursorgen needed).

cage/wlroots draws its own pointer in the middle of the screen until a mouse moves over a client,
which never happens on a touchscreen. The kiosk unit points XCURSOR_PATH/XCURSOR_THEME at this
theme, so every cursor name resolves to a 1x1 fully transparent image.

    python3 tools/make_blank_cursor.py /usr/share/aetherrv/cursors
"""
import os
import struct
import sys

NAMES = ["default", "left_ptr", "arrow", "top_left_arrow", "pointer", "hand1", "hand2", "text", "xterm",
         "wait", "watch", "progress", "crosshair", "grab", "grabbing", "move", "not-allowed", "help"]


def xcursor_blank(size: int = 24) -> bytes:
    img_hdr = struct.pack("<9I", 36, 0xFFFD0002, size, 1, 1, 1, 0, 0, 0)   # header, type, nominal, ver, w, h, xhot, yhot, delay
    pixels = struct.pack("<I", 0x00000000)                                 # one transparent ARGB pixel
    toc_pos = 16 + 12
    header = struct.pack("<4sIII", b"Xcur", 16, 0x10000, 1)
    toc = struct.pack("<III", 0xFFFD0002, size, toc_pos)
    return header + toc + img_hdr + pixels


def main(root: str) -> None:
    d = os.path.join(root, "default", "cursors")
    os.makedirs(d, exist_ok=True)
    data = xcursor_blank()
    for n in NAMES:
        with open(os.path.join(d, n), "wb") as f:
            f.write(data)
    with open(os.path.join(root, "default", "index.theme"), "w") as f:
        f.write("[Icon Theme]\nName=AetherRV blank\nComment=Invisible pointer for the touch kiosk\n")
    print(f"blank cursor theme written to {root}/default ({len(NAMES)} names)")


if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else "/usr/share/aetherrv/cursors")

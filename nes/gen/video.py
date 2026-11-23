#!/usr/bin/env python3
"""The video path to a `Screen` (phase 7's output stage, built early).

    python3 nes/gen/video.py > nes/build/video.json   (needs the cell library)

- `ppu_timing`: the 2C02's frame timing at gate level: dots 0..340 and
  scanlines 0..261 per frame (NTSC), with the odd-frame skip: while
  `skip` (rendering on), every other frame's pre-render line ends at dot
  339, one dot early; and the `Screen`'s Scanline-mode sync: `hsync` at
  dot 0 (the beam to the start of the next row), `vsync` through lines
  240..261 and at dot 0 of line 0 (the beam held at the top). So dots
  1..256 of lines 0..239 are pixels 0..255 of rows 0..239.
- `nes_palette`: NES colour index (6 bits) -> the Screen's Rgb332. An
  approximation of the 2C02 palette (the common FCEUX one), until the
  Screen has an NES palette of its own (plan item 0.8).
- `video_test`: the timing, the palette and a 256 x 256 Scanline Screen
  showing all 64 colours as an 8 x 8 grid.
"""

import json
import sys

import cells
from cells import chip, gate, inp, out, unopt, use

cells.EDITS.clear()


def inc(n):
    """`inc<n>`: S = A + Ci, a chain of half adders."""
    chip(f"inc{n}",
         [inp("A", n), inp("Ci"), use("h", "half_adder", n), out("S", n), out("Co")],
         [["A", "h.A"], ["Ci", "h[0].B"], [f"h[0..{n - 1}].C", f"h[1..{n}].B"], ["h.S", "S"],
          [f"h[{n - 1}].C", "Co"]],
         [{"name": "function", "formula": {"expect": {"S": "A + Ci", "Co": f"(A + Ci) >> {n}"}}}, unopt()])


def regr(n):
    """`reg<n>r`: n flip-flops with enable and synchronous reset."""
    chip(f"reg{n}r",
         [inp("D", n), inp("En"), inp("Rst"), use("b", "dffre", n), out("Q", n)],
         [["D", "b.D"], ["En", "b.En"], ["Rst", "b.Rst"], ["b.Q", "Q"]],
         [{"name": "function", "sequence": {"steps": [
             {"set": {"D": 300, "En": 1}, "ticks": 2, "expect": {"Q": 300}},
             {"set": {"En": 0, "D": 5}, "ticks": 2, "expect": {"Q": 300}},
             {"set": {"Rst": 1}, "ticks": 2, "expect": {"Q": 0}}]}}, unopt(1000)])


def eqc(n, value):
    """`eq<n>_<value>`: A == value, an AND of the bits (inverted where 0)."""
    name = f"eq{n}_{value}"
    parts, wires = [inp("A", n)], []
    bits = []
    for i in range(n):
        if value >> i & 1:
            bits.append(f"A:{i}")
        else:
            parts.append(gate(f"n{i}", "Not"))
            wires.append([f"A:{i}", f"n{i}"])
            bits.append(f"n{i}")
    acc = bits[0]
    for i, b in enumerate(bits[1:]):
        parts.append(gate(f"k{i}", "And"))
        wires += [[acc, f"k{i}.a"], [b, f"k{i}.b"]]
        acc = f"k{i}"
    parts.append(out("Y"))
    wires.append([acc, "Y"])
    chip(name, parts, wires,
         [{"name": "function", "formula": {"expect": {"Y": f"A == {value}"}}}, unopt()])
    return name


inc(9)
regr(9)
E340, E339, E261, E0 = eqc(9, 340), eqc(9, 339), eqc(9, 261), eqc(9, 0)

# Line 240 or more (lines are < 512, and below 256 the top four bits).
chip("ge240",
     [inp("A", 9), gate("k0", "And"), gate("k1", "And"), gate("k2", "And"), gate("o", "Or"), out("Y")],
     [["A:7", "k0.a"], ["A:6", "k0.b"], ["k0", "k1.a"], ["A:5", "k1.b"], ["k1", "k2.a"],
      ["A:4", "k2.b"], ["k2", "o.a"], ["A:8", "o.b"], ["o", "Y"]],
     [{"name": "function", "formula": {"expect": {"Y": "A >= 240"}}}, unopt()])

ONE = {"name": "one", "type": "Const", "value": 1}

chip("ppu_timing",
     [inp("en"), inp("skip"), ONE, use("dotr", "reg9r"), use("dinc", "inc9"), use("lr", "reg9r"),
      use("linc", "inc9"), use("d340", E340), use("d339", E339), use("l261", E261), use("d0", E0),
      use("l0", E0), use("vb", "ge240"), use("odd", "dffe"), gate("nodd", "Not"),
      gate("sk0", "And"), gate("sk1", "And"), gate("sk2", "And"), gate("lend", "Or"),
      gate("de", "And"), gate("le", "And"), gate("v0", "And"), gate("vs", "Or"),
      out("dot", 9), out("line", 9), out("hsync"), out("vsync")],
     [["one", "dinc.Ci", "linc.Ci"],
      ["dotr.Q", "dinc.A", "d340.A", "d339.A", "d0.A", "dot"], ["dinc.S", "dotr.D"],
      ["en", "dotr.En", "de.b"],
      # A line ends at dot 340, or at 339 on the pre-render line of an odd
      # frame while skipping.
      ["skip", "sk0.a"], ["odd.Q", "sk0.b", "nodd"], ["sk0", "sk1.a"], ["l261.Y", "sk1.b"],
      ["sk1", "sk2.a"], ["d339.Y", "sk2.b"], ["d340.Y", "lend.a"], ["sk2", "lend.b"],
      ["lend", "de.a"], ["de", "dotr.Rst", "lr.En", "le.a"],
      ["nodd", "odd.D"], ["le", "odd.En"],
      ["lr.Q", "linc.A", "l261.A", "l0.A", "vb.A", "line"], ["linc.S", "lr.D"],
      ["l261.Y", "le.b"], ["le", "lr.Rst"],
      ["d0.Y", "hsync", "v0.b"], ["l0.Y", "v0.a"], ["vb.Y", "vs.a"], ["v0", "vs.b"], ["vs", "vsync"]],
     [{"name": "frame", "sequence": {"steps": [
         {"set": {"en": 1}, "ticks": 1, "expect": {"dot": 0, "line": 0, "hsync": 1, "vsync": 1}},
         {"ticks": 341, "expect": {"dot": 0, "line": 1, "hsync": 1, "vsync": 0}},
         {"ticks": 239 * 341, "expect": {"dot": 0, "line": 240, "vsync": 1}},
         {"ticks": 22 * 341, "expect": {"dot": 0, "line": 0, "vsync": 1}},
         {"set": {"en": 0}, "ticks": 5, "expect": {"dot": 1, "line": 0}}]}},
      {"name": "odd frame skip", "sequence": {"steps": [
         {"set": {"en": 1, "skip": 1}, "ticks": 1, "expect": {"dot": 0, "line": 0}},
         {"ticks": 262 * 341, "expect": {"dot": 0, "line": 0}},
         {"ticks": 262 * 341 - 1, "expect": {"dot": 0, "line": 0}},
         {"ticks": 262 * 341, "expect": {"dot": 0, "line": 0}}]}}])

# NES colour index -> RGB (the FCEUX palette), as 0xRRGGBB.
NES_RGB = [
    0x7C7C7C, 0x0000FC, 0x0000BC, 0x4428BC, 0x940084, 0xA80020, 0xA81000, 0x881400,
    0x503000, 0x007800, 0x006800, 0x005800, 0x004058, 0x000000, 0x000000, 0x000000,
    0xBCBCBC, 0x0078F8, 0x0058F8, 0x6844FC, 0xD800CC, 0xE40058, 0xF83800, 0xE45C10,
    0xAC7C00, 0x00B800, 0x00A800, 0x00A844, 0x008888, 0x000000, 0x000000, 0x000000,
    0xF8F8F8, 0x3CBCFC, 0x6888FC, 0x9878F8, 0xF878F8, 0xF85898, 0xF87858, 0xFCA044,
    0xF8B800, 0xB8F818, 0x58D854, 0x58F898, 0x00E8D8, 0x787878, 0x000000, 0x000000,
    0xFCFCFC, 0xA4E4FC, 0xB8B8F8, 0xD8B8F8, 0xF8B8F8, 0xF8A4C0, 0xF0D0B0, 0xFCE0A8,
    0xF8D878, 0xD8F878, 0xB8F8B8, 0xB8F8D8, 0x00FCFC, 0xF8D8F8, 0x000000, 0x000000,
]


def rgb332(c):
    r, g, b = c >> 16, (c >> 8) & 0xFF, c & 0xFF
    return (r >> 5) << 5 | (g >> 5) << 2 | (b >> 6)


chip("nes_palette",
     [inp("index", 6), {"name": "rom", "type": "Rom", "addr_bits": 6, "w": 8,
                        "contents": [rgb332(c) for c in NES_RGB]}, out("rgb", 8)],
     [["index", "rom.addr"], ["rom.data", "rgb"]],
     [{"name": "function", "table": {"inputs": ["index"], "outputs": ["rgb"],
                                     "rows": [[i, rgb332(c)] for i, c in enumerate(NES_RGB)]}}])

# `video_test`: the timing, a colour-grid test pattern, the palette and a
# screen (the console in the root circuit uses the same three parts).
root = [
    {"add": [
        {"name": "en", "type": "Const", "value": 1},
        {"name": "timing", "use": "ppu_timing"},
        {"name": "palette", "use": "nes_palette"},
        {"name": "screen", "type": "Screen", "mode": "Scanline", "width": 256, "height": 256,
         "palette": "Rgb332", "scale": 2},
    ]},
    {"wire": [
        ["en", "timing.en"],
        ["timing.dot:5..8", "palette.index:0..3"], ["timing.line:5..8", "palette.index:3..6"],
        ["palette.rgb", "screen.colour"], ["timing.hsync", "screen.hsync"],
        ["timing.vsync", "screen.vsync"],
    ]},
]
cells.EDITS.append({"circuit": "video_test", "ops": root})

if __name__ == "__main__":
    cells.emit(cells.EDITS)

#!/usr/bin/env python3
"""The console: the root circuit of the `nes` workspace (phases 6 and 7).

    python3 nes/gen/console.py > nes/build/console.json

CPU (`cpu6502`) at one cycle per three PPU dots (`div3`), the reference
PPU (`ppu_ref`, nes/rtl/ppu_ref.v) with the gate-level `ppu_timing`, OAM
DMA (`oam_dma_ref`, nes/rtl/oam_dma_ref.v), a cartridge slot and the
Screen.

Root inputs: `reset` (the button), `vertical` (nametable mirroring of an
NROM cartridge, iNES flags 6 bit 0: 1 vertical, 0 horizontal; mappers with
mirroring control ignore it). `lxa_magic` is a jumper: the constant the
unstable opcodes ANE and LXA mix into A, which differs between real 2A03s
($FF here, as blargg's tests and most emulators expect; $EE is another
common chip); `magic_shown` displays it.

The cartridge `cart` is a chip with the same ports for every board:
`cart_nrom` (mapper 0) or `cart_mmc3` (mapper 4, `nes/rtl/mmc3_ref.v`);
swap with an `edit` `set` {"part": "cart", "use": ...}. Its memories are
`cart/prg_rom`, `cart/prg_ram` (8 KiB at $6000) and `cart/chr`.

  in:  a[16] rw d[8] ce (the CPU side, ce: the tick that commits a CPU
       cycle), pa[14] pwd[8] pwe (the PPU side), vertical
  out: dout[8] (read data for $6000-$FFFF), pd[8] (CHR data for PPU
       $0000-$1FFF), ciram_a10 (which 1 KiB of the console's nametable RAM
       PPU A10 selects), irq_n

CPU map (A15-A13 of the system bus): 000 `ram` (2 KiB, mirrored), 001 the
PPU's registers (A2-A0), 010 open bus ($4000-$5FFF: APU and controllers to
come; $4014 starts OAM DMA), 011 and 1xx the cartridge. The system bus is
the CPU's, or the DMA unit's while it copies. PPU map: $0000-$1FFF the
cartridge, $2000-$3EFF `vram` (2 KiB; the cartridge picks A10).
"""

import json
import sys

sys.path.insert(0, __file__.rsplit("/", 1)[0])
import cells  # noqa: E402
from cells import chip, gate, inp, out, unopt, use  # noqa: E402

cells.EDITS.clear()

# One CPU cycle per three dots: states 0, 1, 2; ce in state 2.
chip("div3",
     [gate("s0", "Delay"), gate("s1", "Delay"), gate("n", "Nor"), out("ce")],
     [["s0", "s1", "n.a"], ["s1", "n.b", "ce"], ["n", "s0"]],
     [{"name": "function", "sequence": {"steps": [
         {"ticks": 1, "expect": {"ce": 0}}, {"ticks": 1, "expect": {"ce": 1}},
         {"ticks": 1, "expect": {"ce": 0}}, {"ticks": 1, "expect": {"ce": 0}},
         {"ticks": 1, "expect": {"ce": 1}}]}}])

# Power-on reset: low for the first four ticks.
chip("por",
     [{"name": "one", "type": "Const", "value": 1}, gate("d", "Delay", 4), out("ok")],
     [["one", "d[0]"], ["d[0..3]", "d[1..4]"], ["d[3]", "ok"]],
     [{"name": "function", "sequence": {"steps": [
         {"ticks": 3, "expect": {"ok": 0}}, {"ticks": 1, "expect": {"ok": 1}}]}}])

CART_PORTS = [inp("a", 16), inp("rw"), inp("d", 8), inp("ce"), inp("pa", 14), inp("pwd", 8),
              inp("pwe"), inp("vertical"), out("dout", 8), out("pd", 8), out("ciram_a10"),
              out("irq_n")]
CART_COMMON = [
    {"name": "one", "type": "Const", "value": 1},
    {"name": "prg_ram", "type": "Ram", "addr_bits": 13, "w": 8},
    use("rd", "mux2x8"), gate("nrw", "Not"), gate("wk", "And"), gate("wkc", "And"),
    gate("npa13", "Not"), gate("cwe", "And"),
]
# Shared wiring: reads are ROM at A15, else the RAM; the RAM is written in
# its window ($6000-$7FFF, `wram`) on a CPU write; CHR is written (a CHR-RAM
# board) by a PPU write below $2000.
CART_COMMON_WIRES = [
    ["a:0..13", "prg_ram.addr"], ["d", "prg_ram.wdata"], ["prg_ram.data", "rd.A"], ["a:15", "rd.S"],
    ["rd.Y", "dout"], ["rw", "nrw"], ["nrw", "wk.b"], ["wk", "wkc.a"], ["ce", "wkc.b"],
    ["wkc", "prg_ram.we"], ["pa:13", "npa13"], ["npa13", "cwe.a"], ["pwe", "cwe.b"],
    ["cwe", "chr.we"], ["pwd", "chr.wdata"], ["chr.rdata0", "pd"],
]

# NROM: 32 KiB PRG (16 KiB loaded twice), 8 KiB CHR, mirroring by `vertical`.
chip("cart_nrom",
     CART_PORTS + CART_COMMON + [
         {"name": "prg_rom", "type": "Rom", "addr_bits": 15, "w": 8, "contents": []},
         {"name": "chr", "type": "Ram2", "addr_bits": 13, "w": 8, "reads": 1},
         use("w3", "decoder3"), use("mir", "mux2")],
     CART_COMMON_WIRES + [
         ["a:0..15", "prg_rom.addr"], ["prg_rom.data", "rd.B"],
         ["a:13..16", "w3.A"], ["one", "w3.En", "irq_n"], ["w3.Y:3", "wk.a"],
         ["pa:0..13", "chr.waddr", "chr.raddr0"],
         ["pa:11", "mir.A"], ["pa:10", "mir.B"], ["vertical", "mir.S"], ["mir.Y", "ciram_a10"]],
     [])

# MMC3: up to 512 KiB PRG and 256 KiB CHR; `prg_mask` and `chr_mask` are
# the board's bank counts - 1 (set when a ROM is loaded; SMB3: $1F, $7F).
chip("cart_mmc3",
     CART_PORTS + CART_COMMON + [
         {"name": "prg_rom", "type": "Rom", "addr_bits": 19, "w": 8, "contents": []},
         {"name": "chr", "type": "Ram2", "addr_bits": 18, "w": 8, "reads": 1},
         {"name": "prg_mask", "type": "Const", "w": 6, "value": 0x1F},
         {"name": "chr_mask", "type": "Const", "w": 8, "value": 0x7F},
         use("mmc3", "mmc3_ref")],
     CART_COMMON_WIRES + [
         ["a", "mmc3.a"], ["rw", "mmc3.rw"], ["d", "mmc3.d"], ["ce", "mmc3.ce"], ["pa", "mmc3.pa"],
         ["prg_mask", "mmc3.prg_mask"], ["chr_mask", "mmc3.chr_mask"],
         ["mmc3.prg_addr", "prg_rom.addr"], ["prg_rom.data", "rd.B"], ["mmc3.wram", "wk.a"],
         ["mmc3.chr_addr", "chr.waddr", "chr.raddr0"], ["mmc3.ciram_a10", "ciram_a10"],
         ["mmc3.irq_n", "irq_n"]],
     [])

# The standard controller: an 8-bit shift register. While `strobe` is high
# every bit loads its button; each `shift` (a read of $4016) moves the next
# button to `q`: A, B, Select, Start, Up, Down, Left, Right, then 1s.
BUTTONS = ["a", "b", "select", "start", "up", "down", "left", "right"]
# Keyboard keys for pad 1 while the simulation runs (`sim::keybind`): the
# usual emulator layout.
KEYS = {"a": "KeyX", "b": "KeyZ", "select": "ShiftRight", "start": "Enter",
        "up": "ArrowUp", "down": "ArrowDown", "left": "ArrowLeft", "right": "ArrowRight"}
chip("joypad",
     [inp(b) for b in BUTTONS] + [inp("strobe"), inp("shift"),
      {"name": "one", "type": "Const", "value": 1},
      use("sr", "dffe", 8), use("m", "mux2", 8), gate("en", "Or"), out("q")],
     [[b, f"m[{i}].B"] for i, b in enumerate(BUTTONS)] +
     [["strobe", "m.S", "en.a"], ["shift", "en.b"], ["en", "sr.En"], ["m.Y", "sr.D"],
      ["sr[1..8].Q", "m[0..7].A"], ["one", "m[7].A"], ["sr[0].Q", "q"]],
     [{"name": "read order", "sequence": {"steps": [
         # Start and Left held; strobe, then eight reads, then 1s.
         {"set": {"start": 1, "left": 1, "strobe": 1}, "ticks": 2, "expect": {"q": 0}},
         {"set": {"strobe": 0}, "ticks": 1, "expect": {"q": 0}},
         # shift reaches the register a tick after it is set: q is still A.
         {"set": {"shift": 1}, "ticks": 1, "expect": {"q": 0}},
         *[{"ticks": 1, "expect": {"q": v}} for v in (0, 0, 1, 0, 0, 1, 0, 1, 1)]]}},
      unopt(2000)])


def eqc(n, value):
    """`eq<n>_<value>`: A == value, an AND of the bits (inverted where 0)."""
    name = f"eq{n}_{value}"
    parts, wires, bits = [inp("A", n)], [], []
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
    chip(name, parts, wires, [{"name": "function", "formula": {"expect": {"Y": f"A == {value}"}}}, unopt()])
    return name


# $4016/$4017 inside $4000-$5FFF: A12..A1 = 0000 0000 1011.
IO_EQ = eqc(12, 0x4016 >> 1 & 0xFFF)

ONE = {"name": "one", "type": "Const", "value": 1}
root = [
    {"add": [
        inp("reset"), inp("vertical"), ONE, gate("zero", "Const"),
        *[dict(inp("p1_" + b), key=KEYS[b]) for b in BUTTONS],
        # Controllers: $4016 write bit 0 is the strobe; reads shift pad 1
        # ($4016) or read an empty port 2 ($4017), open bus in bits 7-5.
        use("pad1", "joypad"), use("io_eq", IO_EQ), gate("io", "And"), gate("na0", "Not"),
        gate("p1", "And"), gate("io_rd", "And"), gate("p1_rd", "And"), gate("p1_shift", "And"),
        gate("p1_wr", "And"), gate("p1_wrc", "And"), gate("nio_rd", "Not"), gate("s2", "And"),
        gate("p1_bit", "And"), use("strobe", "dffe"),
        {"name": "lxa_magic", "type": "Const", "w": 8, "value": 0xFF},
        {"name": "magic_shown", "type": "Display", "w": 8},
        use("por", "por"), gate("nrst", "Not"), gate("res", "And"),
        use("div", "div3"), use("cpu", "cpu6502"), use("dma", "oam_dma_ref"),
        # The system bus: the CPU's, or the DMA unit's while it owns it.
        use("bus_lo", "mux2x8"), use("bus_hi", "mux2x8"), use("bus_d", "mux2x8"), use("bus_rw", "mux2"),
        use("dec", "decoder3"), use("din", "sel8x8"), use("seen", "mux2x8"), use("latch", "reg8"),
        gate("cart_sel", "Or"),
        gate("wr", "Not"), gate("we0", "And"), gate("we0c", "And"), gate("cs", "And"),
        {"name": "ram", "type": "Ram", "addr_bits": 11, "w": 8},
        use("cart", "cart_nrom"),
        use("timing", "ppu_timing"), use("ppu", "ppu_ref"),
        {"name": "vram", "type": "Ram", "addr_bits": 11, "w": 8},
        use("pdm", "mux2x8"), gate("vwe", "And"),
        use("palette", "nes_palette"),
        {"name": "screen", "type": "Screen", "mode": "Scanline", "width": 256, "height": 256,
         "palette": "Rgb332", "scale": 2},
    ]},
    {"wire": [
        # Reset, clock enable, the CPU's pins.
        ["por.ok", "res.a"], ["reset", "nrst"], ["nrst", "res.b"], ["res", "cpu.res_n"],
        ["div.ce", "cpu.ce", "dma.ce", "cart.ce", "we0c.b", "cs.b", "latch.En"],
        ["one", "dec.En", "timing.en"], ["zero", "cpu.bcd_en"],
        ["dma.rdy", "cpu.rdy"], ["cart.irq_n", "cpu.irq_n"], ["ppu.nmi_n", "cpu.nmi_n"],
        ["lxa_magic", "cpu.magic", "magic_shown"],
        # OAM DMA watches the CPU's writes; it reads through the same data mux.
        ["cpu.addr", "dma.cpu_addr"], ["cpu.rw", "dma.cpu_rw"], ["cpu.dout", "dma.cpu_dout"],
        ["cpu.addr:0..8", "bus_lo.A"], ["dma.addr:0..8", "bus_lo.B"],
        ["cpu.addr:8..16", "bus_hi.A"], ["dma.addr:8..16", "bus_hi.B"],
        ["cpu.dout", "bus_d.A"], ["dma.dout", "bus_d.B"], ["cpu.rw", "bus_rw.A"], ["dma.rw", "bus_rw.B"],
        ["dma.own", "bus_lo.S", "bus_hi.S", "bus_d.S", "bus_rw.S"],
        # CPU map, on the system bus.
        ["bus_hi.Y:5..8", "dec.A"], ["bus_lo.Y", "ram.addr:0..8", "cart.a:0..8"],
        ["bus_hi.Y:0..3", "ram.addr:8..11"], ["bus_hi.Y", "cart.a:8..16"],
        ["bus_lo.Y:0..3", "ppu.ra"],
        ["bus_d.Y", "ram.wdata", "ppu.cpu_wd", "cart.d", "seen.A"],
        ["bus_rw.Y", "wr", "ppu.rw", "cart.rw", "seen.S"],
        ["dec.Y:0", "din.S0", "we0.a"], ["dec.Y:1", "din.S1", "cs.a"],
        # $4000-$5FFF: the controller ports, else open bus.
        ["bus_lo.Y:1..8", "io_eq.A:0..7"], ["bus_hi.Y:0..5", "io_eq.A:7..12"],
        ["dec.Y:2", "io.a", "s2.a"], ["io_eq.Y", "io.b"], ["bus_lo.Y:0", "na0"],
        ["io", "p1.a", "io_rd.a"], ["na0", "p1.b"], ["bus_rw.Y", "io_rd.b"],
        ["io_rd", "nio_rd", "din.S4"], ["nio_rd", "s2.b"], ["s2", "din.S2"],
        ["p1", "p1_rd.a", "p1_wr.a", "p1_bit.b"], ["io_rd", "p1_rd.b"],
        ["p1_rd", "p1_shift.a"], ["div.ce", "p1_shift.b", "p1_wrc.b"], ["p1_shift", "pad1.shift"],
        ["wr", "p1_wr.b"], ["p1_wr", "p1_wrc.a"], ["p1_wrc", "strobe.En"], ["bus_d.Y:0", "strobe.D"],
        ["strobe.Q", "pad1.strobe"], ["pad1.q", "p1_bit.a"],
        *[["p1_" + b, f"pad1.{b}"] for b in BUTTONS],
        ["p1_bit", "din.D4:0"], ["zero", "din.D4:1", "din.D4:2", "din.D4:3", "din.D4:4"],
        ["latch.Q:5..8", "din.D4:5..8"],
        ["dec.Y:3", "cart_sel.a"], ["bus_hi.Y:7", "cart_sel.b"], ["cart_sel", "din.S3"],
        ["zero", "din.S5", "din.S6", "din.S7"],
        ["ram.data", "din.D0"], ["ppu.cpu_rd", "din.D1"], ["latch.Q", "din.D2"], ["cart.dout", "din.D3"],
        ["din.Y", "cpu.din", "dma.din", "seen.B"], ["seen.Y", "latch.D"],
        ["wr", "we0.b"], ["we0", "we0c.a"], ["we0c", "ram.we"], ["cs", "ppu.cs"],
        # PPU map: the cartridge below $2000, the nametables above.
        ["timing.dot", "ppu.dot"], ["timing.line", "ppu.line"], ["ppu.rendering", "timing.skip"],
        ["ppu.pa", "cart.pa"], ["ppu.pwd", "cart.pwd", "vram.wdata"], ["ppu.pwe", "cart.pwe", "vwe.b"],
        ["vertical", "cart.vertical"],
        ["ppu.pa:0..10", "vram.addr:0..10"], ["cart.ciram_a10", "vram.addr:10"],
        ["ppu.pa:13", "pdm.S", "vwe.a"], ["cart.pd", "pdm.A"], ["vram.data", "pdm.B"],
        ["pdm.Y", "ppu.pd"], ["vwe", "vram.we"],
        # Video.
        ["ppu.colour", "palette.index"], ["palette.rgb", "screen.colour"],
        ["timing.hsync", "screen.hsync"], ["timing.vsync", "screen.vsync"],
    ]},
]
cells.EDITS.append({"circuit": "main", "ops": root})

if __name__ == "__main__":
    cells.emit(cells.EDITS)

# Generators of the `nes` workspace

The workspace is generated, never edited by hand: change a script, rebuild.
`./build.sh` writes every request into `nes/build/` (gitignored output);
then, through the gatequest MCP server, on a new workspace named `nes`:

| Step | Call | Makes |
|---|---|---|
| 1 | `edit {"file": "nes/build/cells.json"}` | the gate-level cell library (`cells.py`) |
| 2 | `edit {"file": "nes/build/probes.json"}` | `probe_<cell>` wrappers with `compiled` pins (`probes.py`) |
| 3 | `import_rtl {"file": "nes/build/cpu6502_ref.v", "into": "open", "top": "cpu6502_ref"}` | the reference CPU, generated from `nes/model` (`verilog.py`) |
| 4 | `edit {"file": "nes/build/alu.json"}` | the gate-level ALU and its blocks (`alu.py`) |
| 5 | `edit {"file": "nes/build/cpu.json"}` | `cpu6502`: the reference with gate blocks swapped in (`cpu.py`) |
| 6 | `edit {"file": "nes/build/tb.json"}` | `cpu_tb`, `cpu_tb_gates` (`tb.py`) |
| 7 | `edit {"file": "nes/build/video.json"}` | `ppu_timing`, `nes_palette`, the `video_test` demo (`video.py`) |
| 8 | `import_rtl {"file": "nes/rtl/ppu_ref.v", "into": "open", "top": "ppu_ref"}` | the reference PPU (hand-written: `nes/rtl/ppu_ref.v`) |
| 9 | `import_rtl {"file": "nes/rtl/mmc3_ref.v", "into": "open", "top": "mmc3_ref"}` | the MMC3 mapper logic |
| 10 | `import_rtl {"file": "nes/rtl/oam_dma_ref.v", "into": "open", "top": "oam_dma_ref"}` | the OAM DMA unit |
| 11 | `edit {"file": "nes/build/console.json"}` | the cartridges and the console in the root circuit (`console.py`) |

Then `test {}` (every stored test) and the suites in `nes/tests/`
(`cargo run --release --example nes_suite -- .gatequest/workspaces/nes nes/tests/harte_gates.json`).

A chip of the open workspace cannot be deleted or replaced yet, so a
changed chip means rebuilding from step 1 (or reopening the last save and
re-applying the steps after it).

To watch a ROM in the game, give the saved cartridge memories its contents
(the run tool's `load` only affects one simulation; an `edit` `set` with
`load` changes the part itself). For nestest (16 KiB PRG, mirrored):

```json
{"ops": [
  {"set": [{"part": "prg_rom", "load": {"file": "nes/roms/other/nestest.nes", "skip": 16, "length": 16384, "at": 0}}]},
  {"set": [{"part": "prg_rom", "load": {"file": "nes/roms/other/nestest.nes", "skip": 16, "length": 16384, "at": 16384}}]},
  {"set": [{"part": "chr", "load": {"file": "nes/roms/other/nestest.nes", "skip": 16400, "length": 8192, "at": 0}}]}
]}
```

and set the root input `vertical` to the header's mirroring (flags 6 bit 0;
nestest is horizontal, 0).

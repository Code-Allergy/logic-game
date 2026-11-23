#!/usr/bin/env python3
"""The committed NES workspace and its cartridges.

`nes/workspace/manifest.json` is the whole `nes` workspace (version 5, as
the editor saves it). The only ROM image in it is nestest's, in the NROM
cartridge, plugged in; the MMC3 cartridge is empty (other ROMs, such as a
cartridge dump, are never committed). This script moves between it and a
playable copy.

    # A playable copy for the game (or the suite runner), with a ROM in it:
    python3 nes/gen/cartridge.py install [--into .gatequest/workspaces/nes]
    python3 nes/gen/cartridge.py insert .gatequest/workspaces/nes nes/roms/smb3.nes

    # After changing the workspace (MCP edits, a rebuild): update the
    # committed copy from a saved one, its cartridges emptied and nestest
    # put back in.
    python3 nes/gen/cartridge.py strip .gatequest/workspaces/nes --rom nes/roms/other/nestest.nes

`insert` reads the iNES header: mapper 0 plugs `cart_nrom` into the slot
(16 KiB of PRG is mirrored to 32 KiB), mapper 4 `cart_mmc3` (with its bank
masks set from the ROM's size). The other cartridge is emptied, so a
workspace holds one ROM. An NROM game's mirroring is the root input
`vertical` (iNES flags 6 bit 0), which is set in the editor or by a
suite config, not stored in the workspace.
"""

import argparse
import json
import os
import shutil
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
COMMITTED = os.path.normpath(os.path.join(HERE, "../workspace"))
CARTS = {0: "cart_nrom", 4: "cart_mmc3"}
# (circuit, part label, field): the memories that hold a ROM image.
ROM_MEMORIES = [(cart, part, field) for cart in CARTS.values()
                for part, field in (("prg_rom", "contents"), ("chr", "init"))]


def load(folder):
    with open(os.path.join(folder, "manifest.json")) as f:
        return json.load(f)


def save(folder, doc):
    os.makedirs(folder, exist_ok=True)
    path = os.path.join(folder, "manifest.json")
    tmp = path + ".tmp"
    with open(tmp, "w") as f:
        json.dump(doc, f, indent=2)
        f.write("\n")
    os.replace(tmp, path)


def circuit(doc, name):
    for c in doc["circuits"]:
        if c["name"] == name:
            return c
    raise SystemExit(f"the workspace has no circuit \"{name}\"")


def part(c, label):
    for n in c["nodes"]:
        if n.get("label") == label:
            return n
    raise SystemExit(f"circuit \"{c['name']}\" has no part \"{label}\"")


def root(doc):
    return next(c for c in doc["circuits"] if c["id"] == doc["root"])


def empty_carts(doc):
    for cart, label, field in ROM_MEMORIES:
        part(circuit(doc, cart), label)["kind"][field] = []


def strip(args):
    doc = load(args.workspace)
    for cart in CARTS.values():
        circuit(doc, cart)  # both cartridges must be there
    empty_carts(doc)
    doc["name"] = "nes"
    save(COMMITTED, doc)
    print(f"wrote {COMMITTED}/manifest.json (cartridges empty)")
    if args.rom:
        args.workspace = COMMITTED
        insert(args)


def install(args):
    if os.path.exists(os.path.join(args.into, "manifest.json")) and not args.force:
        raise SystemExit(f"{args.into} exists; pass --force to replace it (its .bak is kept)")
    if os.path.exists(os.path.join(args.into, "manifest.json")):
        shutil.copy(os.path.join(args.into, "manifest.json"), os.path.join(args.into, "manifest.json.bak"))
    save(args.into, load(COMMITTED))
    print(f"installed the committed workspace in {args.into} (no ROM yet: use insert)")


def insert(args):
    rom = open(args.rom, "rb").read()
    if rom[:4] != b"NES\x1a":
        raise SystemExit(f"{args.rom} is not an iNES file")
    mapper = (rom[6] >> 4) | (rom[7] & 0xF0)
    if mapper not in CARTS:
        raise SystemExit(f"mapper {mapper} has no cartridge yet (there are: {sorted(CARTS)})")
    prg_len, chr_len = rom[4] * 16384, rom[5] * 8192
    prg = list(rom[16:16 + prg_len])
    chr_ = list(rom[16 + prg_len:16 + prg_len + chr_len])
    doc = load(args.workspace)
    empty_carts(doc)
    cart = circuit(doc, CARTS[mapper])
    if mapper == 0:
        if prg_len not in (16384, 32768) or chr_len > 8192:
            raise SystemExit("NROM is 16 or 32 KiB of PRG and at most 8 KiB of CHR")
        prg = prg * (32768 // prg_len)
    else:
        if prg_len > 512 * 1024 or chr_len > 256 * 1024:
            raise SystemExit("MMC3 is at most 512 KiB of PRG and 256 KiB of CHR")
        part(cart, "prg_mask")["kind"]["value"] = prg_len // 8192 - 1
        part(cart, "chr_mask")["kind"]["value"] = max(chr_len // 1024, 1) - 1
    part(cart, "prg_rom")["kind"]["contents"] = prg
    part(cart, "chr")["kind"]["init"] = chr_
    slot = part(root(doc), "cart")
    slot["kind"]["definition"] = cart["id"]
    save(args.workspace, doc)
    note = ""
    if mapper == 0:
        note = f"; set the root input `vertical` to {rom[6] & 1} (its mirroring)"
    print(f"{os.path.basename(args.rom)}: mapper {mapper}, {prg_len // 1024} KiB PRG, "
          f"{chr_len // 1024} KiB CHR, in {CARTS[mapper]}{note}")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("strip", help="update nes/workspace from a saved workspace, cartridges emptied")
    s.add_argument("workspace")
    s.add_argument("--rom", help="then insert this ROM (nestest) into the committed copy")
    s.set_defaults(fn=strip)
    i = sub.add_parser("install", help="copy nes/workspace to a workspace folder")
    i.add_argument("--into", default=".gatequest/workspaces/nes")
    i.add_argument("--force", action="store_true")
    i.set_defaults(fn=install)
    r = sub.add_parser("insert", help="put an iNES ROM into a workspace's cartridge")
    r.add_argument("workspace")
    r.add_argument("rom")
    r.set_defaults(fn=insert)
    args = ap.parse_args()
    args.fn(args)


if __name__ == "__main__":
    sys.exit(main())

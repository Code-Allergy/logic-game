# GateQuest

A logic-circuit sandbox built with [Bevy](https://bevyengine.org). The
simulator is the `logi-sim` crate in this workspace.

## Building and running

```sh
cargo run -p gate_quest            # default build
cargo dev                          # alias: cargo run -p gate_quest --features dev
cargo test -p gate_quest --lib     # app integration tests (headless)
cargo test -p logi-sim             # simulator tests
trunk serve                        # web build (wasm32-unknown-unknown)
```

The Nix flake (`nix develop`, or direnv with `.envrc`) provides the
toolchain and system libraries.

### Cargo features

The default build is what releases and the web build use. For native
development, enable:

| Feature | Effect |
| --- | --- |
| `dev` | `bevy/dynamic_linking` (much faster incremental links) plus `inspector`. Native only. |
| `inspector` | The egui world inspector window. |

For example `cargo run -p gate_quest --features dev`, or
`--features inspector` for the inspector without dynamic linking.

## Look and feel

The canvas and panels use the "Studio Workshop" design: chunky tiles with
an ink outline and a solid bottom edge, cased wires (an ink casing around a
coloured core; marigold for on, slate for off, teal for buses with a value
pill), on a dot-grid bench. Wires are routed on the grid in horizontal and
vertical runs with rounded corners, a dot where a net branches and a gap
where one wire passes over another (`src/sim/routing.rs`). Every colour comes from the `Theme` resource
(`src/theme.rs`), with a day and a night palette; press `T` or use the
sun/moon button on the file card to switch.

## The editor

The canvas fills the window; everything else floats over it as cards,
anchored to the window edges (`HudLayout` in `src/ui_style.rs`, compact
below 1600 x 900):

| Where | Card |
|---|---|
| Top left | File card: workspace or demo name, origin and view path, saved/unsaved/read-only. `☰ Library` opens the drawer with save, new, import RTL, copy a demo, the demos, gallery, saved workspaces and the definition table. |
| Top centre | Transport: run, pause, step, reset (back to tick 0, inputs kept), speed chips (1 Hz, 60 Hz, 1 kHz, Max; `…` for an exact rate), tick. |
| Top right | Build status: built and part count, or the loop/build error (click for details). |
| Right | Inspector of the selected part: label, width, parameters, live values (Q/D/EN for a register), input drive, readout format, Watch, Open inside, Delete, wires. |
| Bottom left | Minimap (click or drag to move the view) and zoom; the percentage fits the circuit. |
| Bottom centre | Parts bar: Logic, Memory, In & out, Routing, Arithmetic and Chips (saved workspaces, demos and gallery items as cells); the width of new parts; the wire tool. |
| Bottom right | Signals: live values of watched parts, inputs, clocks, registers and outputs. |

Keys (ignored while a text field has focus): `1`–`9` arm a part from the
open tab (then click the canvas, or drag a slot onto it), `Esc` drops it
(or the wire tool, or closes the Library; with nothing to drop it clears
the selection), `Space` run or pause, `W` wire tool, `F` fit, `R` reset to
tick 0, arrow keys pan, `Delete` deletes the selected part (or parts),
`Ctrl+G` makes a chip of the selected parts, `T` day or night, `Ctrl+S`
saves, `Ctrl+Z` undoes, `Ctrl+Shift+Z` or `Ctrl+Y` redoes (`Cmd` on
macOS). Mouse: left-click selects (on empty canvas: deselects) and flips
a 1-bit input (also in a read-only demo; a wider input is set in the
inspector), `Shift`+click adds a part to the selection or takes it out,
`Shift`+drag on empty canvas selects the parts in a box, left- or
right-drag moves a part (a right drag on a selected group moves the
group; on a 10-unit snap grid, half the wire grid, so pins line up; new
parts land on it too), a left drag from a pin to a pin makes a wire
(`Esc` drops it; with `Shift` the press selects instead), middle-drag
pans, the wheel zooms about the
cursor.

`cargo run --example screenshot -- <id> out.png [--night] [--size WxH]
[--select LABEL] [--set LABEL=VALUE] [--tab NAME] [--library] [--editable]
[--fit] [--definition NAME] [--confirm]` saves a screenshot of a demo, a gallery item
(or `empty`, a new workspace, or `loop`, a build error) or a version 6
file (optionally one of its definitions); `--map [--opened ID,ID,...]
[--level N]` shows the demo map instead, with that progress (in memory);
`--completed ID,...` marks challenges done. `--challenge ID` opens a
challenge (the first argument `-` for its starter, or a circuit file in its
place) and `--check` presses Check. `--confirm` shows the unsaved-changes
question. `--drag LABEL:DX,DY` drags a part through the real pointer path.
`--multi LABEL,...` selects several parts (the selection card), `--make-chip
NAME` makes them a chip and `--open-chip` opens it. `--wire FROM.PIN:TO.PIN`
drags a wire (`--hold` keeps the button down for the shot) and `--hover
LABEL.PIN` points at a pin. `--ui-scale auto|90|100|115|130` sets the UI
size. Run it with
`GATEQUEST_DATA_DIR` set to a scratch folder.

## Saving

Workspaces are saved in the player's data folder, not the folder the game
was started from:

| System | Folder |
|---|---|
| Linux and other Unix | `$XDG_DATA_HOME/gatequest/workspaces`, else `~/.local/share/gatequest/workspaces` |
| macOS | `~/Library/Application Support/GateQuest/workspaces` |
| Windows | `%APPDATA%\GateQuest\workspaces` |

`GATEQUEST_DATA_DIR=<folder>` overrides it (workspaces go in
`<folder>/workspaces`; tests and tools use a scratch folder). Without any
of these variables the game falls back to `./workspaces`. Earlier versions
saved to `./workspaces` in the launch folder: when that folder exists, its
workspaces (and demo progress) are still listed and opened, and the next
save of one writes it to the new folder (the old copy is left alone).

- Each workspace is a folder `<name>/manifest.json`; every save writes a
  temporary file, flushes it to disk and renames it into place, keeping
  the previous save as `manifest.json.bak`.
- Names are made safe for every system: no `/ \ : * ? " < > |`, no
  leading dot (so no `..`), no trailing dot or space, no Windows device
  names (`CON`, `NUL`, `COM1`, ...). Names that differ only in case are
  the same workspace.
- Save in the Library (or `Ctrl+S`) saves the open workspace. Typing
  another name makes it "Save as new": a copy with its own identity
  (chips are keyed by it), which becomes the open workspace; the old one
  stays as it was. A save never replaces a different workspace of the
  same name: pick another name, or load that one.
- "Copy to my workspace" saves the copy at once as `<title> (copy)`, then
  `(copy 2)`, ...; a new empty workspace is `workspace`, `workspace (2)`,
  ...
- Unlabelled Inputs, Outputs and Readouts are named `in0`, `out0`,
  `display0`, ... in the saved file.
- Opening something else (a demo, a challenge, a saved workspace, New,
  Import RTL, a copy) over unsaved changes asks first: Save, Discard or
  Cancel.
- Closing the window or Exit saves unsaved changes to the open
  workspace (under a free name if a different workspace has taken its
  name). Read-only demos and untouched or never-saved empty workspaces are
  not saved.
- The Library's Display section sets the UI size (Auto, 90% to 130%);
  it and the theme are kept in `.settings.json` in the workspaces folder,
  next to the demo progress.
- The web build has no file system yet: Save, Load and saved chips are
  hidden there, and a copied demo is editable for the session only.

## The demo map

A full-screen view of the curriculum as a road from one gate to a CPU,
opened from the map button on the file card, "Open the demo map" in the
Library, or "Demo map" on the title menu; `Esc` or "Back to workshop"
returns to the editor. The app still starts on the title menu.

- The road has one stop per level, laid out from the level count: marigold
  once a demo of the level has been opened, sky with a ring and a "You are
  here" pin at the level of the demo opened last, white before that. Click
  a stop to pick its level. The progress card counts the opened demos and
  names the next stop (the first stop after yours with a demo left).
- The level panel shows the level's description (`levels[].description`
  in the manifest) and its demos with thumbnails drawn from the circuit,
  a visited box (click to tick or clear), "Open" and "Copy to my
  workshop". Below them, its **challenges** (see below).
- The gallery shelf filters by category; click a card to open it.

Opened demos are recorded whenever a demo or gallery item is open, however
it was opened, and so are passed challenges. Native builds keep the record
in `.demo-progress.json` in the workspaces folder (see Saving;
`{"format": "gatequest-progress", "version": 2, "opened": [...], "last":
"...", "completed": [...]}`, written atomically with a `.bak` of the
previous one; version 1 files still load; a file that cannot be read is
kept as `.demo-progress.json.corrupt` before starting over; hidden files
are not listed as workspaces). The web build keeps it in memory for the
session.

### Challenges

Levels 1 to 7 each have one or two build-it challenges (eleven in all:
AND and XOR from NAND only, a half and a full adder, an 8-bit adder, a
multiplexer and a 2-to-4 decoder, an SR latch, a D latch, a 4-bit
register and a 4-bit counter). They are listed in the level panel under
the demos and in the Library; a stop with challenges wears a "done/total"
pill, and the progress card counts them.

"Build it" opens the challenge as your own editable workspace, named
`<title> (challenge)`, with its input and output ports placed and
labelled (or your saved work for it, if you saved or quit before). The
challenge card under the file card shows the brief and the parts rule;
parts outside the rule are greyed out in the parts bar. **Check** tests
the circuit on a separate simulation, so it is instant and leaves the
running one alone, and shows "Passed" or the first failing case: its
inputs, the expected outputs and what the circuit gave (or a part that is
not allowed, a missing port, a loop). A pass ticks the challenge on the
map. "Start over" (click twice) goes back to the bare ports. Challenges
are defined in `assets/demos/manifest.json` with a starter file each; see
[docs/save-format.md](docs/save-format.md#challenges).

## Documentation

- `docs/sim-v2-design.md`: simulator design and implementation notes.
- `docs/app-simulation-bridge.md`: how the app maps onto the simulator.
- `logi-sim/ARCHITECTURE.md`: the simulator crate as built, with benchmarks.
- `logi-sim/PROFILING.md`: benchmarking and profiling the simulator.
- `docs/save-format.md`: workspace file format (version 5, and the
  hand-editable version 6 the demos are written in), and the demo manifest.

## Demos

The editor's Demos list climbs from gates to a CPU, each level built from
instances of the ones below. The demos are files in `assets/demos/`
(`manifest.json` gives the order); they open read-only, and "Copy to my
workspace" makes an editable copy.

| Level | Demos |
|---|---|
| 1 Gates | Logic gates (NOT, AND, OR, XOR, NAND, NOR); NAND and NOR are universal |
| 2 Adding bits | Half adder; full adder |
| 3 Adding words | 8-bit ripple-carry adder (beside `Add`) |
| 4 Choosing | 2-to-4 decoder; 2-to-1 multiplexer (beside `Mux`); mux tree |
| 5 Feedback | SR latch from two NORs and a Delay (beside `SrLatch`) |
| 6 Latches and flip-flops | D latch (beside `DLatch`); rising-edge D flip-flop |
| 7 Registers and counters | 8-bit register (beside `Register`); counter (beside a word-level one) |
| 8 Register file | 4 x 8-bit register file, one write and two read ports |
| 9 ALU | 8-bit ALU: add, subtract, AND, OR, carry and zero flags |
| 10 A small CPU | Program counter, ROM program, microcode, register file and ALU; multiplies 5 by 7 |

A **Gallery** of 29 more circuits sits beside the curriculum in
`assets/gallery/`: combinational (comparators, parity, Gray code, priority
encoder, 7-segment display, barrel shifter, carry-lookahead adder, array
multiplier), sequential (shift register, LFSR, counters, debouncer, PWM,
state machines, traffic light, stopwatch) and small systems (a stack, a
UART transmitter, a reaction-timer game, a CPU printing Fibonacci
numbers, and a 16-bit CPU of about 15 000 gates printing primes beside
its twin of word primitives). See `docs/save-format.md` ("Gallery").

`cargo run --example demo_fmt` rewrites the demo and gallery files in
canonical form (`-- --check` checks them); `cargo run --example
render_svg -- <id>` draws SVG previews of a demo's or gallery item's
circuits (`--ortho` routes the wires as the editor does).

## Credits

Fonts bundled in `assets/fonts/` (embedded in the binary), both under the
SIL Open Font License 1.1:

- [Nunito](https://github.com/googlefonts/nunito), Copyright 2014 The
  Nunito Project Authors (SemiBold and ExtraBold). Licence:
  `assets/fonts/OFL-Nunito.txt`.
- [JetBrains Mono](https://github.com/JetBrains/JetBrainsMono), Copyright
  2020 The JetBrains Mono Project Authors (Bold). Licence:
  `assets/fonts/OFL-JetBrainsMono.txt`.

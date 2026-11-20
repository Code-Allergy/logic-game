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
pill), on a dot-grid bench. Every colour comes from the `Theme` resource
(`src/theme.rs`), with a day and a night palette; press `T` or use the
Day/Night card to switch. `cargo run --example screenshot -- <id> out.png
[--night]` saves a screenshot of a demo or gallery item.

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

A **Gallery** of 27 more circuits sits beside the curriculum in
`assets/gallery/`: combinational (comparators, parity, Gray code, priority
encoder, 7-segment display, barrel shifter, carry-lookahead adder, array
multiplier), sequential (shift register, LFSR, counters, debouncer, PWM,
state machines, traffic light, stopwatch) and small systems (a stack, a
UART transmitter, a reaction-timer game and a CPU printing Fibonacci
numbers). See `docs/save-format.md` ("Gallery").

`cargo run --example demo_fmt` rewrites the demo and gallery files in
canonical form (`-- --check` checks them); `cargo run --example
render_svg -- <id>` draws SVG previews of a demo's or gallery item's
circuits.

## Credits

Fonts bundled in `assets/fonts/` (embedded in the binary), both under the
SIL Open Font License 1.1:

- [Nunito](https://github.com/googlefonts/nunito), Copyright 2014 The
  Nunito Project Authors (SemiBold and ExtraBold). Licence:
  `assets/fonts/OFL-Nunito.txt`.
- [JetBrains Mono](https://github.com/JetBrains/JetBrainsMono), Copyright
  2020 The JetBrains Mono Project Authors (Bold). Licence:
  `assets/fonts/OFL-JetBrainsMono.txt`.

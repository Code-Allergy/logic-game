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

## Documentation

- `docs/sim-v2-design.md`: simulator design and implementation notes.
- `docs/app-simulation-bridge.md`: how the app maps onto the simulator.
- `logi-sim/ARCHITECTURE.md`: the simulator crate as built, with benchmarks.
- `logi-sim/PROFILING.md`: benchmarking and profiling the simulator.
- `docs/save-format.md`: workspace file format.

{
  description = "Logi-game development environment with dependencies";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs = {
        nixpkgs.follows = "nixpkgs";
      };
    };
  };

  outputs = {
    self,
    nixpkgs,
    flake-utils,
    rust-overlay,
  }:
    flake-utils.lib.eachDefaultSystem (
      system: let
        overlays = [(import rust-overlay)];
        pkgs = import nixpkgs {
          inherit system overlays;
          config = {
            allowUnfree = true;
            android_sdk.accept_license = true;
          };
        };

        # Use the latest nightly Rust
        rustVersion = pkgs.rust-bin.nightly.latest.default.override {
          extensions = [
            "rust-std"
            "rust-src"
            "llvm-tools-preview"
          ];
          targets = [
            "x86_64-unknown-linux-gnu"
            "x86_64-pc-windows-gnu"
            "wasm32-unknown-unknown"
          ];
        };
        androidPkgs = pkgs.androidenv.androidPkgs;
      in {
        devShells.default = pkgs.mkShell {
          packages = [
            pkgs.perf
            pkgs.rust-bindgen
            pkgs.clang
            pkgs.lld
            pkgs.pkg-config
            pkgs.alsa-lib
            pkgs.libudev-zero
            pkgs.libX11
            pkgs.libXrandr
            pkgs.libXcursor
            pkgs.libXi
            pkgs.libxkbcommon
            pkgs.trunk
            # Verilog import through the MCP server (docs/mcp-server.md).
            pkgs.yosys
            pkgs.wayland
            pkgs.vulkan-loader
          ];

          nativeBuildInputs = [
            pkgs.pkg-config
          ];

          buildInputs = [
            rustVersion
            pkgs.cargo-flamegraph
            pkgs.perf
            pkgs.cargo-pgo
          ];

          LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath [
            pkgs.pkg-config
            pkgs.libudev-zero
            pkgs.alsa-lib
            pkgs.libX11
            pkgs.libXrandr
            pkgs.libXcursor
            pkgs.libXi
            pkgs.libxkbcommon
            pkgs.libglvnd
            pkgs.wayland
            pkgs.vulkan-loader
          ];

          RUST_BACKTRACE = 1;

          # Optional: Add shellHook for additional setup
          shellHook = ''
            echo "Rust development environment loaded!"
            echo "Rust version: $(rustc --version)"
          '';
        };
      }
    );
}

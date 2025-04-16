{
  description = "Logi-game development environment with dependencies";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs = {
        nixpkgs.follows = "nixpkgs";
        flake-utils.follows = "flake-utils";
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
        };

        # Use the latest nightly Rust
        rustVersion = pkgs.rust-bin.nightly.latest.default.override {
          extensions = ["rust-std" "llvm-tools-preview"];
          targets = ["x86_64-unknown-linux-gnu" "wasm32-unknown-unknown"];
        };
      in {
        devShells.default = pkgs.mkShell {
          packages = [
            pkgs.linuxPackages_latest.perf
            pkgs.clang
            pkgs.lld
            pkgs.pkg-config
            pkgs.alsa-lib
            pkgs.libudev-zero
            pkgs.xorg.libX11
            pkgs.xorg.libXrandr
            pkgs.xorg.libXcursor
            pkgs.xorg.libXi
            pkgs.libxkbcommon
          ];

          nativeBuildInputs = [
            pkgs.pkg-config
          ];

          buildInputs = [
            rustVersion
            pkgs.cargo-pgo
          ];

          LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath [
            pkgs.pkg-config
            pkgs.libudev-zero
            pkgs.alsa-lib
            pkgs.xorg.libX11
            pkgs.xorg.libXrandr
            pkgs.xorg.libXcursor
            pkgs.xorg.libXi
            pkgs.libxkbcommon
            pkgs.libglvnd
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

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
          extensions = ["rust-std" "llvm-tools-preview"];
          targets = ["x86_64-unknown-linux-gnu" "x86_64-pc-windows-gnu" "wasm32-unknown-unknown"];
        };
        androidPkgs = pkgs.androidenv.androidPkgs;
      in {
        devShells.default = pkgs.mkShell {
          packages = [
            pkgs.linuxPackages_latest.perf
            #pkgs.pkgsCross.mingwW64.buildPackages.gcc
            #pkgs.pkgsCross.mingwW64.windows.pthreads
            pkgs.rust-bindgen
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
            pkgs.trunk
            pkgs.cargo-apk
            pkgs.cargo-ndk
            # androidPkgs.androidsdk
            # androidPkgs.platform-tools
            # androidPkgs.tools
            # androidPkgs.ndk-bundle
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
            export ANDROID_SDK_ROOT=${androidPkgs.androidsdk}/libexec/android-sdk
            export ANDROID_NDK_ROOT=${androidPkgs.ndk-bundle}
            export PATH=$PATH:${androidPkgs.platform-tools}/libexec/platform-tools
          '';
        };
      }
    );
}

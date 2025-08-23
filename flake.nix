{
  description = "ergot";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

    crane = {
      url = "github:ipetkov/crane";
    };

    flake-utils.url = "github:numtide/flake-utils";

    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = {
    nixpkgs,
    crane,
    flake-utils,
    rust-overlay,
    ...
  }:
    flake-utils.lib.eachDefaultSystem (
      system: let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [(import rust-overlay)];
        };
        nightlyCraneLib = (crane.mkLib pkgs).overrideToolchain (
          p:
            p.rust-bin.nightly.latest.default.override {
              extensions = ["rust-src" "miri"];
              targets = [
                "x86_64-unknown-linux-gnu"
              ];
            }
        );
        stableCraneLib = (crane.mkLib pkgs).overrideToolchain (
          p:
            p.rust-bin.stable.latest.default.override {
              targets = [
                "riscv32imac-unknown-none-elf"
                "thumbv8m.main-none-eabihf"
                "thumbv7em-none-eabihf"
                "thumbv7em-none-eabi"
                "thumbv6m-none-eabi"
                "aarch64-apple-darwin"
                "aarch64-unknown-linux-gnu"
                "x86_64-apple-darwin"
                "x86_64-unknown-linux-gnu"
              ];
            }
        );
      in {
        devShells = builtins.mapAttrs (_: value:
          value.devShell {
            packages = [
              pkgs.probe-rs-tools
              pkgs.picotool
            ];
          }) {
          default = stableCraneLib;
          nightly = nightlyCraneLib;
        };
      }
    );
}

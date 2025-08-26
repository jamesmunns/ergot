{
  description = "ergot";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

    crane = {
      url = "github:ipetkov/crane";
    };

    flake-utils.url = "github:numtide/flake-utils";

    probe-rs = {
      url = "github:probe-rs/probe-rs";
      flake = false;
    };

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
    probe-rs,
    ...
  }:
    flake-utils.lib.eachDefaultSystem (
      system: let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [
            (import rust-overlay)
            (final: prev: {
              probe-rs-tools = pkgs.rustPlatform.buildRustPackage rec {
                pname = "probe-rs-tools";
                version = "0.29.1";

                src = probe-rs;
                cargoHash = "sha256-EQSfZS3PSwCG+UZRc1du0ymA15K0jxb09NavsEAOU7o=";

                buildAndTestSubdir = pname;

                nativeBuildInputs = [
                  pkgs.cmake
                  pkgs.pkg-config
                ];

                buildInputs = [
                  pkgs.libusb1
                  pkgs.openssl
                ];

                checkFlags = [
                  # require a physical probe
                  "--skip=cmd::dap_server::server::debugger::test::attach_request"
                  "--skip=cmd::dap_server::server::debugger::test::attach_with_flashing"
                  "--skip=cmd::dap_server::server::debugger::test::disassemble::instructions_after_and_not_including_the_ref_address"
                  "--skip=cmd::dap_server::server::debugger::test::disassemble::instructions_before_and_not_including_the_ref_address_multiple_locations"
                  "--skip=cmd::dap_server::server::debugger::test::disassemble::instructions_including_the_ref_address_location_cloned_from_earlier_line"
                  "--skip=cmd::dap_server::server::debugger::test::disassemble::negative_byte_offset_of_exactly_one_instruction_aligned_"
                  "--skip=cmd::dap_server::server::debugger::test::disassemble::positive_byte_offset_that_lands_in_the_middle_of_an_instruction_unaligned_"
                  "--skip=cmd::dap_server::server::debugger::test::launch_and_threads"
                  "--skip=cmd::dap_server::server::debugger::test::launch_with_config_error"
                  "--skip=cmd::dap_server::server::debugger::test::test_initalize_request"
                  "--skip=cmd::dap_server::server::debugger::test::test_launch_and_terminate"
                  "--skip=cmd::dap_server::server::debugger::test::test_launch_no_probes"
                  "--skip=cmd::dap_server::server::debugger::test::wrong_request_after_init"
                  # compiles an image for an embedded target which we do not have a toolchain for
                  "--skip=util::cargo::test::get_binary_artifact_with_cargo_config"
                  "--skip=util::cargo::test::get_binary_artifact_with_cargo_config_toml"
                  # requires other crates in the workspace
                  "--skip=util::cargo::test::get_binary_artifact"
                  "--skip=util::cargo::test::library_with_example_specified"
                  "--skip=util::cargo::test::multiple_binaries_in_crate_select_binary"
                  "--skip=util::cargo::test::workspace_binary_package"
                  "--skip=util::cargo::test::workspace_root"
                ];
              };
            })
          ];
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

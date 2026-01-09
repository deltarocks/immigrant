{
  description = "Immigrant: Database schema description language";
  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/release-25.11";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    flake-parts = {
      url = "github:hercules-ci/flake-parts";
      inputs.nixpkgs-lib.follows = "nixpkgs";
    };
    crane.url = "github:ipetkov/crane";
    shelly.url = "github:CertainLach/shelly";
  };
  outputs =
    inputs@{
      nixpkgs,
      flake-parts,
      shelly,
      rust-overlay,
      crane,
      ...
    }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      systems = nixpkgs.lib.systems.flakeExposed;
      imports = [ shelly.flakeModule ];
      perSystem =
        {
          config,
          system,
          pkgs,
          ...
        }:
        let
          rust = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
          craneLib = (crane.mkLib pkgs).overrideToolchain rust;
          sharedDeps = with pkgs; [
            # PG parser
            rustPlatform.bindgenHook
            cmake
          ];
        in
        rec {
          _module.args.pkgs = import nixpkgs {
            inherit system;
            overlays = [ rust-overlay.overlays.default ];
          };
          packages = {
            default = packages.immigrant;
            immigrant = craneLib.buildPackage {
              pname = "immigrant";
              src = craneLib.cleanCargoSource (craneLib.path ./.);
              strictDeps = true;
              nativeBuildInputs = sharedDeps;
            };
          };
          shelly.shells.default = {
            factory = craneLib.devShell;
            packages =
              with pkgs;
              [
                cargo-edit
                just
              ]
              ++ sharedDeps;
          };
          formatter = pkgs.alejandra;
        };
    };
}

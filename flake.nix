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
          lib,
          ...
        }:
        let
          rust = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
          craneLib = (crane.mkLib pkgs).overrideToolchain rust;
          sharedDeps = with pkgs; [
            # PG parser
            rustPlatform.bindgenHook
            cmake
            libpq
          ];
        in
        rec {
          _module.args.pkgs = import nixpkgs {
            inherit system;
            overlays = [ rust-overlay.overlays.default ];
          };
          packages = let
            root = ./.;
            src = lib.fileset.toSource {
              inherit root;
              fileset = lib.fileset.unions [
                (craneLib.fileset.commonCargoSources root)
                (lib.fileset.fileFilter (file: file.hasExt "schema") root)
              ];
            };
          in {
            default = packages.immigrant;
            immigrant = craneLib.buildPackage {
              pname = "immigrant";
              inherit src;
              strictDeps = true;
              nativeBuildInputs = sharedDeps;
            };
            immigrant-web = let
              craneLibWasm = craneLib.overrideToolchain (rust.override {
                targets = [ "wasm32-unknown-unknown" ];
              });
              cargoArtifacts = craneLibWasm.buildDepsOnly {
                inherit src;
                pname = "immigrant-web-deps";
                doCheck = false;
                cargoExtraArgs = "--lib -p immigrant-web --target wasm32-unknown-unknown";
                nativeBuildInputs = sharedDeps;
              };
              wasmLib = craneLibWasm.buildPackage {
                inherit src cargoArtifacts;
                pname = "immigrant-web";
                doCheck = false;
                cargoExtraArgs = "--lib -p immigrant-web --target wasm32-unknown-unknown";
                nativeBuildInputs = sharedDeps;
                installPhaseCommand = ''
                  mkdir -p $out/lib
                  cp target/wasm32-unknown-unknown/release/immigrant_web.wasm $out/lib/
                '';
              };
            in pkgs.stdenv.mkDerivation {
              pname = "immigrant-web";
              version = "0.2.0";
              dontUnpack = true;
              nativeBuildInputs = [ pkgs.wasm-bindgen-cli ];
              buildPhase = ''
                wasm-bindgen ${wasmLib}/lib/immigrant_web.wasm \
                  --out-dir $out \
                  --target web \
                  --out-name web
              '';
              installPhase = "true";
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
          formatter = pkgs.nixfmt;
        };
    };
}

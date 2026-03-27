{
  description = "vitobar — a custom Wayland bar for Niri";

  inputs = {
    nixpkgs.url     = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay    = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, rust-overlay, ... }:
  let
    system = "x86_64-linux";
    pkgs   = import nixpkgs {
      inherit system;
      overlays = [ rust-overlay.overlays.default ];
    };

    rustToolchain = pkgs.rust-bin.stable.latest.default.override {
      extensions = [ "rust-src" "rust-analyzer" ];
    };
  in
  {
    devShells.${system}.default = pkgs.mkShell {
      buildInputs = with pkgs; [
        rustToolchain
        pkg-config
        wayland
        wayland-protocols
        libxkbcommon
      ];

      shellHook = ''
        echo "vitobar dev shell ready"
        echo "run: cargo build"
      '';
    };

    packages.${system}.default = pkgs.rustPlatform.buildRustPackage {
      pname   = "vitobar";
      version = "0.1.0";
      src     = ./.;

      cargoLock.lockFile = ./Cargo.lock;

      nativeBuildInputs = with pkgs; [ pkg-config ];
      buildInputs       = with pkgs; [
        wayland
        wayland-protocols
        libxkbcommon
      ];
    };
  };
}

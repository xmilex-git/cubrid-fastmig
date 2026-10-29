# Pinned Rust toolchain. `just toolchain` builds it into the .toolchain GC root; every just recipe
# puts .toolchain/bin first on PATH.
# rustc 1.99.0. 1.99.1 is not in nixpkgs-unstable as of 2026-10-29 (staging-next 9ecc0a67); raise only the pin once it lands.
let
  nixpkgs = builtins.fetchTarball {
    url = "https://github.com/NixOS/nixpkgs/archive/e62746b0b472753ef019baceab6f1e2e52014980.tar.gz";
    sha256 = "010xm16sbwwh5jlhj3p79avsiy8qx85bk2kjyg4kgb46spvmy7dk";
  };
  pkgs = import nixpkgs {
    config = { };
    overlays = [ ];
  };
in
pkgs.buildEnv {
  name = "cubrid-fastmig-toolchain";
  paths = with pkgs; [ rustc cargo clippy rustfmt gcc ];
}

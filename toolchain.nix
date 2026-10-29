# Pinned Rust toolchain. `nix-build toolchain.nix -o .toolchain` builds it into a GC root;
# put .toolchain/bin on PATH.
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

{ pkgs ? import <nixpkgs> {} }:

pkgs.mkShell {
  name = "loom-dev";

  nativeBuildInputs = with pkgs; [
    cargo
    rustc
    rustfmt
    clippy
    rust-analyzer
    git
  ];

  RUST_BACKTRACE = "1";
}

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

  # Cap the address space of every process in this shell at 4 GiB.
  # A runaway allocation in a test or a fuzz case must fail with an
  # allocation error, not exhaust the host memory.
  shellHook = ''
    ulimit -v 4194304
  '';
}

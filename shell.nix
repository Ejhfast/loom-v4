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
    # CPython is the frame of reference for the language-operation
    # benchmarks. It runs the same workload as the Loom program, so a
    # ratio says whether a number is reasonable for an interpreter.
    python3
  ];

  RUST_BACKTRACE = "1";

  # Cap the address space of every process in this shell at 4 GiB.
  # A runaway allocation in a test or a fuzz case must fail with an
  # allocation error, not exhaust the host memory.
  shellHook = ''
    ulimit -v 4194304
  '';
}

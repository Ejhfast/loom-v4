# Loom Project Guide

This repository contains Loom, an object language with a reified compiler,
reified virtual machines, explicit effect rows, and isolated procs. The
reference implementation is Rust.

## Rule: Simplified Technical English

All text that you write for this project must obey ASD-STE100 (Simplified
Technical English). This rule applies to documentation, comments, commit
messages, diagnostics text, and reports. Follow these points:

- Write short sentences. Use a maximum of 20 words in an instruction.
  Use a maximum of 25 words in a description.
- Use the active voice.
- Give one instruction in each sentence.
- Use a word for only one meaning, and use one approved term for each thing.
- Use simple verb tenses. Do not use the words "shall" or "should" — write
  "must" or give the instruction directly.
- Do not use idioms or slang.

Code identifiers and required technical terms are exempt.

## Build setup (NixOS)

The host is NixOS. There is no global Rust toolchain. All build tools come
from `shell.nix` in the repository root.

To run one command in the build environment:

```sh
nix-shell --run "cargo build --workspace"
nix-shell --run "cargo test --workspace"
nix-shell --run "cargo run -p lm-cli -- run --show-result examples/01-basics/factorial.lm"
```

To open an interactive build shell:

```sh
nix-shell
```

Do not call `cargo` or `rustc` directly. Always use `nix-shell --run "..."`.
Do not install tools with `rustup`, `cargo install`, or `nix-env`. If you
need a new tool, add it to `shell.nix`.

### Memory cap

`shell.nix` caps the address space of every process in the build shell
at 4 GiB (`ulimit -v`). A runaway allocation in a test or a fuzz case
must fail with an allocation error, not exhaust the host memory. Do not
remove or raise this cap. Do not run tests outside the capped shell.
If one process has a real need past the cap, raise it in `shell.nix`
in its own reviewed commit.

## Repository layout

- `docs/specs/language-spec.md` — the normative language specification
  (version 0.2).
- `docs/specs/build-order.md` — the weekly vertical-slice implementation
  plan. Each week lands a complete source-to-execution increment.
- `docs/specs/sidecar/` — sidecar specifications. Each one refines the
  language specification for one topic. Keep `docs/specs/` itself to the
  language specification and the build order.
- `crates/` — the Rust workspace crates (`lm-source`, `lm-types`,
  `lm-bytecode`, `lm-verify`, `lm-vm`, `lm-cli`, ...).
- `examples/` — runnable Loom programs (`.lm` files) with checked output.
- `tests/` — UI, run-pass, run-fail, and verifier test suites.

## Commit rules

- Write a plain, descriptive commit message.
- Do not add attribution trailers. Do not add `Co-Authored-By` lines,
  tool branding, or vendor branding of any kind.

## Engineering rules

- Keep the dependency direction from `docs/specs/build-order.md` section 1.
  `lm-vm` must not depend on the filesystem, clock, network, or compiler
  frontend.
- Unsupported syntax or semantics must reject with a clear diagnostic.
  Do not add a silent fallback path.
- A decoder must never size an allocation from an untrusted length
  field before it checks the length against the remaining input.
  Reject impossible sizes before any large allocation.
- Every executed function must pass the independent bytecode verifier.
- Format code with `nix-shell --run "cargo fmt"`. Keep
  `nix-shell --run "cargo clippy --workspace"` clean before a commit.
- Run the full test suite before you report a task as complete.

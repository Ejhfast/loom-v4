# Loom

Loom is a capability-secure programming language with lightweight processes and metaprogramming based on reifiable computation.

See the runnable [examples](examples/). Read the [language specification](docs/language-spec.md) for the complete language definition.

## Build

The Nix shell supplies Rust and applies the project memory limit.

```sh
nix-shell --run "cargo build --workspace"
nix-shell --run "cargo test --workspace"
```

Run an example through Cargo:

```sh
nix-shell --run "cargo run -p lm-cli -- run --show-result examples/01-basics/factorial.lm"
```

Run `nix-shell` to open an interactive build shell.

## CLI

The build creates `target/debug/lm`.

- `lm new <name>` creates a package.
- `lm check <file.lm>` checks one source file.
- `lm build [path]` builds a source file or package.
- `lm run [options] [path]` builds or loads a program and runs it.
- `lm disasm <path>` prints decoded bytecode.
- `lm inspect <path>` describes artifacts, snapshots, or live state.
- `lm snapshot save` saves a snapshot.
- `lm snapshot verify` verifies a snapshot.
- `lm snapshot run` restores and runs a snapshot.

Run `lm --help` for command options.

## Organization

Most implementation code is under `crates/`.

### Crates

- `lm-abi` defines operation, effect-group, resource, and intrinsic manifests.
- `lm-scc` finds strongly connected components in dense graphs.
- `lm-source` scans and parses source code and renders diagnostics.
- `lm-types` interns types and checks subtype relations.
- `lm-hir` checks source programs and lowers them to bytecode.
- `lm-bytecode` defines decoded and serialized bytecode formats.
- `lm-verify` independently verifies decoded bytecode.
- `lm-math` supplies pure floating-point math algorithms.
- `lm-digest` supplies pure content digest algorithms.
- `lm-regex` supplies bounded regular-expression compilation and execution.
- `lm-jit` compiles verified bytecode to native code.
- `lm-link` links artifacts and publishes immutable code namespaces.
- `lm-value` defines the runtime value representation.
- `lm-heap` stores objects and defines native object layouts.
- `lm-graph` traverses object graphs for collection, copying, and digests.
- `lm-vm` executes verified code and manages worlds and snapshots.
- `lm-proc` schedules lightweight processes and snapshot barriers.
- `lm-compiler` compiles modules and builds packages.
- `lm-cli` provides the `lm` command-line program.
- `lm-host` implements operating-system capabilities for the command line.
- `lm-testkit` supports language tests and benchmarks.

### Other directories

- `core/` contains the core language classes and interfaces.
- `std/` contains standard library modules.
- `examples/` contains runnable programs with checked output.
- `tests/` contains source-level language cases.
- `benchmarks/` contains performance programs and results.
- `docs/` contains the language specification.

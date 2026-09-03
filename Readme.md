# Loom

Loom is a capability-secure programming language with lightweight processes and metaprogramming based on reifiable computation.

See the runnable [examples](examples/). Read the [language specification](docs/language-spec.md) for the complete language definition.

## Crates

- `lm-abi` defines operation, effect-group, resource, and intrinsic manifests.
- `lm-scc` finds strongly connected components in dense graphs.
- `lm-source` scans and parses source code and renders diagnostics.
- `lm-types` interns types and checks subtype relations.
- `lm-hir` checks source programs and lowers them to bytecode.
- `lm-bytecode` defines decoded and serialized bytecode formats.
- `lm-verify` independently verifies decoded bytecode.
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

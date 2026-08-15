# Packages and Modules

Status: draft for review. Weeks 5 and 6 implement the base. Week 12
completes the tooling.

This document defines the ordinary developer loop: how a project
lives on disk, how files refer to each other, and how one package
depends on another. The goal is basic ergonomics in the Cargo style,
with the smallest possible feature set.

---

## 1. Package layout

`lm new hello` creates a package:

```text
hello/
  lm.package
  src/
    main.lm
```

A package is a directory with one `lm.package` manifest and one
`src/` tree. `lm build`, `lm run`, and `lm test` work from any
directory inside the package.

## 2. The manifest

The manifest is TOML and stays minimal:

```toml
[package]
name = "hello"
version = "0.1.0"

[dependencies]
mathlib = { path = "../mathlib" }
```

Version 0.2 supports path dependencies only. The dependency key is
the local name of that package inside this one. Rename a dependency
by its key when two names collide. Registries, version ranges,
lockfiles, and workspaces are out of scope for now. Dependency
identity is content-hashed, so a lockfile adds nothing yet.

## 3. Modules from files

The file tree under `src/` is the module tree. The file
`src/geometry/shapes.lm` is the module `geometry.shapes`. Every
top-level definition is exported by its source name (specification
3.1). There are no visibility modifiers in version 0.2.

`src/main.lm` is special in one way only: its trailing expression is
the program entry. A package without `src/main.lm` is a library.

## 4. The `use` declaration

`use` is a name-resolution declaration. It never grants authority,
and it never runs code. `use` lines come first in a module, before
definitions. One dotted path per line; the last segment becomes the
bound name:

```lm
use std.io            # io.print(...)
use std.io.print      # print(...)
use sys.vm            # vm.Vm()
use mathlib.matrix    # matrix.Matrix(...)
use geometry.shapes   # a module of this package
```

The first path segment resolves against a fixed root set: the
dependency names from the manifest, this package's own top-level
modules, `std`, and `sys`. A collision inside the root set is a
compile error; the fix is a manifest rename. The bound name enters
the module scope below locals and parameters, in the value or type
namespace as the item requires.

A `use` of another package compiles to a named import slot
(specification 3.3). The build tool constructs the compile
environment from the manifest and the dependency interfaces, and it
fulfills every slot. The `CompileEnv`/`LinkEnv` API is the same
mechanism driven by hand; it stays the embedding and sandbox path,
and ordinary development never touches it.

Fully qualified references (`mathlib.matrix.Matrix`) work without a
`use` line for one-off mentions.

## 5. The standard library

`std` ships with the toolchain and needs no manifest entry. Its
names still enter a module only through explicit `use` lines. There
are no ambient standard-library names. The core prelude (`Option`,
`Result`, `List`, ...) is unchanged and stays separate from `std`.

## 6. Commands

- `lm new NAME` — scaffold the layout above.
- `lm build` — build the dependency graph into verified artifacts,
  with a content-addressed cache. The linked program artifact lands
  at `build/debug/<name>.lma`.
- `lm run [--allow ...]` — build, then execute the program artifact
  under the given policy grants. `lm run <path>.lma` executes a
  prebuilt artifact directly, with no package or source present.
- `lm test` — build and run the package tests (full harness in
  week 12).

The `.lma` artifact is the deployment and sandbox unit. It is the
same container the runtime compiler produces and the linker
consumes (specification 3.4-3.6). Every load path admits code
through the one verifier.

## 7. Identity and caching

Definitions, modules, and interfaces are content-hashed
(specification 3.7). The build cache keys on semantic hashes, so an
edit to comments or formatting rebuilds nothing downstream. This is
the one place the design intends to beat Cargo: reproducibility and
rebuild precision come from content identity, not timestamps or
version strings.

Content identity may later support Unison-style features, for
example definition-level storage or hash-addressed code sharing.
All of that is explicitly out of scope now.

## 8. Resolution order summary

For a simple name inside a module body: locals and parameters,
then module definitions, then `use` bindings, then the prelude,
then fixed bindings (`sys`). Ambiguity inside one layer is an
error, never a silent pick.

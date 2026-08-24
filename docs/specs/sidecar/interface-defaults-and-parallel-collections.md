# Interface defaults and parallel collections

Status: accepted design.

This sidecar defines interface default methods and structured collection parallelism.

The language specification remains normative for unchanged rules.

## 1. Goals

This extension has these goals.

- One interface method body serves every conforming type.
- A class can replace a default with an exact method override.
- Every iterable receives one common eager collection surface.
- `par_map` uses scheduler procs without exposing scheduler workers.
- Sequential and parallel mapping have the same value and fault results.
- Existing native collection methods retain their fast paths.

## 2. Interface default methods

An interface method can have a body.

```lm
interface Named
  def name(self): String

  def decorated(self): String
    "<#{self.name()}>"
  end
end
```

A declaration without a body remains a required method.

The compiler stores one verified function for each default body.

The compiler does not copy the body into each conforming class.

An interface call first selects a matching class method.

The call selects the interface default when the class has no matching method.

A class method must match the complete interface method contract.

The contract includes generic parameters, bounds, effects, parameters, and the result.

Two unrelated defaults with one selector cause a diagnostic.

The class can resolve the conflict with an explicit method.

Interface inheritance does not use an implicit linearization order.

Default functions enter module identity and normal bytecode verification.

An imported interface identifies each default through a hidden function binding.

The binding is not a source name.

## 3. Generic interface methods

An interface method can declare type and effect parameters.

```lm
def map[U, effect e](self, f: (Self.Item) -> U with e): List[U] with e
```

The checker infers method arguments from arguments and the expected result.

The bytecode call records only the method-owned arguments.

One compact call site stores 16-bit interface and method indices.

A module can contain at most 65,536 interfaces.

An interface can contain at most 65,536 methods.

The receiver conformance supplies the interface-owned arguments.

The verifier checks both argument sets independently.

## 4. Default method bounds

A default can use a `when` clause.

```lm
def min(self): Option[Self.Item] when Self.Item: Comparable
```

The call is available only when the receiver proves every bound.

The first release permits type parameters and associated-type projections as premise subjects.

The checker and verifier perform the same bounded conformance judgment.

## 5. Iterable surface

`Iterable` requires only `iterator` and its associated types.

It provides these eager defaults.

- `each`, `map`, `filter`, `fold`, and `to_list`
- `any`, `all`, `find`, `position`, and `count_where`
- `each_indexed` and `map_indexed`
- `take`, `drop`, and `chunks`
- Bounded `min`, `max`, and `join`

Each callback is nonescaping unless a method explicitly marks it `escaping`.

Each callback method carries one inferred effect row.

Iteration follows the iterator order.

`any`, `all`, `find`, and `position` stop at their first result.

Negative `take` and `drop` counts fault.

A nonpositive chunk size faults.

`List` keeps its optimized methods as explicit overrides.

`Range` receives the common methods through `Iterable`.

## 6. Process outcomes and faults

`Handle[M, R].done()` returns `Result[R, Fault]`.

A run terminal operation also returns `Result[R, Fault]`.

Core removes `ProcResult` and `RunResult`.

`StepEvent` and `DriveEvent` remain distinct event types.

`raise(fault)` raises the same fault with its existing trace.

`Result.value()` returns an `Ok` value or raises an `Err` fault.

`Result.value()` exists only when the error type is `Fault`.

## 7. Closure proc launch

`sys.proc.run` accepts a transferred closure with no parameters.

```lm
handle = sys.proc.run(do ||: Int
  solve()
end)
```

The result type is `Handle[Never, R]`.

The closure row becomes the child birth grant.

The caller must own `Proc.Spawn` and every operation in that row.

The closure and its captures must be sendable.

The child has no mailbox.

Class-based proc launch remains available for stateful mailbox workers.

## 8. Closure effect inference

A closure uses its expected polymorphic row when one exists.

The checker infers that row from the closure body.

A standalone closure still requires an explicit `with` clause for effects.

An ambiguous closure still requires an explicit `with` clause.

An explicit `with ()` prevents contextual row inference.

E1046 names the explicit `with` repair.

E1064 names the `escaping` repair.

## 9. Parallel mapping

`Iterable` provides this method.

```lm
def par_map[U](self, escaping task: (Self.Item) -> U): List[U] with Proc
```

The callback has an empty effect row.

The method returns the same values as `map` in source order.

The method raises the first child fault in source chunk order.

The implementation uses at most 16 contiguous chunks.

Each nonempty chunk runs in one closure proc.

The implementation never reads the scheduler worker count.

Deterministic mode uses the same implementation.

The `Proc` effect remains visible in the method contract.

## 10. Small core additions

`Int` provides `min` and `max`.

`Iterable` provides `sum(start)` when its item type implements `Add`.

`Iterable.chunks` provides the common chunk operation.

A parameterless generic `sum()` does not use `Add` alone.

Addition does not define an identity value for an empty iterable.

## 11. Gates

The parser tests requirements, defaults, generics, and bounded defaults.

Cross-module tests call an imported default.

Verifier tests reject forged default targets and generic applications.

Diamond tests require an explicit override.

Differential tests compare shared defaults with existing `List` overrides.

`par_map` tests both scheduler modes.

The tests compare values, order, empty input, and child faults.

Snapshot tests capture a running closure worker.

Snapshot admission accepts a nullary verified function as a closure proc body.

The benchmark compares `par_map` with the hand-written proc implementation.

The benchmark also records deterministic `map` and `par_map` costs.

The release gate permits no regression in existing native `List.map` calls.

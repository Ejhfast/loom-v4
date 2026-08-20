# Collections and iteration

Status: accepted design. This work extends Week 10.

This sidecar defines `Option`, collection storage, interfaces, iteration, and collection views.

The language specification remains authoritative where this sidecar states no replacement rule.

## 1. Goals

This design has these goals:

- `Option[T]` adds no guest heap object on typed paths.
- `List[T]` and `Map[K,V]` use native storage behind core classes.
- Common collection operations keep their current constant factors.
- Collection algorithms use ordinary verified Loom code where practical.
- `for` gives one common traversal form.
- Nominal interfaces support future data structures without structural dispatch.
- Public views avoid unnecessary collection copies.
- Interface growth keeps core compilation fast.

The design does not add interface values, orphan conformances, or multiple conformances.

## 2. Native `Option`

`Option[T]` remains a sealed core enum at the source level.

The VM gives this enum a native one-payload representation.

`Some(value)` stores the payload value directly.

`None` stores one immediate empty-case value.

The empty-case value records the closed outer `Option` type and the arm number.

This type identity separates `None[Option[Int]]` from `Some(None[Int])`.

Typed storage keeps the representation for locals, fields, captures, lists, maps, calls, and replies.

An erased `Any` value can require a box. This rule keeps erased values unambiguous.

The bytecode has explicit pack and empty-case instructions.

The verifier checks each instruction against the pinned `Option` roles.

Pattern tests and field reads understand the native representation.

Snapshots encode the semantic arm and payload. They do not encode process-local table numbers.

Transfers remap the closed type of each empty-case value.

Digests encode the semantic `Option` structure.

The first implementation reserves this layout for pinned `Option`.

The VM can later use the same model for compatible one-payload enums.

`Result[T,E]` keeps its ordinary enum representation.

## 3. One-probe lookup

`List.get` uses one checked native read.

`Map.get` uses one hash-table probe.

Each instruction returns the native `Option` representation.

`Map.put` and `Map.remove` return the previous value through the same representation.

No implementation can lower `Map.get` as `has` followed by `at`.

## 4. Core collection classes

The pinned core declares these final native classes:

```lm
final class List[T]
end

final class Map[K, V]
end
```

The declarations own the public method tables.

Native VM objects keep the storage that existing list and map bytecode uses.

The source classes add no wrapper object.

List literals and map literals produce these native class values.

`[T]` remains type sugar for `List[T]`.

`{K: V}` remains type sugar for `Map[K,V]`.

Leaf intrinsics expose storage operations to trusted core methods.

Public methods compose those intrinsics in ordinary Loom code.

Measured hot operations can use one dedicated verified instruction.

## 5. List storage and mutation

A list stores a contiguous `Value` buffer, a length, a capacity, and a structural epoch.

`push` has amortized O(1) time.

`at`, `get`, `set`, and `swap_remove` have O(1) time.

`insert`, ordered `remove`, and range moves have O(n) time.

The runtime starts epoch tracking when a traversal captures the epoch.

The runtime can skip epoch writes before tracking starts.

Each structural change increments the epoch after tracking starts.

These changes include length, capacity, and element-order changes.

`set` does not increment the epoch.

Freeze keeps the same representation and blocks later writes.

The complete core surface follows language specification section 24.4.

## 6. Map storage and mutation

A map stores insertion-ordered entries and an open-addressed lookup index.

It also stores a structural epoch.

Lookup has expected O(1) time.

Iteration has O(n) time and performs no key lookup.

Replacing one value retains its entry position.

Replacing one value does not increment the epoch.

Inserting, removing, reordering, or clearing entries increments a tracked epoch.

Removal can leave an internal tombstone.

Public traversal remains dense and follows insertion order.

The complete core surface follows language specification section 24.5.

## 7. Nominal interfaces

An interface declares method requirements and associated types.

An interface can declare type parameters and effect parameters.

The initial syntax is:

```lm
interface Iterator
  type Item
  def next(mut self): Option[Self.Item]
end

interface Iterable
  type Item
  type Iter: Iterator
  def iterator(self): Self.Iter
end
```

A class declares its conformances in its header.

The class body binds each associated type.

```lm
final class Values[T] implements Iterable
  type Item = T
  type Iter = ValuesIterator[T]
end
```

An interface application writes effect arguments with `effect`.

```lm
interface Source[effect e]
  type Item
  def next(mut self): Option[Self.Item] with e
end

final class PureSource implements Source[effect ()]
  type Item = Int
end
```

A type parameter adds one or more bounds after a colon.

```lm
def count[T: Iterable](values: T): Int
end
```

Associated projections use `T.Item` or `Self.Item`.

The type that declares the conformance owns it.

One closed type has at most one conformance to one interface.

Conformance is explicit. Matching method names do not imply conformance.

Interface values do not exist in this version.

Interfaces cannot appear as fields, parameters, results, or collection elements.

A bound can select interface methods for a type parameter.

Concrete final receivers use direct calls.

Generic calls use a dense conformance entry from the active type environment.

No witness value travels through a guest call.

The verifier checks every generic interface call and every conformance entry.

The module interface records exported interfaces, bounds, and conformances.

Each structural hash covers all interface contract data.

This data includes associated types, method signatures, rows, and conformance bindings.

The first slice adds `Iterator`, `Iterable`, and `Counted`.

`Hash`, `Equal`, and ordering interfaces remain separate future contracts.

## 8. Iterator contract

`Iterator` has one associated `Item` type.

Its `next` method returns `Option[Item]`.

`Iterable` has associated `Item` and `Iter` types.

Its `Iter` type must conform to `Iterator` with the same item type.

`Counted` provides `len() -> Int`.

List, map, text, range, and collection views provide native iterators.

An iterator stores a source reference, a position, and the captured epoch.

Text iterators store a byte cursor.

Range iterators store the next integer and the range limit.

Manual `next` calls return native `Option` values.

Map iterators yield `(K,V)` tuples.

This tuple can allocate when it escapes.

Specialized traversal can pass the key and value as two scalar values.

## 9. `for` statement

The statement syntax is:

```lm
for item in values
  use(item)
end
```

A map pattern can bind two values:

```lm
for key, value in table
  use(key, value)
end
```

The source expression runs once.

The iterator expression runs once.

Each successful step binds the loop pattern.

The first `None` exits the loop.

`break`, `continue`, and `return` keep their ordinary meanings.

The statement result is `()`.

The enclosing row includes all iterator effects.

The compiler specializes known native iterables.

A list loop reads the native buffer by index.

A map loop reads insertion-ordered entries by position.

A text loop advances one Unicode scalar at a time.

A range loop advances integers directly.

These loops allocate no iterator and no `Option` per element.

A generic loop uses `Iterable.iterator` and `Iterator.next`.

It allocates at most one iterator object.

Its `Option` steps use no guest heap objects.

## 10. Mutation during traversal

An iterator compares its captured epoch before each step.

A structural mismatch raises `CollectionModified`.

List `set` remains valid during traversal.

Map value replacement remains valid during traversal.

All structural changes invalidate active iterators and views.

Specialized `for` loops apply the same checks.

## 11. Collection views

`List.slice_view(start, length)` returns `ListSlice[T]`.

The view stores the source list, the range, and the captured epoch.

The view does not copy elements.

`List.slice` keeps its eager copy behavior.

`Map.keys`, `Map.values`, and `Map.entries` return views.

Their types are `MapKeys[K,V]`, `MapValues[K,V]`, and `MapEntries[K,V]`.

Each view stores the source map and the captured epoch.

Each view follows insertion order.

The methods `keys_list`, `values_list`, and `entries_list` create eager lists.

Views retain their source objects.

Programs can use eager methods when retention is undesirable.

A structural source change invalidates the view.

Freezing a view freezes its reachable source graph.

Snapshots preserve the source edge, range, position, and epoch.

## 12. Closures and traversal methods

Trailing braces remain ordinary Loom closures.

`return` exits the closure. It does not exit the caller.

`break` and `continue` cannot cross a closure boundary.

Collection methods use effect-polymorphic closure parameters.

The compiler marks selected parameters as nonescaping.

A nonescaping closure cannot be stored, returned, or captured.

It can only pass to another nonescaping parameter.

The compiler can use a stack callback descriptor for such a call.

The descriptor contains code, captures, and the active type environment.

Snapshots preserve an active descriptor as machine state.

Ordinary escaping closures keep their heap representation.

## 13. Layering

The implementation uses five layers.

1. Native objects own storage, epochs, and derived indexes.
2. Verified instructions implement measured leaf operations.
3. Core Loom classes define public methods and algorithms.
4. Nominal interfaces define reusable static contracts.
5. The compiler specializes `for` and nonescaping callbacks.

`lm-vm` depends on no filesystem, network, clock, or compiler frontend.

## 14. Compilation cost

The parser interns interface names during declaration prepass.

The checker stores dense interface and conformance identifiers.

Method lookup uses precomputed tables.

Bound checks use memoized closed-type and interface pairs.

The compiler does not scan every conformance for each call.

The core image compiles once for each test harness process.

Tests request only the standard modules that they use.

## 15. Delivery order

Implement the work in this order:

1. Add this sidecar and benchmark gates.
2. Add native `Option` and one-probe lookups.
3. Add nominal interfaces and module-interface records.
4. Add native iterators and `for` specialization.
5. Move `List` and `Map` method tables into core.
6. Add the complete collection operation surface.
7. Add nonescaping callback verification and lowering.
8. Add collection views and eager materializers.
9. Run correctness, compiler, and language benchmarks.

## 16. Required gates

The full workspace test suite must pass.

The bytecode decoder must reject invalid lengths before allocation.

The verifier must reject forged option, interface, iterator, and view operations.

Typed `Some` and `None` construction must allocate no guest object.

`Map.get` must perform one lookup probe.

Specialized `for` must allocate no object per element.

`list_push`, `list_index`, `map_insert`, and `map_lookup` must not regress materially.

The common higher-order collection pipelines must improve or remain stable.

Core compilation and artifact loading must remain below two milliseconds on the reference host.

Interface-heavy typechecking must keep near-linear growth.

Final reports must include baseline and branch medians.

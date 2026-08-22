# Conditional collections and native tuples

This sidecar defines the pre-release collection and interface extension.

The language specification remains normative for unchanged rules.

## 1. Goals

This extension has these goals.

- Generic types can declare conformances with type parameter premises.
- Generic methods can declare the same premises.
- Core collections use common interfaces without losing native storage.
- Every legal non-unit tuple has an ordinary core method table.
- Map removal has expected constant cost.
- Frozen classes make stable map keys statically visible.
- Native scalar and collection operations retain their fast paths.

## 2. Conditional conformances

A class or enum can attach premises to one conformance.

```lm
final class Box[T] implements Display when T: Display
  value: T

  def append_to(self, mut builder: StringBuilder) when T: Display
    self.value.append_to(builder)
    ()
  end
end
```

Each premise has the form `TypeParam: InterfaceUse`.

A plus sign separates several bounds for one type parameter.

A comma separates premises and conformances.

The parser uses the colon to distinguish a premise from the next conformance.

The first release permits only class type parameters in premises.

It does not permit concrete types, projections, or effect rows as premise subjects.

A concrete application conforms when every substituted premise conforms.

The checker limits recursive conformance resolution to 128 applications.

The verifier performs the same judgment independently.

Artifacts store each premise with the conformance.

Premises enter interface identities and class definition identities.

## 3. Premise implication

Interface inheritance can produce several paths to one interface.

`Hashable` extends `PartialEq`.

A `Hashable` premise therefore implies the matching `PartialEq` premise.

The compiler removes a derived conformance when a weaker direct conformance covers it.

The compiler rejects incomparable duplicate conformances.

This release does not encode disjunctive premise sets.

## 4. Constrained methods

A generic method can use the same premise form.

```lm
def sort(mut self) when T: Comparable
```

The checker exposes the method only when its substituted premises conform.

The method body can use every declared premise.

Artifacts store method premises with the callable contract.

The verifier checks the body and every call independently.

## 5. Native tuple carriers

Core declares `Tuple2` through `Tuple16`.

Each class has a native tuple representation for its arity.

The following representations do not change.

- `Type::Tuple`
- `BcType::Tuple`
- `Object::Tuple`

Type checking views an N-element tuple as `TupleN` during conformance lookup.

Method lookup uses the same view.

The verifier independently maps each tuple type to its core role.

The VM maps each tuple object length to the same core role.

Tuple literals and tuple patterns retain their existing instructions.

The surface type `(A, B)` remains the normal type spelling.

`Tuple2[A, B]` resolves to the same structural tuple type.

Tuple classes have no separate heap representation.

The language permits tuple arities from two through sixteen.

`()` remains the unit value.

Core provides a native `Unit` method table for that value.

## 6. Pair removal

Core removes `Pair[A, B]`.

Network acceptance returns `(TcpStream, SocketAddress)`.

Programs use tuple patterns or indexed access.

`Tuple2.swap()` replaces `Pair.swap()`.

This change invalidates earlier artifacts and snapshots.

The release makes no compatibility translation.

## 7. Core conformances

Core adds these conformances.

| Type | Conformances |
| --- | --- |
| `Unit` | `Display`, `PartialEq`, `Hashable` |
| `Tuple2` through `Tuple16` | Conditional `Display`, `PartialEq`, `Hashable`, and `Comparable` |
| `List[T]` | Conditional `Display`, `PartialEq`, `Hashable`, and `Comparable`; `Copyable` |
| `ListSlice[T]` | Conditional `Display`, `PartialEq`, and `Comparable` |
| `Map[K, V]` | Conditional `Display`, `PartialEq`, and `Hashable`; `Copyable` |
| `Set[T]` | Conditional `Display`; `PartialEq`, `Hashable`, and `Copyable` |
| `Option[T]` | Conditional `Display`, `PartialEq`, and `Hashable` |
| `Result[T, E]` | Conditional `Display`, `PartialEq`, and `Hashable` |
| `StringBuilder` | `Copyable` |
| `ByteBuffer` | `Copyable` |

List equality compares elements in order.

Map equality ignores insertion order.

Set equality ignores insertion order.

Tuple equality compares arity and elements in order.

List and tuple comparison use lexicographic order.

Map and set do not define total order.

A collection key must be deeply frozen before map insertion.

`ListSlice` remains non-hashable because it retains its source list.

## 8. Hashing

Core provides `hash_of` for every `Hashable` value.

Core provides `hash_combine` for ordered field hashing.

The mixer uses defined wrapping arithmetic inside a native intrinsic.

Tuple hashes include the arity and every element.

List hashes include element order.

Map hashes combine entries without insertion order.

Set hashes combine elements without insertion order.

Equal values produce equal semantic hashes.

The VM applies its private process key after semantic hashing.

## 9. Map storage

A removed map entry becomes a tombstone.

Lookup probing continues through tombstones.

Iteration skips tombstones.

Map operations compact storage when tombstones pass a bounded threshold.

Compaction preserves insertion order among live entries.

Compaction increments the structural epoch.

Snapshot capture compacts every admitted map.

The snapshot format stores no tombstones.

Map lookup has expected constant cost.

Map removal has amortized constant cost.

Map iteration has linear cost.

## 10. Frozen classes

The `frozen class` declaration creates an always-frozen nominal type.

Its methods cannot declare `mut self` except `init`.

Every field type must be always frozen.

The checker evaluates this property recursively.

Primitive values, immutable text, immutable bytes, and frozen classes satisfy the property.

A tuple satisfies the property when every element satisfies it.

Generic frozen classes require always-frozen type arguments at each construction site.

An instance becomes frozen after successful initialization.

The verifier enforces the declaration and field rules.

## 11. Supporting interfaces

`Comparable` extends `PartialEq`.

It defines `compare(self, other: Self): Ordering`.

Int, Bool, Text, Char, and Bytes implement `Comparable`.

`Copyable` defines `copy(self): Self`.

Collection copies own independent storage and share element values.

Builder copies own independent mutable storage.

`Error` extends `Display` and adds no method.

Every portable core error implements `Error`.

## 12. Diagnostics

A failed conditional conformance names the first unmet premise.

A tuple arity diagnostic names the supported range.

`MutableMapKey` tells the user to freeze the key before insertion.

The diagnostic also names `frozen class` for suitable user keys.

A mutable interface hook reports its exact `mut self` mismatch.

## 13. Performance gates

Each stage records core classes, functions, artifact size, compilation, and loading.

Each stage records the full workspace suite time.

No stage can add workers to hide a slower suite.

Native Int, Text, Bytes, List, and Map paths remain within normal noise.

Focused benchmarks cover tuple dispatch, collection equality, hashing, sorting, and map removal.

The benchmark baseline lives in `benchmarks/release-baseline.md`.

## 14. Snapshot trust

Admission verifies every stored map entry and semantic hash.

The runtime trusts admitted map hashes.

Untrusted snapshot execution remains unsupported.

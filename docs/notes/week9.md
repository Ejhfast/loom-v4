# Week 9 Status

This note records the week 9 work. It covers:

- what landed;
- the format, field by field;
- the decisions and the rejected alternatives;
- the simplifications inside the slice;
- the changed tests, the new tests, the measurements, the gates, the
  open questions, and the deferred work.

Bytecode format version 13 carries the `SnapshotImage` and `Snapshot`
types. The interface format is version 5 for the same reason. The
compiler ABI version is 7, because the canonical type identity
encoding gained two tags, and the verifier version is 6, because the
verifier gained two core families and four operation rules. The
operation manifest ABI version is 3: it gained the four snapshot
operations of specification 23.5, and the manifest rule makes a
membership change an ABI change. The core image pin moved to
`db43c1783f9b7b27209232063b8563e1de670fae98b63b0fac8dd7a646d3ae6b`
and `core/pinned-core-defs.txt` holds forty-two definition hashes: the
thirty-six of week 8 plus the two snapshot error families. The shape
table holds sixteen shapes. The snapshot container format version is
1.

The work after week 9 moved several of these numbers.
`docs/notes/post-week9-admission.md` records the current values: the
bytecode format is 14, the compiler ABI is 8, the verifier is 8, the
operation manifest ABI is 4, and the snapshot container format is 2.

## Landed

### The machine world

`vm.snapshot()` copies the world reachable from one held machine.
`sys.vm.snapshot_self()` copies the world reachable from the
performing machine. `sys.vm.Vm().restore(snap)` builds a complete
independent world from either copy.

The world is closed by construction. Reachability follows the handles
in the captured state, so every handle in the bytes targets a captured
machine and a reference that leaves the world is not representable.
The plan therefore carries no ownership records and no restore
bindings.

### The cut moved into `lm-vm`

The consistent cut of specification 17.3 now lives in
`crates/lm-vm/src/snapshot/write.rs`. `lm_proc::Barrier` calls it and
keeps its week-8 report shape.

The move is forced: the guest operation `Vm.SnapshotHeld` runs inside
the driver loop, and `lm-vm` depends on no scheduler. Duplicating the
algorithm was the alternative, and one world must have one cut.

The steps are unchanged, and step 6 now encodes:

1. stop the root and every reachable machine;
2. close the set over the machine references the stopped state holds;
3. freeze mailbox acceptance at one cut marker;
4. record the machine states;
5. preflight the host attachments;
6. encode;
7. resume the original world after success and after failure.

### Snapshot roots, not collection roots

The week-8 carry-forward asked for the split, and it landed.
`Machine::snapshot_roots` is the one declaration point of snapshot
reachability:

> frame closures, locals, operands, pending arguments, the terminal
> value, the mailbox queue, the proc body, the interned literals.

It is the collection roots without the policy-table entries.
`World::machine_references` and `World::snapshot_preflight` both read
it, so the closed set and the encoder agree on what the world holds. A
machine that only a table-held mock closure names is no longer part of
the world, which is what specification 17.2 requires.

The image reader reads the same list from the image
(`snapshot::image_roots`), and the loader proves that the stored heap
is exactly the canonical traversal of it. The two orders therefore
cannot drift without a test failure.

### The canonical container

`crates/lm-vm/src/snapshot/codec.rs` holds the writer and the reader.
One image has one byte string, and the reader proves it: a container
that decodes must encode back to exactly the bytes it came from.

### Restore

Restore is valid on an `EmptyVm` alone. It builds every machine,
allocates every captured heap in canonical order, patches every child
reference, sets every frozen bit, relocates every handle, and installs
the frames, arenas, literals, pending request, terminal result,
mailbox, and block record of each machine.

Every restored machine starts with a fresh default-deny table.
Internal pass chains re-root on the restored parent; a machine whose
parent stayed outside the world re-roots on the restoring machine.
Restored procs sit behind one world gate, and the first `run`, `step`,
or `drive` of the restored root opens it for the whole restored world.

A failed restore removes every machine it added, returns the child
reservations, and leaves the target empty.

### The states

| Captured state | Restores as |
|---|---|
| between instructions | between the same instructions |
| `asked` | `asked`, and one `drive` mints a fresh token |
| terminal | terminal, with its stored result |
| holder-paused | paused, and `resume` reactivates it |
| receiverless self snapshot | `asked` on `Vm.SnapshotSelf` |
| `waiting` | blocked: `ResourceActive` names the attachment |

### The tools

- `lm snapshot save [--allow LIST] <file.lm> <out.lms>` runs a program
  and writes the last image it captured. The program states in its own
  source which world a checkpoint holds.
- `lm snapshot verify [--program P] <file.lms>` prints the one-line
  verdict.
- `lm snapshot run [--allow LIST] [--program P] <file.lms>` restores a
  container and drives the restored world.
- `lm inspect <file.lms>` prints the readable dump.

Each tool needs the program the container names, because the load
checks read its code hashes. `--program` names it, and the default is
the file with the same stem beside the container, so the build-order
spelling works as written.

### Runnable outputs

```text
$ lm run --show-result examples/08-snapshots/branch.lm --allow Vm
Done((42, 42))

$ lm run --show-result examples/08-snapshots/machine-world.lm \
    --allow Proc,Vm
Done((42, 42, 42))

$ lm snapshot verify checkpoints/asked-tree.lms
valid: state=asked machines=3 mailboxes=2
```

The machine-world example captures a held machine, a worker it names
in its own locals, and a helper the worker reaches through a handle in
its mailbox. Each restore gets its own complete copy of all three, and
the original three continue unchanged.

## The format

```text
magic        8 bytes  "LMSNAP\0\x01"
format       u32 LE   the container format version
abi          u32 LE   the operation manifest ABI version
compiler     u32 LE   the compiler ABI version
verifier     u32 LE   the verifier version
entries      u8       the section count
section table: entries * (kind u32 LE, offset u32 LE, length u32 LE)
section payloads, in table order, without a gap
container hash 32 bytes  BLAKE3-256, domain "lm-snapshot-container-v1\0"
```

Sections, in canonical order:

| Kind | Name | Content |
|---|---|---|
| 1 | header | machine count, root ordinal, module semantic hash, root result type |
| 2 | code | every referenced function and class slot with its definition hash |
| 3 | heaps | one heap per machine, in ordinal order |
| 4 | machines | one machine record per machine, in ordinal order |

Inside a payload every count and every ordinal is a canonical LEB128
integer, and every fixed field is little-endian. Machines and objects
are named by ordinal, code and classes by definition hash, operations
by manifest identity, types by semantic type digest, and fault codes
by stable name. No numeric heap slot, scheduler identifier, or
allocation order reaches the bytes.

Object ordinals come from the canonical traversal of the snapshot
roots. Machine ordinals come from a breadth-first walk from the root
over the machine references each captured heap holds, in canonical
object order.

The section table is fixed-width by design. A variable-width offset
would depend on the length of the table that holds it, and that circle
has no canonical answer.

### Room for a delta container

The container is sectioned and versioned, and the reader names the
sections it expects. A later format may add a section kind, for
example a base section that names a base image by container hash, and
a delta section beside it. Nothing in the present sections forbids it:

- a section kind is an explicit number, not a position;
- an object ordinal is local to one machine record, and a machine
  ordinal is local to one image, so a delta may carry its own ordinal
  map without renumbering a base;
- code, classes, operations, types, and fault codes are named by
  content, so a delta names them the same way a base does.

No delta implementation landed. The property above is the whole claim.

## The decisions

### A restored proc takes the birth grant

Specification 17.5 says every restored machine receives a fresh
default-deny table. Specification 18.3 says a launched proc receives a
birth grant that carries the `Proc` group and nothing else.

Restore gives a restored proc that same birth grant on top of the
fresh table. Without it a restored proc could not read its own
mailbox, and a restored proc world would be a copy that cannot run.
The grant creates no authority of its own: the chain still resolves
through the table of the restoring machine, so the restorer must hold
`Proc` exactly as a spawner must.

The alternative was the literal reading of 17.5: a restored proc gets
nothing and fails closed on its first `receive`. It was rejected
because it makes the machine-world restore of the build order
impossible to run, and because 18.3 already mints this grant at every
launch. The open question below records the tension.

The rule is visible in the machine-world example: the holder grants
the restored root what it needs with `world.table().pass(Proc)`,
because restore creates no authority.

### The guest holds bytes, and a hash decides the path

A guest snapshot value is the canonical container bytes. A restore
looks the bytes up by container hash in a bounded per-world table of
images this process wrote or already checked. A hit reads the decoded
image and repeats no structural check. A miss runs the external loader
once.

The alternative was to put the decoded image in the heap object. It
was rejected because `lm-heap` cannot name a `lm-vm` type, and a type
erased payload would lose the derived traits the shape table needs.

The second alternative was to trust every guest snapshot value. It was
rejected because a snapshot value can arrive inside another image: a
captured heap may hold a `Snapshot`, and those nested bytes are as
untrusted as the container that carried them. The hash lookup gives
the right answer in both directions without a provenance flag.

### The loader proves types, not only structure

A snapshot copies data, and the interpreter reads that data through
the types the verified code declares. A forged image that put a
string where the code expects an integer would reach an interpreter
path that no verified program can reach, and the interpreter would
abort instead of faulting.

The loader therefore proves the shape of every value that carries a
declared type:

- every local slot, against the declared local type of its frame;
- every operand, against the type the verifier proved at that program
  point. `lm_verify::FrameTypes` answers from the same abstract
  interpretation the type proof runs, so the loader and the verifier
  cannot disagree;
- every instance field, against the class layout;
- every closure capture, against the declared capture type;
- every argument of a pending perform, against the operand types the
  verifier proved just before the perform. The rule reads no manifest
  parameter type, so it covers a machine control operation as well as
  a fixed one;
- every accepted message, against the mailbox type the proc class
  fixes. A non-proc machine holds no accepted message, so a queued
  message on one rejects rather than sitting unchecked;
- the stored terminal value, against the recorded result type of its
  machine. A `Done` value that is not the unit must carry a result
  type, so an image that stripped the type cannot skip the proof;
- the elements of every collection those positions reach.

`Unit` and the uninitialized marker pass at every type, because an
unwritten local slot holds unit and an unassigned field holds the
marker.

The pending-argument rule reads the operand types, not the manifest.
The first version checked a pending argument against the manifest
parameter type, which exists for a fixed operation alone. A machine
control operation carries a generic schema, so that rule left the
arguments of a machine captured `asked` on a control operation
unchecked, and the restorer reads them through the dispatch path. The
rule now reads the operand types the verifier proved just before the
perform, for every operation kind, so a forged control argument
cannot slip through. This is the contract: a pending argument is
proved against the verifier's operand type at the perform, and the
argument count must equal the count the proved program point holds.

### The recorded result type is a machine field

A terminal machine keeps no frame, so nothing else records the type
its stored result carries. `Machine::result_ty` records the declared
result type of the entry frame, and the image records its semantic
digest.

The first version read the type from the entry frame at capture time.
It was wrong for a capture taken inside a proc constructor, which
would have named the constructor result instead of the proc result.

### Restore clamps the limits

An image is data, so it may claim any budget. Restore takes the
minimum of the captured limits and the limits the restoring machine
already runs under, field by field. A restored world therefore never
grows past the authority of the machine that built it. Every machine
past the root also charges one unit of the restorer's child budget.

### `as_call` answers a self snapshot

A restored self snapshot holds `Vm.SnapshotSelf` pending, and the
restorer answers it through the ordinary typed call path
(specification 17.6). `as_call` therefore admits that one machine
control operation beside the fixed host operations, and the manifest
names its reply type `Result[SnapshotImage, SnapshotError]`. Every
other machine control operation stays outside `as_call`.

## Simplifications inside the slice

- **A capture cannot copy a nested driver stack.** A machine that a
  driver holds mid flight keeps activation state outside its own
  record. Two shapes are still copyable: the root of a self snapshot,
  whose activation belongs to the original world, and a machine that
  blocked with its own one-activation stack stored, which the
  scheduler rebuilds when the block clears. Anything deeper faults the
  caller with `InvalidVmState` and a message that names the reason.
- **The container carries no type table.** Specification 17.2 lists
  one. A restore builds into the same program, so the class and
  function manifests plus the recorded result-type digests name every
  type the image needs. A cross-program restore would need the table,
  and it would need much more besides. The work after week 9 reversed
  this: container format 2 carries a closed type table, and
  `docs/notes/post-week9-admission.md` states why.
- **`Vm.LoadSnapshot` has no guest form.** The operation takes a
  `Bytes` value, and version 0.2 declares no `Bytes` type. The
  verifier rejects the instruction with that reason. The host and the
  command line carry the byte paths instead.
- **`SnapshotImage.cast_result` is a host method.** The guest form
  takes a `Type[T]` descriptor, which version 0.2 does not have. The
  host form carries the same rule against the recorded result-type
  digest.
- **The round-trip case walks a bounded prefix.** The property is a
  property of one boundary, and a factorial walks thousands of
  instructions, so the case walks the first forty boundaries of each
  pure example.
- **The trusted-image table has a byte-bounded cache.** Eviction makes
  the next restore repeat admission. The cache never rejects an image.
- **A block record names its target by ordinal alone.** The generation
  comes from the restored target, so a restored block never reads as a
  dead peer. A proc slot is never reused today, so the two always
  agreed anyway.

## Changed tests

- `crates/lm-testkit/tests/week8_barrier.rs`, one case reads the new
  `BarrierError::ResourceActive` shape, which names the bounded
  machine path by ordinal instead of one machine identifier.
- `crates/lm-proc/tests/records.rs`, the barrier report gained the
  canonical machine order.
- `crates/lm-testkit/tests/week4_verifier.rs`, the `as_call` rejection
  message moved from `not fixed` to `not answerable`.
- `crates/lm-testkit/tests/week7_graph.rs`, the shape count is sixteen
  and the checked digest output moved with the compiler ABI version.
- `crates/lm-testkit/tests/fuzz.rs` reads the core role table at its
  new size through `CORE_ROLE_COUNT` instead of a constant.
- `crates/lm-heap/src/shape.rs`, the sample list, the tag table, and
  the boundary-column pin cover the `Snapshot` shape.
- `core/pinned-hash.txt` and `core/pinned-core-defs.txt` regenerated.
- `tests/fuzz-regressions/*.lmbc` regenerated for container version
  13. The layer-specific rejection assertions are unchanged.

## New tests

- `crates/lm-testkit/tests/week9_snapshot.rs`, 23 cases: the two
  runnable outputs, the boundary round trip over seven examples, the
  deterministic machine ordinals, the reproducible byte string, the
  closed handle set, handle relocation, multi-shot independence, the
  policy exclusion through a mock closure, the restored table, both
  failure-atomicity directions, the one-time external check, the five
  captured states, the byte limit, the world gate, and the
  deterministic dump diff, and the typed cast.
- `crates/lm-testkit/tests/week9_image.rs`, 28 cases: the container
  frame, the section table, the header, the code manifest, canonical
  integers, the canonical heap order, unreachable objects, handle
  generations, machine references, the state rules, the layout rules,
  the mailbox rules, the literal rule, the declared-type rules for
  locals, fields, operands, pending arguments, messages, and terminal
  values, the terminal value with no result type, the non-proc queue,
  the proc with an underivable mailbox type, the cyclic parent graph,
  the terminal frame and the frameless operand, a blanket single-bit
  sweep over the whole container, a truncation sweep, and the
  deep-graph case on a 256 KiB stack.
- `crates/lm-testkit/tests/fuzz.rs`, one snapshot surface: sixteen
  hundred mutation rounds against a real container. Every non-truncated
  mutant is resealed, so it reaches the structural loader instead of
  the container-hash gate. An accepted mutant must encode back to
  exactly the bytes it came from, and it must restore and run under a
  tight heap and fuel cap without a panic.
- `tests/fuzz-regressions/`, two container seeds: one valid machine
  world and one whose heap is not in canonical traversal order.
- `crates/lm-testkit/tests/bench_smoke.rs`, three snapshot entries.
- `crates/lm-cli/tests/cli.rs`, 8 new cases: the two examples through
  the tool, the verify verdict, the restore run, the byte-for-byte
  rewrite of the checkpoint, the readable dump, the shape table, and
  the rejection of a damaged container.
- `crates/lm-abi/src/fault.rs`, one case: every stable fault code
  round trips through its name.
- `tests/ui/`, 5 new pairs for the row rule, the two arity rules, the
  restore argument rule, and the `Snapshot` type arity.

Test count: 719 before, 783 after. Three cases followed the first
security review and three more followed the re-review, closing the
whole "underivable governing type" pattern in the loader, the parent
cycle, and the terminal-frame asymmetry.

## Measurements

`cargo test -p lm-testkit --test bench_smoke -- --nocapture --exact
<name>`, debug profile, one entry per process.

| Entry | Week 8 | Week 9 |
|---|---|---|
| `alloc_gc_100k` | 80.0 ms | 79.6 ms |
| `freeze_chain_50k` | 50.1 ms | 48.9 ms |
| `transfer_graph_20k` | 29.1 ms | 28.9 ms |
| `digest_graph_20k_plus_1k_cached` | 27.4 ms | 28.5 ms |
| `mark_sweep_100k_under_256k` | 107.6 ms | 103.6 ms |
| `proc_send_receive_20k` | 78.6 ms | 80.0 ms |
| `proc_spawn_500` | 13.3 ms | 14.5 ms |
| `proc_pause_resume_5k` | 10.5 ms | 10.3 ms |
| `proc_terminal_200x200` | 33.1 ms | 33.2 ms |

`proc_spawn_500` rose about nine percent. The operation manifest grew
from twenty-four slots to twenty-eight, so every fresh policy table
allocates four more entries, and a spawn allocates one table. Nothing
else on that path changed.

The three snapshot entries, by workload shape:

| Shape | Size | Machines | Write | Load | Restore |
|---|---|---|---|---|---|
| wide heap, 10k list elements | 90 440 B | 1 | 0.41 ms | 0.78 ms | 0.16 ms |
| deep chain, 5k instances | 115 423 B | 1 | 7.14 ms | 4.20 ms | 2.47 ms |
| machine world, three machines | 914 B | 3 | 0.02 ms | 0.22 ms | 0.04 ms |

The two large shapes hold a similar number of bytes and a very
different number of objects: the wide heap is two objects and the deep
chain is five thousand and one. The per-object cost dominates, which
is what the codec does. The restore column is one restore, averaged
over four runs for the two large shapes and twenty for the small one.

## The gates

| Gate | State |
|---|---|
| Snapshot round trips cover every bytecode boundary in the example corpus | Met. `a_snapshot_round_trips_at_every_boundary_of_the_example_corpus` walks seven pure examples. At each boundary the bytes decode, encode back byte for byte, and restore to a world that finishes with the same result. The case walks a bounded prefix; the simplification above states it. |
| Machine ordinals are deterministic and independent from scheduler IDs | Met. `machine_ordinals_follow_reachability_not_machine_identifiers` reads a world whose identifier order and reachability order disagree: the closed set is `[1, 2, 3]` and the canonical order is `[3, 2, 1]`. `one_world_shape_produces_one_byte_string` states the reproducibility. |
| Every handle in snapshot bytes targets a captured machine | Met. `every_handle_in_the_bytes_targets_a_captured_machine` walks a writer image, and `a_machine_reference_past_the_world_rejects` states the loader rule. |
| Handle relocation covers every VM and mailbox root | Met. `restore_relocates_every_vm_and_mailbox_root` walks every restored heap and proves no restored machine names a machine of the original world. |
| Multi-shot restore creates complete independent worlds | Met. `multi_shot_restore_creates_independent_worlds` runs the guest form, and `two_restores_share_nothing_with_each_other_or_the_original` reads the machine tables. |
| Policy tables and root grants never enter snapshot bytes | Met. `a_machine_reachable_only_through_a_mock_closure_is_not_in_the_world` and `a_restored_table_is_default_deny_plus_the_birth_grant`. The birth-grant decision above records what a restored proc does receive. |
| A failed restore exposes no partial world | Met. `a_failed_restore_exposes_no_partial_world` reads the machine count, the target state, the target heap, and the child reservation. |
| A failed snapshot resumes the original world | Met. `a_waiting_machine_names_its_attachment` reads the barrier and the freeze flag after the failure, and `a_capture_past_the_byte_limit_returns_the_typed_error` runs the world to completion afterwards. |
| The loader checks machine references, mailbox types, limits, and accepted values | Met. `week9_image.rs`. The mailbox type comes from the proc class, never from the image. |
| Whole-image structural verification occurs once on external load | Met and instrumented. `World::snapshot_checks` counts the passes, and `external_bytes_are_checked_once_and_the_trusted_path_checks_nothing` loads once and restores twice. |
| In-process trusted restore and external byte load remain separate APIs | Met. `World::capture_snapshot` never decodes, and `World::load_snapshot_bytes` is the only entry that checks. The same case reads both. |
| Snapshot size/load/write benchmarks are tracked by workload shape | Met. Three entries above. |

## Open questions

### Does a restored proc take the birth grant?

The decision above says it does. The tension is real: specification
17.5 says "each restored machine receives a fresh default-deny table",
and specification 18.3 says a launch mints the `Proc` group. A restore
is a launch of a world that already existed, so both texts describe
it, and they describe it differently. The implementation follows 18.3
for a proc and 17.5 for everything else. The project owner decides
which text moves.

### The result type of a machine with no entry frame

`Machine::result_ty` comes from the entry frame at load time. A
machine that never loaded a frame, for example a captured `EmptyVm`,
records no result type. Such a machine stores no terminal value
either, because a machine reaches a terminal only by running a frame.

The loader does not take that pairing on trust: a forged image is
exactly what `load_external` defends against, so a `Done` machine
whose stored value is not the unit must carry a result type, and the
loader rejects the image when it is absent. Without the rule a forged
image could strip the result type and hand a wrong-typed terminal
value to a consumer that reads it at the declared result type. The
unit value alone needs no type, because it passes at every type.

The open part is smaller: whether the field should live in `VmState`
instead of `Machine`, which would make it serializable by
construction rather than by an explicit record.

### The receiver-heap fault attribution stays open

The week-8 question is unchanged: a `send` whose copy exceeds the
receiving heap faults the sender. A restore reaches the same path
when it fills a mailbox, and it answers with `RestoreLimitExceeded`
instead. The two answers are consistent with their own callers and
with each other, and the underlying question is still open.

### A snapshot inside a snapshot is opaque

A captured heap may hold a `Snapshot` value, and the container writes
its bytes as an opaque payload. The loader does not open it. The
restore of such a value runs the loader over it, because the hash
lookup misses for bytes this process did not write. The rule is
sound, and the cost is one full check per nested restore. Whether the
outer load should check a nested image once, and remember it, is open.

### Is the machine path the right shape for `ResourceActive`?

The typed error carries `path: List[Int]`, and the ordinals are the
canonical machine ordinals of the world the capture would have
written. A caller that wants to close the attachment holds handles,
not ordinals, so the path names the machine without naming a way to
reach it. A `List[Handle]` would name a way, and it would put live
references inside an error value.

### The classification is still outside the manifest digest

The week-7 question stands: `OpDef.snapshot` is manifest content, and
`manifest_digest` does not cover it.

### The load does not enforce a deep-freeze invariant

The only frozen rule at load is that a born-frozen shape carries the
frozen bit. The loader does not enforce that every object reachable
from a frozen object is frozen. The second re-review asked whether the
iterative reachability walk should add it.

It must not, because deep freeze is not a system invariant. A `Tuple`
and a `Closure` are born frozen, and their elements need not be: every
tuple literal is a frozen object over possibly mutable elements. A
walk that rejected a frozen object with a mutable child would reject
those valid images.

The invariant the rest of the system relies on is narrower and is
checked where it is used, not assumed of stored state. `digest` and
`deep_equal` require the whole reachable graph frozen, and their graph
modes prove it at the call, rejecting a mutable object with
`UnsendableValue`. The write barrier reads the frozen bit per object,
so a mutable child reachable from a frozen parent is writable and the
parent is not. A forged image that froze a container and left an
element mutable is therefore inert and sound: no consumer reads the
element as frozen without the call-time check catching it.

The image cannot separate a born-frozen container from a
user-deep-frozen graph either, because the frozen bit is one bit. So
even the narrower "a user-frozen graph is deep-frozen" rule is not
expressible at load. The question stays recorded rather than enforced.

## The blind security review

A blind reviewer read the whole loader. The byte layer held: no panic,
no unbounded allocation, no hang, and no memory-unsafety across a
five-hundred-thousand-round reseal fuzzer; the decode is iterative;
every length and count is checked before an allocation; and the closed
world holds. The reviewer found three defects in the type proofs, all
one pattern: a type check keyed on a record that a forged image can
leave absent.

- **F1, the serious one.** The terminal type proof fired only when the
  machine recorded a result type. A forged `Done` machine with the
  result type stripped and an arbitrary value skipped the proof, and
  the wrong-typed value reached a consumer that reads it at the
  declared result type. The fix: a `Done` value that is not the unit
  must carry a result type, and the loader rejects the image when it
  is absent. `a_terminal_value_without_a_result_type_rejects` reproduces
  the stripped image and asserts rejection.
- **F2a.** `mailbox_type` returns `None` for a non-proc machine, so a
  queue on a non-proc sat unchecked. A restored non-proc gets no birth
  grant and cannot receive, so the queue is inert; the loader now
  rejects the image anyway rather than trusting it.
  `a_non_proc_machine_with_a_queued_message_rejects` states it.
- **F2b.** Pending arguments were checked against the manifest, which
  names types for a fixed operation alone, so a machine captured
  `asked` on a machine control operation carried unchecked arguments.
  The rule now reads the operand types the verifier proved just before
  the perform, for every operation kind. The decision above states the
  contract.
  `a_pending_argument_of_the_wrong_shape_or_count_rejects` states it.
- **F3, test methodology.** The committed loader fuzzer mutated the
  bytes but did not recompute the container hash, so almost every
  mutant died at the hash gate and the structural loader saw
  near-zero coverage. The container hash is an unkeyed integrity check,
  so the real attacker holds a hash-valid crafted image. The fuzzer now
  reseals every non-truncated mutant, and a probe confirmed the
  rejections spread across the whole structural surface instead of the
  hash gate. It stays deterministic and seeded.

The three type fixes share one shape: a value that carries a declared
type is proved whether or not the record that names the type is
present. Where the record is absent and the value is not the trivial
one, the loader rejects.

### The re-review: the whole pattern, not the representative case

A second uninterrupted review found the first round of fixes closed
the representative cases, not the pattern. The governing rule is now
stated plainly: a value in a value-bearing position whose governing
type cannot be derived is rejected at load, never skipped.

- **The mailbox-queue proof (HIGH).** The F2a fix rejected a non-proc
  machine with a queue, but a proc whose mailbox type could not be
  derived from the image still left its queue unproven. The type is
  underivable when the proc body is gone and the entry frame declares
  no proc instance, or the first parameter is not a proc class. The
  loader now rejects any proc that carries a message when the mailbox
  type does not derive.
  `a_proc_with_an_underivable_mailbox_type_and_a_queue_rejects` states
  it.
- **Parent-chain acyclicity (MEDIUM).** The parent ordinal was range
  checked but never proven acyclic. A self parent or a cycle decoded,
  and the runtime policy walk of `resolve_policy` followed the parent
  chain with no bound, so a live cyclic segment spun forever. Two
  layers close it: the loader proves the parent graph a forest with an
  iterative three-colour walk, and `resolve_policy` gained a step bound
  that fails closed past the machine count. `a_cyclic_parent_graph_rejects`
  states both the self parent and the two-node cycle.
- **Terminal frames and frameless operands (LOW).** The operand proof
  skips a terminal machine, but nothing forbade a terminal machine from
  carrying a frame, and nothing forbade a frameless machine from
  carrying operands, so a forged terminal machine could hold unproven
  operands. The chosen fix forbids both shapes: a terminal machine
  holds no frame, and a machine with no frame holds no operand. Both
  are true of every real capture. `a_terminal_machine_with_a_frame_or_a_frameless_operand_rejects`
  states them.
- **The deep-freeze question.** Recorded as an open question above:
  born-frozen containers legitimately hold mutable children, so the
  invariant is not enforceable at load without rejecting valid images,
  and the narrower rule the system relies on is checked at the digest
  and equality call sites instead.

The re-review also asked for the fuzzer to restore and run every
accepted mutant, not only decode it. It now does, under a tight heap
and fuel cap and a bounded slice budget. A fifty-thousand-round probe
accepted 9709 mutants, each re-encoded byte for byte and restored and
driven without a panic, and rejected the rest across the whole
structural surface, `mailbox` and `state` included.

## Deferred work

- `Bytes`, and with it the guest `SnapshotImage.to_bytes`,
  `Snapshot[T].to_bytes`, and `sys.vm.load_snapshot`. The shape table
  already states that `Bytes` joins the sendable column.
- `Type[T]` descriptors, and with them the guest
  `SnapshotImage.cast_result` and `SnapshotImage.result_type`.
- `Vm.Stack` and a source-mapped `stack()`. The bytecode carries no
  source spans, so a source map does not exist to read. `lm inspect`
  prints the frame listing of a container in the meantime.
- Incremental checkpoints. The container leaves room for a delta
  section; nothing implements one.
- A cross-program restore. The container names its program by semantic
  hash and rejects any other, which is the safe answer and not the
  general one.
- The remaining week-8 items: one scheduler task per proc, retiring a
  terminal proc record, attenuated send-only handle views, rewriting an
  inherited field default, a `Never` encoding, and distribution.
- Committed benchmark distributions, `cargo-fuzz` targets, Miri, and
  CI workflow files stay deferred as before.

## Maintenance note

`checkpoints/asked-tree.lms` is a checked-in container. It names its
program by semantic hash and its build by four version fields, so it
must be regenerated whenever the core image, the manifest, the
compiler ABI, the verifier version, or the container format moves:

```sh
lm snapshot save --allow Proc,Vm,Clock \
  checkpoints/asked-tree.lm checkpoints/asked-tree.lms
cargo test -p lm-testkit --test fuzz regenerate_fuzz_corpus -- --ignored
```

`snapshot_save_rewrites_the_checkpoint_byte_for_byte` fails until both
run, so a stale checkpoint cannot pass unnoticed.

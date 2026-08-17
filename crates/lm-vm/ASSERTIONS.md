# The assertion inventory of the execution and restore path

This document lists every assertion that an untrusted snapshot
container can reach on the interpreter path or on the restore path. It
covers `unreachable!`, `expect`, `unwrap`, `panic!`, `assert`, and
every unchecked slice index in these files:

- `crates/lm-vm/src/machine.rs`
- `crates/lm-vm/src/world.rs`
- `crates/lm-vm/src/lib.rs`
- `crates/lm-vm/src/snapshot/restore.rs`
- `crates/lm-heap/`
- `crates/lm-graph/`

## The rule this document serves

> No container, valid or forged, may panic or abort the host through
> execution or restore.

An assertion stays only when a rule proves it cannot fire. Every other
assertion is now a machine fault. A machine fault stops one machine and
leaves the host running.

The rules that carry a surviving assertion must be rules that admission
still proves. Admission proves resolved structure. It proves no type of
a stored value, so no entry below may name a type rule.

## The two fault codes

`FaultCode::TypeMismatch` names a value that does not carry the type
its program point expects. `FaultCode::MalformedState` names stored
machine state that does not match the code it runs. Both live in
`crates/lm-abi/src/fault.rs`.

## Part 1: the assertions that became faults

### `crates/lm-vm/src/machine.rs`

The interpreter reads its instruction stream, its arenas, and its heap
from state a container states. Every read below now tests instead of
asserting.

| Position | Old assertion | Now |
| --- | --- | --- |
| `exec_instr` instruction fetch | `frames.last().expect`, `module.funcs[frame.func]`, `blocks[frame.block]`, `block[frame.ip]` | `MalformedState` |
| `LoadLocal`, `StoreLocal` | `frames.last().expect`, `locals[base + slot]` | `MalformedState` |
| `pop` | `operands.pop().expect("verified stack shape")` | `MalformedState` |
| `pop_int`, `pop_bool`, `pop_obj` | `unreachable!("verified operand type")` | `TypeMismatch` |
| `peek` for a virtual receiver | `operands[len - 1 - argc]` | `MalformedState` |
| `CallVirtual`, `CallVirtualG` receiver | `as_obj().expect`, `unreachable!("verified receiver shape")` | `TypeMismatch` |
| `DispatchRow::method` | two `debug_assert` plus an unchecked index and an unchecked subtraction | `TypeMismatch` |
| `CallValue` | `operands.len() - 1 - argc`, `as_obj().expect`, `unreachable!("verified closure shape")` | `MalformedState`, `TypeMismatch` |
| `MakeClosure` | `operands.len() - captures` | `MalformedState` |
| `LoadCapture` | `frames.last().expect`, `closure.expect`, `unreachable!`, `captures[idx]` | `MalformedState`, `TypeMismatch` |
| `New`, `NewG`, `Call`, `CallG`, `push_frame`, `load_frame` | `module.funcs[callee]`, `module.classes[class]` | `MalformedState` |
| `ConstStr` | `module.strings[idx]` | `MalformedState` |
| `TupleNew`, `ListNew`, `MapNew`, `Perform`, `PerformValue`, `push_frame` | `operands.len() - count` | `MalformedState` |
| `TupleGet` | `items[index]`, `unreachable!("verified tuple shape")` | `TypeMismatch` |
| `LoadField`, `StoreField` | `fields[field]`, `unreachable!("verified instance shape")` | `TypeMismatch` |
| `ListLen`, `ListAt`, `ListPush` | `unreachable!("verified list shape")` | `TypeMismatch` |
| `MapLen`, `MapHas`, `MapAt`, `MapPut`, `map_lookup` | five `unreachable!("verified map shape")`, `entries[i]`, `entries[pos]` | `TypeMismatch` |
| `SbAppendStr`, `SbBuild`, `sb_append` | `unreachable!("verified string shape")`, `unreachable!("verified builder shape")` | `TypeMismatch` |
| `BbAppend`, `BbLen`, `BbBuild` | `unreachable!("verified buffer shape")` | `TypeMismatch` |
| `EqDigest`, `NeDigest` | `unreachable!("verified digest operand")` | `TypeMismatch` |
| `EqStr`, `NeStr` | `unreachable!("verified operand type")` | `TypeMismatch` |
| `FaultCode` | `unreachable!("verified fault shape")` | `TypeMismatch` |
| `PerformValue` operation value | `unreachable!("verified operation value")` | `TypeMismatch` |
| `Jump`, `JumpIfFalse`, `JumpIfTrue`, `Return` | `frames.last_mut().expect`, `frames.pop().expect` | `MalformedState` |
| `IsType`, `CastType`, `instance_matches` | `module.types[ty]`, `unreachable!` twice, `module.classes[class]`, an unbounded parent walk | `MalformedState`, `TypeMismatch` |

`instance_matches` also gained a step bound. The class chain of a
verified module is acyclic, and the bound holds whatever built the
table, so the walk cannot spin.

### `crates/lm-vm/src/world.rs`

| Position | Old assertion | Now |
| --- | --- | --- |
| `run_root` | two `unreachable!` | a terminal fault outcome |
| `resume_stack` | `suspended.remove().expect` | `MalformedState` |
| `control` | `unreachable!("the world caller controls a paused machine")` | `MalformedState` |
| `terminal_root_event` | `unreachable!("a terminal machine stores its result")` | `MalformedState` |
| `drive_stack` | `stack.len() - 1`, `unreachable!("an empty or asked machine ...")` | `MalformedState` |
| `wait_token` | two `expect` | the reserved token, which the host answers with a failure |
| `finish` | `stack.pop().expect` | one `Ran` event |
| `deliver_mock`, `build_terminal_event`, `publish_terminal` | three `unreachable!("a terminal machine stores its result")` | a fault record |
| `done_arm`, `fault_arm` | two `unreachable!("mock exits carry no event")` | `None`, then `MalformedState` |
| `make_instance` | `class.expect("the verifier requires the whole core family")` | `MalformedState` |
| `handle_perform` | `stack.last().expect`, `stack.pop().expect` | `MalformedState` |
| `resolve_and_dispatch` | `pending_op().expect` | `MalformedState` |
| `start_wait` | `pending.expect("the pending perform waits")` | `MalformedState` |
| `host_args` | `pending.expect`, two `unreachable!("verified operation argument shape")` | `MalformedState`, `TypeMismatch` |
| `handle_vm` | `as_obj().expect`, `unreachable!("verified handle shape")` | `TypeMismatch` |
| `handle_proc` | `as_obj().expect`, `unreachable!("verified handle shape")` | `TypeMismatch` |
| `kernel_exec`, `proc_exec` | `pending.expect`, `args[0..2]` | `MalformedState`, the uninitialized marker, then `TypeMismatch` |
| `OP_VM_FROM_OBJECT` | `as_obj().expect`, two `unreachable!` | `TypeMismatch` |
| `OP_VM_RUN`, `OP_VM_STEP`, `OP_VM_DRIVE` | `unreachable!("a running machine holds an active reference")`, `pending.as_mut().expect` | `InvalidVmState`, `MalformedState` |
| `OP_VM_ANSWER` | `as_obj().expect`, `unreachable!("verified call token shape")`, `pending.expect` | `TypeMismatch` |
| `OP_VM_REJECT`, `OP_VM_DISPATCH` | `as_obj().expect` twice, two `unreachable!`, `pending.expect` | `TypeMismatch` |
| `OP_VM_RESTORE` | `as_obj().expect`, `unreachable!("verified snapshot shape")` | `TypeMismatch` |
| `kernel_exec` tail, `proc_exec` tail | two `unreachable!("every ... slot has a kernel rule")` | `MalformedState` |
| `build_snapshot_error` | `as_obj().expect("a list is a heap object")` | `MalformedState` |
| `start_mock` | `pending.expect`, `as_obj().expect`, `unreachable!("a mock handler is a closure")` | `MalformedState`, `TypeMismatch` |
| `enter_proc_body` | `start_body.take().expect`, `unreachable!("a proc body is a closure")` | `MalformedState`, `TypeMismatch` |
| `proc_spawn` | `unreachable!("verified argument view shape")`, two `as_obj().expect`, two `unreachable!`, `group_by_name().expect` | `TypeMismatch`, `MalformedState` |
| `handle_table_edit` | `unreachable!("verified table handle shape")`, `mock.expect`, `as_obj().expect`, `exact[slot]`, `group[slot]` | `TypeMismatch`, `MalformedState` |
| `handle_as_call` | `unreachable!("verified request shape")` | `TypeMismatch` |
| `handle_call_args` | `unreachable!("verified call shape")`, `pending.expect` | `TypeMismatch`, an empty argument list |
| `poll_blocked` | two `expect("a blocked machine holds its pending perform")` | `fail_blocked` |
| `drive_proc` | `unreachable!("a proc slice runs to a terminal, a block, or a wait")` | `MalformedState` and `ProcStop::Terminal` |
| `two` | `assert_ne!(a, b)` | `transfer` routes an equal pair to the one-heap copy |
| `run_machine` | `pending.expect("an asked machine holds its request")` | `MalformedState` |

### `crates/lm-graph/src/mode.rs`

| Position | Old assertion | Now |
| --- | --- | --- |
| `copy_passes`, `copy_passes_within` | `shell().expect("pass 1 admitted sendable shapes only")` | `UnsendableValue` |

`crates/lm-heap/src/shape.rs` gained
`a_shape_has_a_shell_exactly_when_it_is_sendable`, so the shape check
of pass 1 and the shell of pass 2 cannot drift.

### `crates/lm-vm/src/snapshot/admit.rs`

Admission reads container data directly, so its own reads must be
total.

| Position | Old assertion | Now |
| --- | --- | --- |
| `check_state` block rule | `pending.expect("a blocked machine has a request")` | an admission rejection |
| `check_world` token rule | `pending.expect("an asked machine holds its request")` | a matched pattern |

## Part 2: the assertions that stay, and the rule that carries each

### `Heap::get`, `Heap::get_mut`, `Heap::is_frozen`, `Heap::set_frozen`, `Heap::recharge`, `Heap::free`

`crates/lm-heap/src/lib.rs:239`, `:248`, `:259`, `:267`, `:275`, `:291`

Each call asserts that the reference is live and generation-current.
Three rules carry them together:

1. `crates/lm-vm/src/snapshot/admit.rs:744` `check_references` proves
   that every object ordinal of a container names one stored object of
   its own machine. It proves this for every local, every operand,
   every pending argument, the terminal value, every mailbox message,
   every frame closure, the proc body, every literal, and every child
   of every stored object. The rule is structural, so it survives the
   removal of the type proof.
2. `crates/lm-vm/src/snapshot/restore.rs:194` `restore_heap` allocates
   one destination object for each captured object, in order, and
   patches every child through the `refs` table. A restored value
   therefore names a slot the restore itself allocated.
3. At run time every `ObjRef` comes from `Heap::alloc`, and
   `crates/lm-vm/src/machine.rs:530` `gc_roots` lists every place one
   machine holds a reference outside its heap. A collection frees a
   slot the roots do not reach, so no live value names a freed slot.

`Heap::entry` at `crates/lm-heap/src/lib.rs:182` indexes the page
table with the slot. A slot never leaves the table once it exists, and
rule 1 and rule 2 bound every slot a container supplies, so the index
holds.

These calls sit on the hottest read path of the interpreter. A fault
here would add one branch to every heap read, and the three rules above
are structural and complete, so the assertions stay.

### `Heap::pop_host_root`

`crates/lm-heap/src/lib.rs:310`

The assertion states a LIFO discipline between one push and one pop
inside `lm-vm` and `lm-graph`. A container states no host root, so no
container reaches it. The comment at the call states that a silent
wrong pop would unroot a live object, which is worse than a stop.

### `lm_graph::collect`

`crates/lm-graph/src/mode.rs:29`

`walk` answers `Err` for a limit or for a rejecting visitor alone. The
call passes `GraphLimits::UNBOUNDED` and the unit visitor, which
rejects nothing, so the call is total.

### `DispatchRow` construction

`crates/lm-vm/src/lib.rs:389`

`methods.iter().max().expect("non-empty")` sits inside the `Some(base)`
arm of `methods.iter().min()`, so the list holds an entry. The input is
the verified class table, never a container.

### `restore_heap` and `relocate_machines`

`crates/lm-vm/src/snapshot/restore.rs:226`, `:449`

`Object::remap` and `Object::shell` answer `Some` for every shape with
`has_refs`, and both calls sit behind a `has_refs` test.
`crates/lm-heap/src/shape.rs` `the_three_shape_walks_agree` proves the
agreement.

### `restore_state` proc grant

`crates/lm-vm/src/snapshot/restore.rs:292`

`lm_abi::group_by_name("Proc")` reads the static operation manifest of
this build. The manifest declares the group, and
`crates/lm-abi/src/lib.rs` fixes it at compile time, so no input
reaches the answer.

### Snapshot capture

`crates/lm-vm/src/snapshot/write.rs:250`, `:672`, `:729`, `:735`

Capture reads one live machine world that this process built. No
container reaches capture: `codec::decode` and `admit` are the one
path from bytes into a host image.

## Part 3: the decisions this inventory made

### The operand-stack depth rule left admission, so `pop` faults

`check_operands` proved the exact operand count of each frame from the
verifier dataflow at the stop point. That rule shared its evidence with
the type proof, and the type proof is gone.

The remaining structural rules are weaker: `check_state` proves that
frame operand bases start at zero, never decrease, and stay inside the
arena. They permit a machine to resume with fewer operands than its
program point pops.

`Machine::pop` therefore returns `Result<Value, FaultCode>`.
`size_of::<Result<Value, FaultCode>>()` is 16 bytes, exactly
`size_of::<Value>()`, because the value tag holds a niche. The change
costs about 8 percent on the two collection-dominated benchmarks and
stays inside the measurement noise everywhere else.

Note what the fault does and does not add. A pop past the region of
one frame still reads the operand a lower frame owns, in both designs.
The fault covers the case where the whole arena is empty. The typed
readers cover every other case by testing the tag.

### `Heap::get` keeps its assertion

The three rules of part 2 are structural, complete, and outside the
type proof. `Heap::get` runs once or more for each instruction that
touches the heap, so a fault there would cost more than the rules are
worth.

### The uninitialized marker fills a missing kernel argument

`kernel_exec` and `proc_exec` read fixed argument positions. A
container states its own argument list, so a position can be missing.
`Args` answers `Value::Uninit` there. Every shape test of every kernel
rule rejects that marker, so a short list faults the caller. The
alternative, a padded vector, allocated twice for each proc operation
and cost about 20 percent of `proc_send_receive_20k`.

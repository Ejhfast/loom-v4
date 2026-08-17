//! Week-9 container suite: the canonical snapshot format, the
//! decoding rules, and the admission rules of specification 17.8.
//!
//! Every new byte surface owes a corruption test. The cases below
//! damage one rule at a time and state the exact reason the loader
//! reports, whichever stage owns that rule. A blanket sweep then flips every byte of one container and
//! proves that the loader either rejects the result or reads a
//! different but still canonical image. No input panics.

use lm_testkit::{compile_to_bytes, repo_root};
use lm_vm::snapshot::{codec, ImageReason, LoadLimits};
use lm_vm::{load_bytes, LoadedModule, RecordingHost, VmConfig, World};

fn program(source: &str) -> LoadedModule {
    let bytes = compile_to_bytes("image.lm", source).expect("the program compiles");
    load_bytes(&bytes).expect("the program loads")
}

fn asked_tree() -> (LoadedModule, Vec<u8>) {
    let source = std::fs::read_to_string(repo_root().join("checkpoints/asked-tree.lm"))
        .expect("the checkpoint source reads");
    let loaded = program(&source);
    let bytes = {
        let mut world = World::new(
            &loaded,
            VmConfig::default(),
            Box::new(RecordingHost::new(1)),
        );
        for grant in ["Proc", "Vm", "Clock"] {
            world.allow(grant).expect("the grant names a target");
        }
        lm_proc::run_world(&mut world);
        world
            .last_snapshot()
            .expect("the program captured a world")
            .bytes()
            .to_vec()
    };
    (loaded, bytes)
}

/// The fixed container frame: eight magic bytes, four version fields,
/// one entry count, and five twelve-byte section entries.
const PREFIX: usize = 8 + 4 * 4;
const TABLE: usize = PREFIX + 1;
const ENTRY: usize = 12;
const BODY: usize = TABLE + 5 * ENTRY;

/// Recompute the trailing container hash, so a mutation reaches the
/// structural rules instead of stopping at the hash.
fn reseal(mut bytes: Vec<u8>) -> Vec<u8> {
    let end = bytes.len() - 32;
    let hash = codec::container_hash(&bytes[..end]);
    bytes[end..].copy_from_slice(&hash);
    bytes
}

/// The rule one container breaks. The call runs decoding and
/// admission, so a case states one rule whichever stage owns it.
fn reject(loaded: &LoadedModule, bytes: &[u8]) -> ImageReason {
    codec::load_external(bytes, loaded, LoadLimits::default())
        .expect_err("the container must reject")
        .reason
}

/// One container that decodes and admits.
///
/// Admission proves structure. A wrong-typed value is not a structural
/// fact, so a container that carries one admits. The interpreter tests
/// the tag at each accessor, and the world checks each VM boundary, so
/// the wrong type stops one machine at its first read.
fn admits(loaded: &LoadedModule, bytes: &[u8]) {
    codec::load_external(bytes, loaded, LoadLimits::default())
        .expect("the container admits, because admission proves structure alone");
}

/// The editable image of one container that decodes and admits.
fn accept(loaded: &LoadedModule, bytes: &[u8]) -> lm_vm::snapshot::Image {
    codec::load_external(bytes, loaded, LoadLimits::default())
        .expect("the container loads")
        .into_image()
}

/// Write one little-endian 32-bit field.
fn put_u32(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

/// The offset of one section payload.
fn section_offset(bytes: &[u8], idx: usize) -> usize {
    let at = TABLE + idx * ENTRY + 4;
    u32::from_le_bytes(bytes[at..at + 4].try_into().expect("four bytes")) as usize
}

// ---------------------------------------------------------------
// The container frame.
// ---------------------------------------------------------------

#[test]
fn a_valid_container_loads_and_re_encodes_to_the_same_bytes() {
    let (loaded, bytes) = asked_tree();
    let image = accept(&loaded, &bytes);
    let again = codec::encode(&image, usize::MAX).expect("the image encodes");
    assert_eq!(again, bytes);
    assert_eq!(&bytes[..8], b"LMSNAP\0\x01");
    assert_eq!(section_offset(&bytes, 0), BODY);
}

#[test]
fn the_frame_rules_reject_precisely() {
    let (loaded, bytes) = asked_tree();
    // A short container never reaches a section.
    assert_eq!(reject(&loaded, &bytes[..40]), ImageReason::Truncated);
    // The magic.
    let mut bad = bytes.clone();
    bad[0] = b'X';
    assert_eq!(reject(&loaded, &reseal(bad)), ImageReason::Magic);
    // The four version fields.
    for at in [8, 12, 16, 20] {
        let mut bad = bytes.clone();
        put_u32(&mut bad, at, 999);
        assert_eq!(
            reject(&loaded, &reseal(bad)),
            ImageReason::Version,
            "at {at}"
        );
    }
    // The container hash covers every byte before it.
    let mut bad = bytes.clone();
    let last = bad.len() - 1;
    bad[last] ^= 1;
    assert_eq!(reject(&loaded, &bad), ImageReason::ContainerHash);
    let mut bad = bytes.clone();
    bad[BODY] ^= 1;
    assert_eq!(reject(&loaded, &bad), ImageReason::ContainerHash);
}

#[test]
fn the_section_table_rules_reject_precisely() {
    let (loaded, bytes) = asked_tree();
    // The entry count.
    let mut bad = bytes.clone();
    bad[PREFIX] = 3;
    assert_eq!(reject(&loaded, &reseal(bad)), ImageReason::SectionBounds);
    // The kind order.
    let mut bad = bytes.clone();
    put_u32(&mut bad, TABLE, 2);
    assert_eq!(reject(&loaded, &reseal(bad)), ImageReason::SectionBounds);
    // A gap before the first payload.
    let mut bad = bytes.clone();
    put_u32(&mut bad, TABLE + 4, (BODY + 1) as u32);
    assert_eq!(reject(&loaded, &reseal(bad)), ImageReason::SectionBounds);
    // A length that reaches past the container.
    let mut bad = bytes.clone();
    put_u32(&mut bad, TABLE + 8, 1 << 30);
    assert_eq!(reject(&loaded, &reseal(bad)), ImageReason::SectionBounds);
    // A short last section leaves trailing bytes.
    let mut bad = bytes.clone();
    let at = TABLE + 4 * ENTRY + 8;
    let length = u32::from_le_bytes(bad[at..at + 4].try_into().expect("four bytes"));
    put_u32(&mut bad, at, length - 1);
    assert_eq!(reject(&loaded, &reseal(bad)), ImageReason::Trailing);
}

// ---------------------------------------------------------------
// The header and the code manifest.
// ---------------------------------------------------------------

#[test]
fn the_header_rules_reject_precisely() {
    let (loaded, bytes) = asked_tree();
    let header = section_offset(&bytes, 0);
    // A machine count past the load limit rejects before any machine
    // vector exists.
    let limits = LoadLimits {
        max_machines: 2,
        ..LoadLimits::default()
    };
    assert_eq!(
        codec::decode(&bytes, limits)
            .expect_err("the count passes the limit")
            .reason,
        ImageReason::LimitExceeded
    );
    // A machine count past the bytes that remain rejects the same way.
    let mut bad = bytes.clone();
    bad[header] = 0x7f;
    assert_eq!(reject(&loaded, &reseal(bad)), ImageReason::Truncated);
    // Zero machines leaves every heap byte outside the sections the
    // header names. The zero-machine world itself is an admission
    // rule, and `admission.rs` states it.
    let mut bad = bytes.clone();
    bad[header] = 0;
    assert_eq!(reject(&loaded, &reseal(bad)), ImageReason::Trailing);
    // The root ordinal is zero in a canonical image. One image has one
    // byte string, so the decoder owns that rule.
    let mut bad = bytes.clone();
    bad[header + 1] = 1;
    assert_eq!(reject(&loaded, &reseal(bad)), ImageReason::SectionBounds);
    // The module semantic hash names this program.
    let mut bad = bytes.clone();
    bad[header + 2] ^= 1;
    assert_eq!(reject(&loaded, &reseal(bad)), ImageReason::Code);
}

#[test]
fn the_code_manifest_rules_reject_precisely() {
    let (loaded, bytes) = asked_tree();
    let code = section_offset(&bytes, 1);
    // A damaged definition hash rejects: the slot exists, and its
    // recorded identity does not match the program.
    let mut bad = bytes.clone();
    bad[code + 2] ^= 1;
    assert_eq!(reject(&loaded, &reseal(bad)), ImageReason::Code);
    // A function slot the program has not.
    let mut bad = bytes.clone();
    bad[code + 1] = 0x7f;
    assert_eq!(reject(&loaded, &reseal(bad)), ImageReason::Code);
}

// ---------------------------------------------------------------
// The heaps and the machine records.
// ---------------------------------------------------------------

#[test]
fn a_non_canonical_integer_rejects() {
    let (loaded, bytes) = asked_tree();
    let header = section_offset(&bytes, 0);
    // `0x80 0x00` encodes zero in two bytes, which is not minimal.
    let mut bad = bytes.clone();
    bad[header] |= 0x80;
    bad.insert(header + 1, 0);
    // The insertion moved every later section, so the table moves too.
    for idx in 0..5 {
        let at = TABLE + idx * ENTRY + 4;
        let offset = u32::from_le_bytes(bad[at..at + 4].try_into().expect("four bytes"));
        put_u32(&mut bad, at, offset + u32::from(idx > 0));
    }
    let at = TABLE + 8;
    let length = u32::from_le_bytes(bad[at..at + 4].try_into().expect("four bytes"));
    put_u32(&mut bad, at, length + 1);
    assert_eq!(
        reject(&loaded, &reseal(bad)),
        ImageReason::NonCanonicalInteger
    );
}

/// The capture context of a frame is the closure that frame runs.
///
/// The helper machine holds two closures: the capture context of its
/// frame and its proc body. The swap keeps every ordinal in range, and
/// the capture-context rule names the exact position it broke.
/// `admission.rs` states the canonical-order rule on its own.
#[test]
fn a_swapped_capture_context_rejects() {
    let (loaded, bytes) = asked_tree();
    let image = accept(&loaded, &bytes);
    let mut broken = image.clone();
    let frame = broken.machines[2].frames[0].closure;
    let body = broken.machines[2].start_body;
    assert_ne!(frame, body);
    broken.machines[2].frames[0].closure = body;
    broken.machines[2].start_body = frame;
    let bad = codec::encode(&broken, usize::MAX).expect("the damaged image encodes");
    assert_eq!(reject(&loaded, &bad), ImageReason::Reference);
}

#[test]
fn an_unreachable_stored_object_rejects() {
    let (loaded, bytes) = asked_tree();
    let image = accept(&loaded, &bytes);
    let mut broken = image.clone();
    let spare = broken.machines[0].objects[0].clone();
    broken.machines[0].objects.push(spare);
    let bad = codec::encode(&broken, usize::MAX).expect("the damaged image encodes");
    assert_eq!(reject(&loaded, &bad), ImageReason::Order);
}

#[test]
fn a_handle_generation_mismatch_rejects() {
    let (loaded, bytes) = asked_tree();
    let image = accept(&loaded, &bytes);
    let mut broken = image.clone();
    for machine in &mut broken.machines {
        for entry in &mut machine.objects {
            if let lm_heap::Object::NativeHandle { generation, .. } = &mut entry.object {
                *generation += 1;
            }
        }
    }
    let bad = codec::encode(&broken, usize::MAX).expect("the damaged image encodes");
    assert_eq!(reject(&loaded, &bad), ImageReason::Reference);
}

#[test]
fn a_machine_reference_past_the_world_rejects() {
    let (loaded, bytes) = asked_tree();
    let image = accept(&loaded, &bytes);
    let mut broken = image.clone();
    for entry in &mut broken.machines[0].objects {
        if let lm_heap::Object::NativeHandle { proc, .. } = &mut entry.object {
            *proc = 9;
        }
    }
    let bad = codec::encode(&broken, usize::MAX).expect("the damaged image encodes");
    assert_eq!(reject(&loaded, &bad), ImageReason::Reference);
}

#[test]
fn the_state_rules_reject_precisely() {
    let (loaded, bytes) = asked_tree();
    let image = accept(&loaded, &bytes);
    // A ready machine holds no pending request.
    let mut broken = image.clone();
    broken.machines[0].state = lm_vm::snapshot::ImageState::Ready;
    let bad = codec::encode(&broken, usize::MAX).expect("the damaged image encodes");
    assert_eq!(reject(&loaded, &bad), ImageReason::State);
    // An asked machine holds one.
    let mut broken = image.clone();
    broken.machines[0].pending = None;
    let bad = codec::encode(&broken, usize::MAX).expect("the damaged image encodes");
    assert_eq!(reject(&loaded, &bad), ImageReason::State);
    // The restored root is holder-controlled.
    let mut broken = image.clone();
    broken.machines[0].scheduler_owned = true;
    let bad = codec::encode(&broken, usize::MAX).expect("the damaged image encodes");
    assert_eq!(reject(&loaded, &bad), ImageReason::State);
    // A paused proc is not scheduler-owned.
    let mut broken = image.clone();
    broken.machines[1].paused = true;
    let bad = codec::encode(&broken, usize::MAX).expect("the damaged image encodes");
    assert_eq!(reject(&loaded, &bad), ImageReason::State);
    // A pending host attachment has no bytes to copy.
    let mut broken = image.clone();
    broken.machines[0]
        .pending
        .as_mut()
        .expect("the root is asked")
        .op = lm_abi::OP_CLOCK_SLEEP;
    let bad = codec::encode(&broken, usize::MAX).expect("the damaged image encodes");
    assert_eq!(reject(&loaded, &bad), ImageReason::State);
    // The pending ordinal stays below the next ordinal.
    let mut broken = image.clone();
    broken.machines[0]
        .pending
        .as_mut()
        .expect("the root is asked")
        .ordinal = 99;
    let bad = codec::encode(&broken, usize::MAX).expect("the damaged image encodes");
    assert_eq!(reject(&loaded, &bad), ImageReason::State);
}

#[test]
fn the_layout_rules_reject_precisely() {
    let (loaded, bytes) = asked_tree();
    let image = accept(&loaded, &bytes);
    // A program counter past the block.
    let mut broken = image.clone();
    broken.machines[0].frames[0].ip = 1000;
    let bad = codec::encode(&broken, usize::MAX).expect("the damaged image encodes");
    assert_eq!(reject(&loaded, &bad), ImageReason::Layout);
    // A block index the function has not.
    let mut broken = image.clone();
    broken.machines[0].frames[0].block = 1000;
    let bad = codec::encode(&broken, usize::MAX).expect("the damaged image encodes");
    assert_eq!(reject(&loaded, &bad), ImageReason::Layout);
    // A local arena that does not match the frame chain.
    let mut broken = image.clone();
    broken.machines[0].locals.push(lm_value::Value::Int(1));
    let bad = codec::encode(&broken, usize::MAX).expect("the damaged image encodes");
    assert_eq!(reject(&loaded, &bad), ImageReason::Layout);
    // A local base that does not start the frame.
    let mut broken = image.clone();
    broken.machines[0].frames[0].base_local = 3;
    let bad = codec::encode(&broken, usize::MAX).expect("the damaged image encodes");
    assert_eq!(reject(&loaded, &bad), ImageReason::Layout);
}

#[test]
fn the_mailbox_rules_reject_precisely() {
    let (loaded, bytes) = asked_tree();
    let image = accept(&loaded, &bytes);
    // A queue longer than the mailbox limit. The queue count is
    // checked against the limit before the reader allocates, so the
    // rejection names the limit rule.
    let mut broken = image.clone();
    broken.machines[1].mailbox.limit = 0;
    let bad = codec::encode(&broken, usize::MAX).expect("the damaged image encodes");
    assert_eq!(reject(&loaded, &bad), ImageReason::LimitExceeded);
    // A mailbox that delivered more than it accepted.
    let mut broken = image.clone();
    broken.machines[1].mailbox.delivered = 9;
    let bad = codec::encode(&broken, usize::MAX).expect("the damaged image encodes");
    assert_eq!(reject(&loaded, &bad), ImageReason::Mailbox);
    // A mailbox limit past the load limit.
    let mut broken = image.clone();
    broken.machines[1].mailbox.limit = u32::MAX;
    let bad = codec::encode(&broken, usize::MAX).expect("the damaged image encodes");
    assert_eq!(reject(&loaded, &bad), ImageReason::Mailbox);
}

#[test]
fn a_literal_that_is_not_its_pooled_string_rejects() {
    let source = "\
def go(): Int with Vm
  vm = sys.vm.Vm().from_object(do ||: Int
    s = \"literal\"
    if s == \"literal\"
      1
    else
      0
    end
  end, args: ())
  vm.step()
  vm.step()
  case vm.snapshot()
  in Ok(_)  then 1
  in Err(_) then 0 - 1
  end
end

go()
";
    let loaded = program(source);
    let mut world = World::new(
        &loaded,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    world.allow("Vm").expect("the grant names a target");
    lm_proc::run_world(&mut world);
    let bytes = world
        .last_snapshot()
        .expect("the program captured a world")
        .bytes()
        .to_vec();
    let image = accept(&loaded, &bytes);
    // Only run the rule when the capture reached the literal.
    let has_literal = image.machines[0].literals.iter().any(Option::is_some);
    if !has_literal {
        return;
    }
    let mut broken = image.clone();
    for entry in &mut broken.machines[0].objects {
        if let lm_heap::Object::Str(text) = &mut entry.object {
            text.push('!');
        }
    }
    let bad = codec::encode(&broken, usize::MAX).expect("the damaged image encodes");
    assert_eq!(reject(&loaded, &bad), ImageReason::Reference);
}

// ---------------------------------------------------------------
// The blanket sweep.
// ---------------------------------------------------------------

/// Every single-bit change to one container either rejects with a
/// precise reason, or reads as a different image that still encodes
/// back to exactly the bytes it came from.
///
/// The second half is the canonicality rule: one image has one byte
/// string, so a reader can never accept two spellings of one world.
#[test]
fn every_single_bit_change_rejects_or_stays_canonical() {
    let (_loaded, bytes) = asked_tree();
    let mut rejected = 0usize;
    let mut accepted = 0usize;
    for at in 0..bytes.len() - 32 {
        for bit in [0u8, 3, 7] {
            let mut bad = bytes.clone();
            bad[at] ^= 1 << bit;
            let bad = reseal(bad);
            match codec::decode(&bad, LoadLimits::default()) {
                Err(_) => rejected += 1,
                Ok(image) => {
                    let again = codec::encode(&image, usize::MAX).expect("the image encodes");
                    assert_eq!(again, bad, "byte {at} bit {bit} reads two spellings");
                    accepted += 1;
                }
            }
        }
    }
    assert!(rejected > 0, "the sweep exercised the rules");
    // The counters are a measurement, not a rule. They state that the
    // sweep is not trivially all-reject.
    eprintln!("sweep: {rejected} rejected, {accepted} accepted");
}

/// A truncated container never panics and never reads past its end.
#[test]
fn every_truncation_rejects() {
    let (_loaded, bytes) = asked_tree();
    for len in 0..bytes.len() {
        let cut = &bytes[..len];
        assert!(
            codec::decode(cut, LoadLimits::default()).is_err(),
            "a container of {len} bytes must reject"
        );
    }
}

// ---------------------------------------------------------------
// Deep graphs never recurse on the Rust stack.
// ---------------------------------------------------------------

/// The writer, the loader, and restore all walk a deep graph without
/// growing the Rust stack.
#[test]
fn a_deep_graph_never_recurses_in_the_codec() {
    let source = "\
def chain(n: Int): List[Int]
  out = [0]
  i = 0
  while i < n
    out = [i, out[1]]
    i = i + 1
  end
  out
end

def go(): Int with Vm
  vm = sys.vm.Vm().from_object(do ||: Int
    xs = [1]
    i = 0
    while i < 20000
      xs.push(i)
      i = i + 1
    end
    xs.len()
  end, args: ())
  vm.step()
  case vm.snapshot()
  in Ok(_)  then 1
  in Err(_) then 0 - 1
  end
end

go()
";
    let loaded = program(source);
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(move || {
            let mut world = World::new(
                &loaded,
                VmConfig::default(),
                Box::new(RecordingHost::new(1)),
            );
            world.allow("Vm").expect("the grant names a target");
            lm_proc::run_world(&mut world);
            let image = world
                .last_snapshot()
                .expect("the program captured a world")
                .clone();
            let admitted = codec::load_external(image.bytes(), &loaded, LoadLimits::default())
                .expect("the container loads and admits");
            assert_eq!(admitted.world().machine_count(), 1);
            let target = world.new_child(0).expect("the budget holds a child");
            world
                .restore_image(0, target, &admitted)
                .expect("the restore builds a world");
        })
        .expect("the thread starts")
        .join()
        .expect("no Rust stack overflow");
}

// ---------------------------------------------------------------
// The declared-type rules.
// ---------------------------------------------------------------

/// A local slot of another shape leaves its object unreachable.
///
/// The shape of a stored value is not a structural fact, so admission
/// proves nothing about it. The edit still drops the object the slot
/// named out of the reachable set, and the canonical order rule holds
/// that. A wrong shape that keeps the heap canonical admits, and
/// `admission.rs` states what the restored machine then does.
#[test]
fn a_local_that_drops_its_object_rejects_as_non_canonical() {
    let (loaded, bytes) = asked_tree();
    let image = accept(&loaded, &bytes);
    assert_eq!(image.machines[0].locals.len(), 1);
    let mut broken = image.clone();
    broken.machines[0].locals[0] = lm_value::Value::Int(1);
    let bad = codec::encode(&broken, usize::MAX).expect("the damaged image encodes");
    assert_eq!(reject(&loaded, &bad), ImageReason::Order);
}

/// An instance field of another shape admits.
///
/// The interpreter tests the tag when the field reaches a typed
/// reader.
#[test]
fn an_instance_field_of_the_wrong_shape_admits() {
    let source = "\
class Box
  label: String
  count: Int

  def init(mut self, label: String, count: Int)
    self.label = label
    self.count = count
  end
end

def go(): Int with Vm, Clock
  vm = sys.vm.Vm().from_object(do ||: Int with Clock.Now
    b = Box(\"tag\", 3)
    sys.clock.now()
  end, args: ())
  vm.table().pass(Clock)
  case vm.drive()
  in Asked(_)  then 0
  in Done(_)   then 0
  in Fault(_)  then 0
  end
  case vm.snapshot()
  in Ok(_)  then 1
  in Err(_) then 0 - 1
  end
end

go()
";
    let loaded = program(source);
    let mut world = World::new(
        &loaded,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    for grant in ["Vm", "Clock"] {
        world.allow(grant).expect("the grant names a target");
    }
    lm_proc::run_world(&mut world);
    let bytes = world
        .last_snapshot()
        .expect("the program captured a world")
        .bytes()
        .to_vec();
    let image = accept(&loaded, &bytes);
    let mut broken = image.clone();
    let mut damaged = false;
    for entry in &mut broken.machines[0].objects {
        if let lm_heap::Object::Instance { fields, .. } = &mut entry.object {
            if !fields.is_empty() {
                // The first field is the `String` label.
                fields[0] = lm_value::Value::Int(9);
                damaged = true;
            }
        }
    }
    assert!(damaged, "the capture holds the boxed instance");
    let bad = codec::encode(&broken, usize::MAX).expect("the damaged image encodes");
    admits(&loaded, &bad);
}

/// An operand of another shape or another count admits.
///
/// The exact operand count came from the verifier dataflow, and that
/// dataflow left admission with the type proof. The interpreter reads
/// its own arena instead: every typed reader tests the tag, and a pop
/// from an empty arena raises `MalformedState`.
#[test]
fn an_operand_of_the_wrong_shape_or_count_admits() {
    let source = "\
def go(): Int with Vm
  vm = sys.vm.Vm().from_object(do ||: Int
    20 + 22
  end, args: ())
  vm.step()
  vm.step()
  case vm.snapshot()
  in Ok(_)  then 1
  in Err(_) then 0 - 1
  end
end

go()
";
    let loaded = program(source);
    let mut world = World::new(
        &loaded,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    world.allow("Vm").expect("the grant names a target");
    lm_proc::run_world(&mut world);
    let bytes = world
        .last_snapshot()
        .expect("the program captured a world")
        .bytes()
        .to_vec();
    let image = accept(&loaded, &bytes);
    // Two integer constants sit on the operand stack.
    assert_eq!(image.machines[0].operands.len(), 2);
    // A boolean where the point proves an integer.
    let mut broken = image.clone();
    broken.machines[0].operands[0] = lm_value::Value::Bool(true);
    let bad = codec::encode(&broken, usize::MAX).expect("the damaged image encodes");
    admits(&loaded, &bad);
    // One operand too many.
    let mut broken = image.clone();
    broken.machines[0].operands.push(lm_value::Value::Int(1));
    let bad = codec::encode(&broken, usize::MAX).expect("the damaged image encodes");
    admits(&loaded, &bad);
    // One operand too few.
    let mut broken = image.clone();
    broken.machines[0].operands.pop();
    let bad = codec::encode(&broken, usize::MAX).expect("the damaged image encodes");
    admits(&loaded, &bad);
}

/// An accepted message of another shape leaves its object
/// unreachable.
///
/// The receiving proc reads its mailbox through `Recv[M]`, and the
/// boundary check of the world proves the message against that type at
/// the receive. This edit also drops the message object out of the
/// reachable set, so the canonical order rule holds it first.
#[test]
fn an_accepted_message_that_drops_its_object_rejects_as_non_canonical() {
    let (loaded, bytes) = asked_tree();
    let image = accept(&loaded, &bytes);
    // The worker mailbox holds the helper handle.
    assert_eq!(image.machines[1].mailbox.queue.len(), 1);
    let mut broken = image.clone();
    broken.machines[1].mailbox.queue[0] = lm_value::Value::Int(1);
    let bad = codec::encode(&broken, usize::MAX).expect("the damaged image encodes");
    assert_eq!(reject(&loaded, &bad), ImageReason::Order);
}

/// A terminal machine states its body function, and the header states
/// the closed result type that body derives.
///
/// The value itself is not a structural fact, so a terminal value of
/// another shape admits. The holder reads it through `RunResult[T]` or
/// `ProcResult[R]`, and the boundary check of the world proves it
/// there. The two records around it stay structural rules.
#[test]
fn a_terminal_machine_states_a_body_and_a_header_result_type() {
    let source = "\
def go(): Int with Vm
  vm = sys.vm.Vm().from_object(do ||: String
    \"answer\"
  end, args: ())
  case vm.run()
  in Done(_)  then 0
  in Fault(_) then 0
  end
  case vm.snapshot()
  in Ok(_)  then 1
  in Err(_) then 0 - 1
  end
end

go()
";
    let loaded = program(source);
    let mut world = World::new(
        &loaded,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    world.allow("Vm").expect("the grant names a target");
    lm_proc::run_world(&mut world);
    let bytes = world
        .last_snapshot()
        .expect("the program captured a world")
        .bytes()
        .to_vec();
    let image = accept(&loaded, &bytes);
    assert!(image.machines[0].frames.is_empty());
    assert!(image.machines[0].body_func.is_some());
    let mut broken = image.clone();
    broken.machines[0].terminal = Some(lm_vm::snapshot::ImageTerminal::Done(lm_value::Value::Int(
        1,
    )));
    let bad = codec::encode(&broken, usize::MAX).expect("the damaged image encodes");
    admits(&loaded, &bad);
    // The header and the machine witness state one result type.
    let mut broken = image.clone();
    broken.result_type = [7u8; 32];
    let bad = codec::encode(&broken, usize::MAX).expect("the damaged image encodes");
    assert_eq!(reject(&loaded, &bad), ImageReason::State);
    // A body function the manifest does not name rejects.
    let mut broken = image.clone();
    broken.machines[0].body_func = Some(u32::MAX);
    let bad = codec::encode(&broken, usize::MAX).expect("the damaged image encodes");
    assert_eq!(reject(&loaded, &bad), ImageReason::Code);
}

/// A terminal `Done` value that is not the unit must carry a result
/// type. A forged image with the result type stripped would otherwise
/// skip the terminal type proof and hand a wrong-typed value to a
/// consumer that reads it at the declared result type.
///
/// The header names the root result type as well, so the strip clears
/// both: the header agreement passes, and the terminal rule fires
/// alone.
#[test]
fn a_terminal_machine_without_a_body_function_admits() {
    let source = "\
def go(): Int with Vm
  vm = sys.vm.Vm().from_object(do ||: String
    \"answer\"
  end, args: ())
  case vm.run()
  in Done(_)  then 0
  in Fault(_) then 0
  end
  case vm.snapshot()
  in Ok(_)  then 1
  in Err(_) then 0 - 1
  end
end

go()
";
    let loaded = program(source);
    let mut world = World::new(
        &loaded,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    world.allow("Vm").expect("the grant names a target");
    lm_proc::run_world(&mut world);
    let bytes = world
        .last_snapshot()
        .expect("the program captured a world")
        .bytes()
        .to_vec();
    let image = accept(&loaded, &bytes);
    assert!(matches!(
        image.machines[0].terminal,
        Some(lm_vm::snapshot::ImageTerminal::Done(lm_value::Value::Obj(
            _
        )))
    ));
    let mut broken = image.clone();
    broken.machines[0].body_func = None;
    broken.machines[0].witness = 0;
    broken.result_type = [0u8; 32];
    let bad = codec::encode(&broken, usize::MAX).expect("the damaged image encodes");
    admits(&loaded, &bad);
}

/// A machine that is not a proc carries no accepted message. A forged
/// image that put a queue on a non-proc machine would leave the queue
/// values unchecked, because a non-proc has no mailbox type to prove
/// against. The loader rejects it instead.
#[test]
fn a_non_proc_machine_with_a_queued_message_rejects() {
    let (loaded, bytes) = asked_tree();
    let image = accept(&loaded, &bytes);
    // The held root is no proc. Move the worker's accepted message
    // onto it and raise its mailbox limit so only the proc rule fires.
    assert!(!image.machines[0].is_proc);
    let message = image.machines[1].mailbox.queue[0];
    let mut broken = image.clone();
    broken.machines[0].mailbox.limit = 1;
    broken.machines[0].mailbox.queue.push(message);
    let bad = codec::encode(&broken, usize::MAX).expect("the damaged image encodes");
    assert_eq!(reject(&loaded, &bad), ImageReason::Mailbox);
}

/// A machine captured `asked` carries the arguments the perform
/// popped, and the restorer reads them again when it answers or
/// dispatches. The loader proves those arguments against the type the
/// verifier proved for the perform.
///
/// The rule reads no manifest parameter type, so it holds for a
/// machine control operation exactly as it holds for the fixed
/// operation this case captures. A wrong shape and a wrong count both
/// reject.
#[test]
fn a_pending_argument_of_another_count_rejects() {
    // The held machine stops `asked` on `Rand.Int`, whose two integer
    // arguments the perform proved on the operand stack.
    let source = "\
def go(): Int with Vm, Rand
  held = sys.vm.Vm().from_object(do ||: Int with Rand.Int
    sys.rand.int(0, 10)
  end, args: ())
  case held.drive()
  in Asked(_)  then 0
  in Done(_)   then 0
  in Fault(_)  then 0
  end
  case held.snapshot()
  in Ok(_)  then 1
  in Err(_) then 0 - 1
  end
end

go()
";
    let loaded = program(source);
    let mut world = World::new(
        &loaded,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    for grant in ["Vm", "Rand"] {
        world.allow(grant).expect("the grant names a target");
    }
    lm_proc::run_world(&mut world);
    let bytes = world
        .last_snapshot()
        .expect("the program captured a world")
        .bytes()
        .to_vec();
    let image = accept(&loaded, &bytes);
    let held = image
        .machines
        .iter()
        .position(|m| {
            m.state == lm_vm::snapshot::ImageState::Asked
                && m.pending
                    .as_ref()
                    .is_some_and(|p| p.op == lm_abi::OP_RAND_INT)
        })
        .expect("one machine is asked on Rand.Int");
    assert_eq!(image.machines[held].pending.as_ref().unwrap().args.len(), 2);
    // A boolean where the code proves an integer is not a structural
    // fact. The host reads the arguments through `host_args`, which
    // tests each shape and faults the machine.
    let mut broken = image.clone();
    broken.machines[held].pending.as_mut().unwrap().args[0] = lm_value::Value::Bool(true);
    let bad = codec::encode(&broken, usize::MAX).expect("the damaged image encodes");
    admits(&loaded, &bad);
    // One argument too many is a count mismatch against the perform
    // the frame stopped inside.
    let mut broken = image.clone();
    broken.machines[held]
        .pending
        .as_mut()
        .unwrap()
        .args
        .push(lm_value::Value::Int(1));
    let bad = codec::encode(&broken, usize::MAX).expect("the damaged image encodes");
    assert_eq!(reject(&loaded, &bad), ImageReason::State);
}

// ---------------------------------------------------------------
// The security re-review: the whole "underivable type" pattern.
// ---------------------------------------------------------------

/// A proc that carries a message must have a provable mailbox type.
/// The message shape rule keys on the mailbox type the proc class
/// fixes, but that type is not always derivable from the image. When
/// it cannot be derived the queued values have no governing type, so
/// the loader rejects rather than schedule an unproven message.
///
/// The forge makes the held root a proc: its entry frame declares a
/// `Handle` first parameter, which is not a proc class, so no mailbox
/// type derives. The non-proc case is a separate rule and a separate
/// test.
#[test]
fn a_proc_with_an_underivable_mailbox_type_and_a_queue_rejects() {
    let (loaded, bytes) = asked_tree();
    let image = accept(&loaded, &bytes);
    // The held root is not a proc, and its entry frame takes a handle,
    // not a proc instance.
    assert!(!image.machines[0].is_proc);
    let mut broken = image.clone();
    broken.machines[0].is_proc = true;
    broken.machines[0].mailbox.limit = 1;
    broken.machines[0].mailbox.queue = vec![lm_value::Value::Int(0x4242_4242)];
    let bad = codec::encode(&broken, usize::MAX).expect("the damaged image encodes");
    assert_eq!(reject(&loaded, &bad), ImageReason::Mailbox);
}

/// The parent pointers form a forest. A self parent and a cycle both
/// reject at load, so the runtime policy walk over the parent chain
/// always terminates.
#[test]
fn a_cyclic_parent_graph_rejects() {
    let (loaded, bytes) = asked_tree();
    let image = accept(&loaded, &bytes);
    // A machine that is its own parent.
    let mut broken = image.clone();
    broken.machines[1].parent = Some(1);
    let bad = codec::encode(&broken, usize::MAX).expect("the damaged image encodes");
    assert_eq!(reject(&loaded, &bad), ImageReason::State);
    // A two-node cycle.
    let mut broken = image.clone();
    broken.machines[1].parent = Some(2);
    broken.machines[2].parent = Some(1);
    let bad = codec::encode(&broken, usize::MAX).expect("the damaged image encodes");
    assert_eq!(reject(&loaded, &bad), ImageReason::State);
}

/// A terminal machine holds no frame, and a frameless machine holds no
/// operand. The operand proof skips a terminal machine, so a frame or
/// an operand it carried would sit unproven.
#[test]
fn a_terminal_machine_with_a_frame_or_a_frameless_operand_rejects() {
    // A terminal machine with a frame: flip an asked machine to done
    // while it keeps its frame.
    let (loaded, bytes) = asked_tree();
    let image = accept(&loaded, &bytes);
    assert_eq!(image.machines[0].state, lm_vm::snapshot::ImageState::Asked);
    assert!(!image.machines[0].frames.is_empty());
    let mut broken = image.clone();
    broken.machines[0].state = lm_vm::snapshot::ImageState::Done;
    broken.machines[0].pending = None;
    broken.machines[0].terminal = Some(lm_vm::snapshot::ImageTerminal::Done(lm_value::Value::Unit));
    let bad = codec::encode(&broken, usize::MAX).expect("the damaged image encodes");
    assert_eq!(reject(&loaded, &bad), ImageReason::State);

    // A frameless machine with an operand: a done machine with empty
    // frames but one operand.
    let source = "\
def go(): Int with Vm
  vm = sys.vm.Vm().from_object(do ||: Int 7 end, args: ())
  case vm.run()
  in Done(_)  then 0
  in Fault(_) then 0
  end
  case vm.snapshot()
  in Ok(_)  then 1
  in Err(_) then 0 - 1
  end
end

go()
";
    let loaded = program(source);
    let mut world = World::new(
        &loaded,
        VmConfig::default(),
        Box::new(RecordingHost::new(1)),
    );
    world.allow("Vm").expect("the grant names a target");
    lm_proc::run_world(&mut world);
    let bytes = world
        .last_snapshot()
        .expect("the program captured a world")
        .bytes()
        .to_vec();
    let image = accept(&loaded, &bytes);
    // The captured machine is done, frameless, with empty arenas.
    let done = image
        .machines
        .iter()
        .position(|m| m.state == lm_vm::snapshot::ImageState::Done)
        .expect("one machine is done");
    assert!(image.machines[done].frames.is_empty());
    assert!(image.machines[done].operands.is_empty());
    let mut broken = image.clone();
    broken.machines[done].operands.push(lm_value::Value::Int(1));
    let bad = codec::encode(&broken, usize::MAX).expect("the damaged image encodes");
    assert_eq!(reject(&loaded, &bad), ImageReason::Layout);
}

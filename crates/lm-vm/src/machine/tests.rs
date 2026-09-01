//! Machine unit tests.

use super::*;

#[test]
fn a_map_rebuilds_its_private_index_from_semantic_hashes() {
    let mut machine = Machine::empty(VmConfig::default(), None);
    let map = machine
        .alloc(Object::Map {
            entries: vec![MapEntry {
                key: Value::Int(7),
                value: Value::Int(9),
                semantic_hash: 99,
            }]
            .into(),
            index: MapIndex::with_live(StructuralEpoch::default(), 1),
        })
        .expect("the map allocation succeeds");
    machine.push(map).expect("the map operand fits");
    machine.push(Value::Int(99)).expect("the hash operand fits");
    machine
        .push(Value::Int(0))
        .expect("the first probe marker fits");
    machine
        .exec_hashable_map_instr(ExtendedInstr::MapProbe)
        .expect("the probe succeeds");
    let token = machine.pop_int().expect("the probe returns a token");
    assert!(machine
        .map_token_entry(map.as_obj().expect("the map has one reference"), token)
        .expect("the token is valid")
        .is_some());
}

#[test]
fn policy_set_overlap_has_stable_precedence() {
    let mut table = PolicyTable::default();
    let client = lm_abi::group_by_name("Tcp.Client").unwrap();
    let stream = lm_abi::group_by_name("Tcp.Stream").unwrap();
    table.set_group(client, Some(Action::Pass));
    assert!(matches!(
        table.lookup(lm_abi::OP_TCP_READ),
        Some(Action::Pass)
    ));
    assert!(table.lookup(lm_abi::OP_TCP_LISTEN).is_none());

    table.set_group(stream, Some(Action::Block));
    assert!(matches!(
        table.lookup(lm_abi::OP_TCP_READ),
        Some(Action::Block)
    ));
    assert!(matches!(
        table.lookup(lm_abi::OP_TCP_CONNECT),
        Some(Action::Pass)
    ));

    table.set_exact(lm_abi::OP_TCP_READ, Some(Action::Pass));
    assert!(matches!(
        table.lookup(lm_abi::OP_TCP_READ),
        Some(Action::Pass)
    ));
}

/// The memory cost of one type environment witness.
///
/// A frame stores one index, and the closure and the instance
/// payloads store one each. `Object` is a Rust enum, so its size
/// is the size of its largest variant, and the witness fits the
/// existing padding of both payload variants.
#[test]
fn the_witness_costs_one_index_and_no_object_growth() {
    assert_eq!(std::mem::size_of::<Witness>(), 4);
    assert_eq!(std::mem::size_of::<Frame>(), 36);
    // The native fault record fixes the largest payload size.
    // The two witness fields fit without increasing that size.
    assert_eq!(std::mem::size_of::<Object>(), 64);
}

/// A fallible operand reader costs no register.
///
/// Every typed reader of the interpreter answers
/// `Result<_, FaultCode>` instead of asserting the tag. The value
/// tag holds a niche, so the fault code fits inside the value and
/// the return keeps the size it had.
#[test]
fn a_fallible_read_keeps_the_value_size() {
    assert_eq!(std::mem::size_of::<FaultCode>(), 1);
    assert_eq!(std::mem::size_of::<Value>(), 16);
    assert_eq!(std::mem::size_of::<Result<Value, FaultCode>>(), 16);
    assert_eq!(std::mem::size_of::<Result<ObjRef, FaultCode>>(), 12);
    assert_eq!(std::mem::size_of::<Result<bool, FaultCode>>(), 2);
    // An integer read pays one word, because `i64` has no niche.
    assert_eq!(std::mem::size_of::<Result<i64, FaultCode>>(), 16);
}

#[test]
fn an_integer_pair_replaces_two_operands_in_place() {
    let mut machine = Machine::empty(VmConfig::default(), None);
    assert_eq!(machine.int_binary(i64::checked_add), Err(BAD_STATE));

    machine.vm.operands = vec![Value::Int(7)];
    assert_eq!(machine.int_binary(i64::checked_add), Err(BAD_STATE));
    assert!(machine.vm.operands.is_empty());

    machine.vm.operands = vec![Value::Bool(false)];
    assert_eq!(machine.int_binary(i64::checked_add), Err(BAD_TYPE));
    assert!(machine.vm.operands.is_empty());

    machine.vm.operands = vec![Value::Int(7), Value::Int(5)];
    machine
        .int_binary(i64::checked_add)
        .expect("the addition succeeds");
    assert_eq!(machine.vm.operands, vec![Value::Int(12)]);

    machine.vm.operands = vec![Value::Bool(false), Value::Int(5)];
    assert_eq!(
        machine.int_binary(i64::checked_add),
        Err(FaultCode::TypeMismatch)
    );
    assert!(machine.vm.operands.is_empty());
}

#[test]
fn integer_text_lengths_cover_signed_bounds() {
    assert_eq!(integer_text_len(0), 1);
    assert_eq!(integer_text_len(9), 1);
    assert_eq!(integer_text_len(10), 2);
    assert_eq!(integer_text_len(-10), 3);
    assert_eq!(integer_text_len(i64::MIN), i64::MIN.to_string().len());
    assert_eq!(integer_text_len(i64::MAX), i64::MAX.to_string().len());
}

#[test]
fn request_ordinal_exhaustion_does_not_wrap() {
    let mut machine = Machine::empty(VmConfig::default(), None);
    machine.vm.next_ordinal = u64::MAX;
    assert_eq!(
        machine.take_request_ordinal(),
        Err(FaultCode::IntegerOverflow)
    );
    assert_eq!(machine.vm.next_ordinal, u64::MAX);
}

#[test]
fn mailbox_metrics_saturate() {
    let mut mailbox = Mailbox::new(1);
    mailbox.accepted = u64::MAX;
    mailbox.delivered = u64::MAX;
    mailbox.push(Value::Int(1));
    assert_eq!(mailbox.accepted, u64::MAX);
    assert_eq!(mailbox.pop(), Some(Value::Int(1)));
    assert_eq!(mailbox.delivered, u64::MAX);
}

#[test]
fn a_terminal_proc_keeps_only_its_dense_result_heap() {
    let mut machine = Machine::empty(VmConfig::default(), None);
    machine.is_proc = true;
    machine.vm.locals = Vec::with_capacity(1024);
    machine.vm.operands = Vec::with_capacity(1024);
    for _ in 0..1500 {
        machine
            .alloc(Object::Str("dead".into()))
            .expect("the dead object fits");
    }
    let result = machine
        .alloc(Object::Str("live".into()))
        .expect("the result fits");
    machine.set_done(result);
    let Some(Terminal::Done(Value::Obj(reference))) = machine.vm.terminal else {
        panic!("the proc stores its result");
    };
    assert_eq!(reference.slot, 0);
    assert_eq!(machine.vm.heap.slot_count(), 1);
    assert_eq!(machine.vm.locals.capacity(), 0);
    assert_eq!(machine.vm.operands.capacity(), 0);
    assert_eq!(machine.vm.heap.get(reference), &Object::Str("live".into()));
}

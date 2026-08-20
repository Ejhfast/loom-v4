//! Verifier coverage for the week-4 surfaces: perform row
//! reconstruction, operation slots, first-class operation types,
//! table edits, and the `Unreachable` terminator.

use lm_bytecode::{BcRow, BcType, Instr, Module};
use lm_testkit::compile_text;

fn compile(source: &str) -> Module {
    compile_text("t.lm", source).expect("the seed program compiles")
}

fn assert_rejected(module: &Module, needle: &str) {
    // The corrupted module must survive encode/decode and then fail
    // verification with a precise message.
    let bytes = lm_bytecode::encode(module);
    let decoded = lm_bytecode::decode(&bytes).expect("the corruption is structural");
    let err = lm_verify::verify_module(&decoded).expect_err("the verifier accepts a forgery");
    assert!(
        err.message.contains(needle),
        "expected `{needle}` in `{}`",
        err.message
    );
}

const GREET: &str =
    "def greet(name: String) with Io.Print\n  sys.io.print(name)\nend\ngreet(\"x\")\n";

/// Find the index of the one function whose name matches.
fn func_index(module: &Module, name: &str) -> usize {
    module
        .funcs
        .iter()
        .position(|f| f.name == name)
        .expect("the function exists")
}

#[test]
fn a_claimed_row_narrower_than_the_performs_is_rejected() {
    let mut module = compile(GREET);
    let greet = func_index(&module, "greet");
    module.funcs[greet].row.clear();
    assert_rejected(&module, "not inside the claimed row");
}

#[test]
fn a_row_name_outside_the_manifest_is_rejected() {
    let mut module = compile(GREET);
    let greet = func_index(&module, "greet");
    let BcRow::Op(idx) = module.funcs[greet].row[0] else {
        panic!("greet declares one exact operation");
    };
    module.strings[idx as usize] = "Zzz.Blast".to_string();
    assert_rejected(&module, "not in the operation manifest");
}

#[test]
fn a_perform_slot_outside_the_manifest_is_rejected() {
    let mut module = compile(GREET);
    let greet = func_index(&module, "greet");
    for block in &mut module.funcs[greet].blocks {
        for instr in block.iter_mut() {
            if let Instr::Perform { op, .. } = instr {
                *op = 9999;
            }
        }
    }
    assert_rejected(&module, "slot out of range");
}

#[test]
fn a_perform_argument_count_forgery_is_rejected() {
    let mut module = compile(GREET);
    let greet = func_index(&module, "greet");
    for block in &mut module.funcs[greet].blocks {
        for instr in block.iter_mut() {
            if let Instr::Perform { argc, .. } = instr {
                *argc = 0;
            }
        }
    }
    assert_rejected(&module, "argument count mismatch");
}

#[test]
fn a_perform_against_a_widened_target_row_is_rejected() {
    // Swap the exact operation of the perform: the claimed row still
    // names Io.Print, but the body performs Io.Error.
    let mut module = compile(GREET);
    let greet = func_index(&module, "greet");
    for block in &mut module.funcs[greet].blocks {
        for instr in block.iter_mut() {
            if let Instr::Perform { op, .. } = instr {
                *op = lm_abi::OP_IO_ERROR;
            }
        }
    }
    assert_rejected(&module, "not inside the claimed row");
}

const OP_VALUE: &str = "def f() with Io.Print\n  p = sys.io.print\n  p(\"x\")\nend\nf()\n";

#[test]
fn an_operation_type_with_a_forged_signature_is_rejected() {
    let mut module = compile(OP_VALUE);
    let forged = module
        .types
        .iter()
        .position(|t| matches!(t, BcType::Fn(params, _, _, _) if params.len() == 1))
        .expect("a one-parameter function type exists") as u32;
    let mut hit = false;
    for ty in &mut module.types {
        if let BcType::Op(_, f) = ty {
            // Point the embedded signature at an Int instead of the
            // manifest signature.
            *f = 2;
            hit = true;
        }
    }
    assert!(hit, "the seed holds a first-class operation type");
    let _ = forged;
    assert_rejected(&module, "wrong signature");
}

#[test]
fn an_op_const_of_a_vm_control_slot_is_rejected() {
    let mut module = compile(OP_VALUE);
    let f = func_index(&module, "f");
    for block in &mut module.funcs[f].blocks {
        for instr in block.iter_mut() {
            if let Instr::OpConst(op) = instr {
                *op = lm_abi::OP_VM_RUN;
            }
        }
    }
    assert_rejected(&module, "not fixed");
}

const MOCKER: &str = "def f(vm: Vm[Int]) with Vm\n  \
    vm.table().mock(Clock.Now, do ||: Int 1 end)\nend\n1\n";

#[test]
fn table_edit_forgeries_are_rejected() {
    // Invalid action encoding.
    let mut module = compile(MOCKER);
    let f = func_index(&module, "f");
    for block in &mut module.funcs[f].blocks {
        for instr in block.iter_mut() {
            if let Instr::TableEdit { action, .. } = instr {
                *action = 7;
            }
        }
    }
    assert_rejected(&module, "invalid table edit");
    // Target slot out of range.
    let mut module = compile(MOCKER);
    for block in &mut module.funcs[f].blocks {
        for instr in block.iter_mut() {
            if let Instr::TableEdit { slot, .. } = instr {
                *slot = 9999;
            }
        }
    }
    assert_rejected(&module, "target out of range");
    // A mock whose handler does not match the retargeted operation
    // signature.
    let mut module = compile(MOCKER);
    for block in &mut module.funcs[f].blocks {
        for instr in block.iter_mut() {
            if let Instr::TableEdit { action, slot, .. } = instr {
                if *action == 2 {
                    *slot = lm_abi::OP_CLOCK_SLEEP;
                }
            }
        }
    }
    assert_rejected(&module, "mock handler");
}

#[test]
fn as_call_of_a_vm_control_slot_is_rejected() {
    let source = "def f(vm: Vm[Int]): Int with Vm\n  case vm.drive()\n  in Asked(q)\n    \
        case q\n    in Call(Clock.Now, call, ()) then 1\n    in _ then 2\n    end\n  \
        in Done(_) then 3\n  in Fault(_) then 4\n  end\nend\n1\n";
    let mut module = compile(source);
    let f = func_index(&module, "f");
    for block in &mut module.funcs[f].blocks {
        for instr in block.iter_mut() {
            if let Instr::AsCall { op, .. } = instr {
                *op = lm_abi::OP_VM_ANSWER;
            }
        }
    }
    assert_rejected(&module, "not answerable");
}

#[test]
fn unreachable_must_terminate_its_block() {
    let mut module = compile("case 1\nin 1 then 2\nin _ then 3\nend\n");
    let entry = module.entry as usize;
    // Insert an Unreachable before a live instruction.
    let block = &mut module.funcs[entry].blocks[0];
    block.insert(0, Instr::Unreachable);
    assert_rejected(&module, "terminator before its end");
}

#[test]
fn compiled_cases_carry_the_unreachable_backstop() {
    let module = compile("case 1\nin 1 then 2\nin _ then 3\nend\n");
    let entry = module.entry as usize;
    let has_backstop = module.funcs[entry]
        .blocks
        .iter()
        .any(|b| b.last() == Some(&Instr::Unreachable));
    assert!(has_backstop, "the case lowered without a backstop");
}

#[test]
fn new_type_entries_check_their_references() {
    let mut module = compile("1\n");
    module.types.push(BcType::Vm(9999));
    assert_rejected(&module, "not an earlier entry");
    let mut module = compile("1\n");
    module.types.push(BcType::PendingCall(0, 9999));
    assert_rejected(&module, "not an earlier entry");
    let mut module = compile("1\n");
    module.types.push(BcType::Op(9999, 0));
    assert_rejected(&module, "invalid first-class operation slot");
}

#[test]
fn from_fn_argument_forgery_is_rejected() {
    // The argument view must match the program parameters.
    let source = "def go(): Int with Vm\n  \
        vm = sys.vm.Vm().from_fn(do |a: Int|: Int\n    a\n  end, args: (1,))\n  1\nend\ngo()\n";
    let mut module = compile(source);
    let go = func_index(&module, "go");
    // Retarget the tuple: replace the args-view TupleNew with a plain
    // unit constant, leaving the stack shape right but the type wrong.
    let mut hit = false;
    for block in &mut module.funcs[go].blocks {
        for instr in block.iter_mut() {
            if let Instr::TupleNew { .. } = instr {
                *instr = Instr::ConstUnit;
                hit = true;
            }
        }
    }
    assert!(hit, "the seed builds an argument tuple");
    assert_rejected(&module, "Vm.FromFn");
}

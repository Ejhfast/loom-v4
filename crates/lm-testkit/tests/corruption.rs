//! Corruption tests: hand-corrupted bytecode must be rejected before
//! execution, by the decoder or by the independent verifier. The
//! cases cover the week-2 table and instruction surfaces.

use lm_bytecode::{BcType, Instr};
use lm_testkit::compile_to_bytes;
use lm_vm::LoadError;

const SOURCE: &str = "def factorial(n: Int): Int
  if n <= 1
    1
  else
    n * factorial(n - 1)
  end
end

factorial(10)
";

/// A program that exercises classes, inheritance, virtual dispatch,
/// closures, collections, builders, and interpolation.
const OBJECT_SOURCE: &str = "class Animal
  name: String

  def init(mut self, name: String)
    self.name = name
  end

  def speak(self): String
    \"...\"
  end
end

class Dog < Animal
  def init(mut self, name: String)
    super.init(name)
  end

  def speak(self): String
    \"woof\"
  end
end

d: Animal = Dog(\"Fido\")
nm = d.name
words = [\"a\", \"b\"]
counts: {String: Int} = {\"a\": 1}
counts.put(words.at(1), 2)
f = do |x: Int|: Int x + 1 end
sb = StringBuilder()
sb.append(\"{d.speak()} {f(1)} {counts.len()}\")
sb.build()
";

fn valid_bytes() -> Vec<u8> {
    compile_to_bytes("corrupt.lm", SOURCE).unwrap()
}

fn object_bytes() -> Vec<u8> {
    compile_to_bytes("corrupt.lm", OBJECT_SOURCE).unwrap()
}

fn expect_verify_reject(bytes: &[u8], needle: &str) {
    match lm_vm::load_bytes(bytes) {
        Err(LoadError::Verify(e)) => {
            assert!(e.message.contains(needle), "wrong rejection: {e}");
        }
        other => panic!("expected a verifier rejection with `{needle}`, got {other:?}"),
    }
}

#[test]
fn valid_bytes_load_and_run() {
    let loaded = lm_vm::load_bytes(&valid_bytes()).unwrap();
    let mut vm = lm_vm::Vm::new(&loaded, lm_vm::VmConfig::default());
    let outcome = vm.run();
    assert_eq!(vm.show_outcome(&outcome), "Done(3628800)");
}

#[test]
fn valid_object_bytes_load_and_run() {
    let loaded = lm_vm::load_bytes(&object_bytes()).unwrap();
    let mut vm = lm_vm::Vm::new(&loaded, lm_vm::VmConfig::default());
    let outcome = vm.run();
    assert_eq!(vm.show_outcome(&outcome), "Done(\"woof 2 2\")");
}

#[test]
fn bad_jump_target_is_rejected_by_the_verifier() {
    let mut module = lm_bytecode::decode(&valid_bytes()).unwrap();
    // Redirect the first jump in the first function to a block that
    // does not exist.
    let mut patched = false;
    'outer: for func in &mut module.funcs {
        for block in &mut func.blocks {
            for instr in block.iter_mut() {
                if let Instr::Jump(target) = instr {
                    *target = 4_000_000;
                    patched = true;
                    break 'outer;
                }
            }
        }
    }
    assert!(patched, "the sample has at least one jump");
    expect_verify_reject(&lm_bytecode::encode(&module), "jump target");
}

#[test]
fn wrong_stack_shape_is_rejected_by_the_verifier() {
    let mut module = lm_bytecode::decode(&valid_bytes()).unwrap();
    // An extra pop at the start of the entry block underflows the
    // reconstructed stack.
    let entry = module.entry as usize;
    module.funcs[entry].blocks[0].insert(0, Instr::Pop);
    expect_verify_reject(&lm_bytecode::encode(&module), "empty stack");
}

#[test]
fn type_confusion_is_rejected_by_the_verifier() {
    let mut module = lm_bytecode::decode(&valid_bytes()).unwrap();
    // Replace an Int constant argument with a Bool constant.
    let mut patched = false;
    'outer: for func in &mut module.funcs {
        for block in &mut func.blocks {
            for instr in block.iter_mut() {
                if matches!(instr, Instr::ConstInt(_)) {
                    *instr = Instr::ConstBool(true);
                    patched = true;
                    break 'outer;
                }
            }
        }
    }
    assert!(patched);
    let bytes = lm_bytecode::encode(&module);
    assert!(matches!(
        lm_vm::load_bytes(&bytes),
        Err(LoadError::Verify(_))
    ));
}

#[test]
fn duplicate_type_entry_is_rejected() {
    let mut module = lm_bytecode::decode(&object_bytes()).unwrap();
    module.types.push(BcType::Int);
    expect_verify_reject(&lm_bytecode::encode(&module), "duplicates");
}

#[test]
fn forward_type_reference_is_rejected() {
    let mut module = lm_bytecode::decode(&object_bytes()).unwrap();
    let end = module.types.len() as u32;
    module.types.push(BcType::List(end + 5));
    expect_verify_reject(&lm_bytecode::encode(&module), "earlier entry");
}

#[test]
fn class_parent_cycle_is_rejected() {
    let mut module = lm_bytecode::decode(&object_bytes()).unwrap();
    // Point the Dog parent at itself.
    module.classes[1].parent = 1;
    expect_verify_reject(&lm_bytecode::encode(&module), "earlier class");
}

#[test]
fn broken_field_layout_prefix_is_rejected() {
    let mut module = lm_bytecode::decode(&object_bytes()).unwrap();
    module.classes[1].fields[0].1 = 0;
    expect_verify_reject(&lm_bytecode::encode(&module), "parent layout");
}

#[test]
fn field_type_out_of_range_is_rejected() {
    let mut module = lm_bytecode::decode(&object_bytes()).unwrap();
    let bad = module.types.len() as u32 + 3;
    module.classes[0].fields[0].1 = bad;
    // The child layout no longer extends the parent, or the type index
    // is invalid; either rejection is before execution.
    let bytes = lm_bytecode::encode(&module);
    assert!(matches!(
        lm_vm::load_bytes(&bytes),
        Err(LoadError::Verify(_))
    ));
}

#[test]
fn method_selector_out_of_range_is_rejected() {
    let mut module = lm_bytecode::decode(&object_bytes()).unwrap();
    module.classes[0].methods[0].0 = 999;
    expect_verify_reject(&lm_bytecode::encode(&module), "selector");
}

#[test]
fn method_function_out_of_range_is_rejected() {
    let mut module = lm_bytecode::decode(&object_bytes()).unwrap();
    module.classes[0].methods[0].1 = 999;
    expect_verify_reject(&lm_bytecode::encode(&module), "does not exist");
}

#[test]
fn call_virtual_selector_out_of_range_is_rejected() {
    let mut module = lm_bytecode::decode(&object_bytes()).unwrap();
    let mut patched = false;
    'outer: for func in &mut module.funcs {
        for block in &mut func.blocks {
            for instr in block.iter_mut() {
                if let Instr::CallVirtual { selector, .. } = instr {
                    *selector = 999;
                    patched = true;
                    break 'outer;
                }
            }
        }
    }
    assert!(patched, "the sample has a virtual call");
    expect_verify_reject(&lm_bytecode::encode(&module), "selector index");
}

#[test]
fn closure_capture_count_mismatch_is_rejected() {
    let mut module = lm_bytecode::decode(&object_bytes()).unwrap();
    let mut patched = false;
    'outer: for func in &mut module.funcs {
        for block in &mut func.blocks {
            for instr in block.iter_mut() {
                if let Instr::MakeClosure { captures, .. } = instr {
                    *captures += 1;
                    patched = true;
                    break 'outer;
                }
            }
        }
    }
    assert!(patched, "the sample creates a closure");
    expect_verify_reject(&lm_bytecode::encode(&module), "capture count");
}

#[test]
fn capture_index_out_of_range_is_rejected() {
    let mut module = lm_bytecode::decode(&object_bytes()).unwrap();
    // Insert a bad capture load into the entry function.
    let entry = module.entry as usize;
    module.funcs[entry].blocks[0].insert(0, Instr::LoadCapture(7));
    expect_verify_reject(&lm_bytecode::encode(&module), "capture index");
}

#[test]
fn new_with_bad_class_index_is_rejected() {
    let mut module = lm_bytecode::decode(&object_bytes()).unwrap();
    let mut patched = false;
    'outer: for func in &mut module.funcs {
        for block in &mut func.blocks {
            for instr in block.iter_mut() {
                if let Instr::New(class) = instr {
                    *class = 250;
                    patched = true;
                    break 'outer;
                }
            }
        }
    }
    assert!(patched, "the sample allocates an instance");
    expect_verify_reject(&lm_bytecode::encode(&module), "class index");
}

#[test]
fn load_field_out_of_range_is_rejected() {
    let mut module = lm_bytecode::decode(&object_bytes()).unwrap();
    let mut patched = false;
    'outer: for func in &mut module.funcs {
        for block in &mut func.blocks {
            for instr in block.iter_mut() {
                if let Instr::LoadField(field) = instr {
                    *field = 88;
                    patched = true;
                    break 'outer;
                }
            }
        }
    }
    assert!(patched, "the sample reads a field");
    expect_verify_reject(&lm_bytecode::encode(&module), "field index");
}

#[test]
fn entry_with_captures_is_rejected() {
    let mut module = lm_bytecode::decode(&object_bytes()).unwrap();
    let entry = module.entry as usize;
    module.funcs[entry].captures.push(2);
    expect_verify_reject(&lm_bytecode::encode(&module), "entry function");
}

#[test]
fn every_truncated_stream_is_rejected_by_the_decoder() {
    for bytes in [valid_bytes(), object_bytes()] {
        for len in 0..bytes.len() {
            match lm_vm::load_bytes(&bytes[..len]) {
                Err(LoadError::Decode(_)) => {}
                other => panic!("prefix length {len}: expected a decode error, got {other:?}"),
            }
        }
    }
}

#[test]
fn unknown_opcode_is_rejected_by_the_decoder() {
    let mut bytes = valid_bytes();
    // The last five bytes are the entry index and the final Return
    // opcode. Overwrite the Return opcode.
    let pos = bytes.len() - 5;
    assert_eq!(bytes[pos], 0x34, "the sample ends with a Return opcode");
    bytes[pos] = 0xfe;
    assert!(matches!(
        lm_vm::load_bytes(&bytes),
        Err(LoadError::Decode(_))
    ));
}

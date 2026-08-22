//! Corruption tests: hand-corrupted bytecode must be rejected before
//! execution, by the decoder or by the independent verifier. The
//! cases cover the week-2 table and instruction surfaces.

use lm_bytecode::{BcClassKind, BcRow, BcType, Instr};
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
f = { |x: Int|: Int x + 1 }
sb = StringBuilder()
sb.append(\"#{d.speak()} #{f(1)} #{counts.len()}\")
sb.build()
";

/// A program that exercises the week-3 surfaces: generics, type
/// applications, enums, case metadata, tuples, casts, and rows.
const WEEK3_SOURCE: &str = "enum Shape
  Circle(r: Int)
  Square(side: Int)

  def area10(self): Int
    case self
    in Circle(r) then 3 * r * r
    in Square(s) then s * s
    end
  end
end

class Box[T]
  value: T

  def init(mut self, value: T)
    self.value = value
  end

  def get(self): T
    self.value
  end
end

def loud() with Fs, Io.Print
end

def id[T](x: T): T
  x
end

s: Shape = Square(3)
t = (id(1), Box(\"a\").get(), s.area10())
t[2]
";

/// A program that digests a frozen graph and compares two integers.
const DIGEST_SOURCE: &str = "xs = [[1], [2]]\nxs.freeze()\nd = xs.digest()\n\
     n = xs.len()\nif n == 2\n  d\nelse\n  d\nend\n";

fn valid_bytes() -> Vec<u8> {
    compile_to_bytes("corrupt.lm", SOURCE).unwrap()
}

fn week3_bytes() -> Vec<u8> {
    compile_to_bytes("corrupt.lm", WEEK3_SOURCE).unwrap()
}

fn object_bytes() -> Vec<u8> {
    compile_to_bytes("corrupt.lm", OBJECT_SOURCE).unwrap()
}

/// The class index of one declared class. The core classes take
/// the first indices, so a module class index is not a constant.
fn class_index(module: &lm_bytecode::Module, name: &str) -> usize {
    module
        .classes
        .iter()
        .position(|c| c.name == name)
        .unwrap_or_else(|| panic!("the module declares `{name}`"))
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
    let dog = class_index(&module, "Dog");
    module.classes[dog].parent = dog as u32;
    expect_verify_reject(&lm_bytecode::encode(&module), "earlier class");
}

#[test]
fn broken_field_layout_prefix_is_rejected() {
    let mut module = lm_bytecode::decode(&object_bytes()).unwrap();
    let dog = class_index(&module, "Dog");
    module.classes[dog].fields[0].1 = 0;
    expect_verify_reject(&lm_bytecode::encode(&module), "parent layout");
}

#[test]
fn field_type_out_of_range_is_rejected() {
    let mut module = lm_bytecode::decode(&object_bytes()).unwrap();
    let bad = module.types.len() as u32 + 3;
    let animal = class_index(&module, "Animal");
    module.classes[animal].fields[0].1 = bad;
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
    let animal = class_index(&module, "Animal");
    module.classes[animal].methods[0].0 = 999;
    expect_verify_reject(&lm_bytecode::encode(&module), "selector");
}

#[test]
fn method_function_out_of_range_is_rejected() {
    let mut module = lm_bytecode::decode(&object_bytes()).unwrap();
    let animal = class_index(&module, "Animal");
    module.classes[animal].methods[0].1 = 999;
    expect_verify_reject(&lm_bytecode::encode(&module), "method function");
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
    expect_verify_reject(&lm_bytecode::encode(&module), "selector");
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
    expect_verify_reject(&lm_bytecode::encode(&module), "class");
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
    expect_verify_reject(
        &lm_bytecode::encode(&module),
        "function target has captures",
    );
}

#[test]
fn valid_week3_bytes_load_and_run() {
    let loaded = lm_vm::load_bytes(&week3_bytes()).unwrap();
    let mut vm = lm_vm::Vm::new(&loaded, lm_vm::VmConfig::default());
    let outcome = vm.run();
    assert_eq!(vm.show_outcome(&outcome), "Done(9)");
}

#[test]
fn app_arity_mismatch_is_rejected() {
    let mut module = lm_bytecode::decode(&week3_bytes()).unwrap();
    assert!(!module.apps.is_empty(), "the sample has applications");
    module.apps[0].types.push(0);
    expect_verify_reject(&lm_bytecode::encode(&module), "arity");
}

#[test]
fn app_with_invalid_type_index_is_rejected() {
    let mut module = lm_bytecode::decode(&week3_bytes()).unwrap();
    let bad = module.types.len() as u32 + 9;
    module.apps[0].types[0] = bad;
    expect_verify_reject(&lm_bytecode::encode(&module), "type index");
}

#[test]
fn non_canonical_declared_row_is_rejected() {
    let mut module = lm_bytecode::decode(&week3_bytes()).unwrap();
    let target = module
        .funcs
        .iter()
        .position(|f| f.row.len() == 2)
        .expect("`loud` declares a two-element row");
    module.funcs[target].row.reverse();
    expect_verify_reject(&lm_bytecode::encode(&module), "canonical");
}

#[test]
fn row_variable_outside_the_arity_is_rejected() {
    let mut module = lm_bytecode::decode(&week3_bytes()).unwrap();
    let target = module
        .funcs
        .iter()
        .position(|f| f.row.len() == 2)
        .expect("`loud` declares a row");
    module.funcs[target].row = vec![BcRow::Var(4)];
    expect_verify_reject(&lm_bytecode::encode(&module), "effect variable");
}

#[test]
fn widened_callee_row_is_rejected() {
    // Give the recursive target a claimed row. The entry function has
    // an empty row, so its call must fail row inclusion.
    let bytes = compile_to_bytes(
        "corrupt.lm",
        "def names_row() with Io.Print\n\
         end\n\
         def row_target(n: Int): Int\n\
         if n == 0\n\
           0\n\
         else\n\
           row_target(n - 1)\n\
         end\n\
         end\n\
         row_target(2)\n",
    )
    .unwrap();
    let mut module = lm_bytecode::decode(&bytes).unwrap();
    let target = module
        .funcs
        .iter()
        .position(|f| f.name == "row_target")
        .expect("the sample defines `row_target`");
    let op = module
        .strings
        .iter()
        .position(|s| s == "Io.Print")
        .expect("the row name is interned");
    module.funcs[target].row = vec![BcRow::Op(op as u32)];
    expect_verify_reject(&lm_bytecode::encode(&module), "slot contract");
}

#[test]
fn case_class_with_normal_parent_is_rejected() {
    let mut module = lm_bytecode::decode(&week3_bytes()).unwrap();
    let parent = module
        .classes
        .iter()
        .enumerate()
        .find(|(index, class)| {
            class.kind == BcClassKind::Abstract
                && module.classes.iter().any(|child| {
                    child.kind == BcClassKind::Case && child.parent() == Some(*index as u32)
                })
        })
        .map(|(index, _)| index)
        .expect("the sample has an enum parent");
    module.classes[parent].kind = BcClassKind::Normal;
    expect_verify_reject(&lm_bytecode::encode(&module), "case class");
}

#[test]
fn allocation_of_an_abstract_parent_is_rejected() {
    let mut module = lm_bytecode::decode(&week3_bytes()).unwrap();
    let parent = module
        .classes
        .iter()
        .position(|c| c.kind == BcClassKind::Abstract)
        .expect("the sample has an enum parent") as u32;
    let mut patched = false;
    'outer: for func in &mut module.funcs {
        for block in &mut func.blocks {
            for instr in block.iter_mut() {
                if let Instr::New(class) = instr {
                    *class = parent;
                    patched = true;
                    break 'outer;
                }
            }
        }
    }
    assert!(patched, "the sample allocates an instance");
    expect_verify_reject(&lm_bytecode::encode(&module), "abstract");
}

#[test]
fn tuple_get_out_of_range_is_rejected() {
    let mut module = lm_bytecode::decode(&week3_bytes()).unwrap();
    let mut patched = false;
    'outer: for func in &mut module.funcs {
        for block in &mut func.blocks {
            for instr in block.iter_mut() {
                if let Instr::TupleGet(index) = instr {
                    *index = 99;
                    patched = true;
                    break 'outer;
                }
            }
        }
    }
    assert!(patched, "the sample reads a tuple");
    expect_verify_reject(&lm_bytecode::encode(&module), "tuple index");
}

#[test]
fn tuple_new_count_mismatch_is_rejected() {
    let mut module = lm_bytecode::decode(&week3_bytes()).unwrap();
    let mut patched = false;
    'outer: for func in &mut module.funcs {
        for block in &mut func.blocks {
            for instr in block.iter_mut() {
                if let Instr::TupleNew { count, .. } = instr {
                    *count += 1;
                    patched = true;
                    break 'outer;
                }
            }
        }
    }
    assert!(patched, "the sample builds a tuple");
    let bytes = lm_bytecode::encode(&module);
    assert!(matches!(
        lm_vm::load_bytes(&bytes),
        Err(LoadError::Verify(_))
    ));
}

#[test]
fn cast_between_unrelated_classes_is_rejected() {
    let mut module = lm_bytecode::decode(&week3_bytes()).unwrap();
    // Retarget the first IsType/CastType at the Box class, which is
    // unrelated to the Shape family.
    let boxc = module
        .classes
        .iter()
        .position(|c| c.name == "Box")
        .expect("the sample defines Box") as u32;
    module.types.push(BcType::Var(9));
    let var_idx = module.types.len() as u32 - 1;
    module.types.push(BcType::Inst(boxc, vec![var_idx]));
    let unrelated = module.types.len() as u32 - 1;
    let mut patched = false;
    'outer: for func in &mut module.funcs {
        for block in &mut func.blocks {
            for instr in block.iter_mut() {
                match instr {
                    Instr::IsType(ty) | Instr::CastType(ty) => {
                        *ty = unrelated;
                        patched = true;
                        break 'outer;
                    }
                    _ => {}
                }
            }
        }
    }
    assert!(patched, "the sample tests a case class");
    let bytes = lm_bytecode::encode(&module);
    assert!(matches!(
        lm_vm::load_bytes(&bytes),
        Err(LoadError::Verify(_))
    ));
}

#[test]
fn class_arity_flip_is_rejected() {
    let mut module = lm_bytecode::decode(&week3_bytes()).unwrap();
    let boxc = module
        .classes
        .iter()
        .position(|c| c.name == "Box")
        .expect("the sample defines Box");
    module.classes[boxc].type_params = 2;
    let bytes = lm_bytecode::encode(&module);
    assert!(matches!(
        lm_vm::load_bytes(&bytes),
        Err(LoadError::Verify(_))
    ));
}

#[test]
fn every_truncated_stream_is_rejected_by_the_decoder() {
    for bytes in [valid_bytes(), object_bytes(), week3_bytes()] {
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
    // The semantic region ends with the entry index; the final
    // Return opcode sits directly before it.
    let sem_at = u32::from_le_bytes(bytes[38..42].try_into().unwrap()) as usize;
    let sem_len = u32::from_le_bytes(bytes[42..46].try_into().unwrap()) as usize;
    let pos = sem_at + sem_len - 5;
    assert_eq!(bytes[pos], 0x34, "the semantic region ends with Return");
    bytes[pos] = 0xfe;
    assert!(matches!(
        lm_vm::load_bytes(&bytes),
        Err(LoadError::Decode(_))
    ));
}

/// Source with a generic virtual call and an enum-arm cast, for the
/// review-fix attacks on `CallVirtualG` and `CastType`.
const REVIEW_SOURCE: &str = "
o: Option[Int] = Some(1)
p: Option[String] = Some(\"x\")
b = Box(2)
n = case o
in Some(v) then v + b.get()
in None    then 0
end
n

class Box[T]
  value: T

  def init(mut self, value: T)
    self.value = value
  end

  def get(self): T
    self.value
  end
end
";

fn review_bytes() -> Vec<u8> {
    compile_to_bytes("corrupt.lm", REVIEW_SOURCE).unwrap()
}

#[test]
fn valid_review_bytes_load_and_run() {
    let loaded = lm_vm::load_bytes(&review_bytes()).unwrap();
    let mut vm = lm_vm::Vm::new(&loaded, lm_vm::VmConfig::default());
    let outcome = vm.run();
    assert_eq!(vm.show_outcome(&outcome), "Done(3)");
}

#[test]
fn virtual_call_app_out_of_range_is_rejected_not_a_panic() {
    let mut module = lm_bytecode::decode(&review_bytes()).unwrap();
    let mut hit = false;
    for func in &mut module.funcs {
        for block in &mut func.blocks {
            for instr in block.iter_mut() {
                if let lm_bytecode::Instr::CallVirtualG { app, .. } = instr {
                    *app = 999_999;
                    hit = true;
                }
            }
        }
    }
    assert!(hit, "the sample contains a generic virtual call");
    expect_verify_reject(&lm_bytecode::encode(&module), "type application");
}

#[test]
fn cast_that_changes_generic_arguments_is_rejected() {
    let mut module = lm_bytecode::decode(&review_bytes()).unwrap();
    // Find one type-test instruction and the instance entry it names.
    let mut target: Option<(usize, Vec<u32>)> = None;
    'search: for func in &module.funcs {
        for block in &func.blocks {
            for instr in block {
                let ty = match instr {
                    lm_bytecode::Instr::CastType(ty) | lm_bytecode::Instr::IsType(ty) => *ty,
                    _ => continue,
                };
                if let lm_bytecode::BcType::Inst(class, args) = &module.types[ty as usize] {
                    if !args.is_empty() {
                        target = Some((*class as usize, args.clone()));
                        break 'search;
                    }
                }
            }
        }
    }
    let (class, args) = target.expect("the sample contains a generic type test");
    // Forge a sibling instantiation: the same class with one argument
    // replaced by a different existing type entry.
    let other = (0..module.types.len() as u32)
        .find(|c| {
            *c != args[0]
                && matches!(
                    module.types[*c as usize],
                    lm_bytecode::BcType::Int | lm_bytecode::BcType::Str | lm_bytecode::BcType::Bool
                )
        })
        .expect("a different scalar entry exists");
    let mut forged_args = args.clone();
    forged_args[0] = other;
    let forged = module.types.len() as u32;
    module
        .types
        .push(lm_bytecode::BcType::Inst(class as u32, forged_args));
    for func in &mut module.funcs {
        for block in &mut func.blocks {
            for instr in block.iter_mut() {
                match instr {
                    lm_bytecode::Instr::CastType(ty) | lm_bytecode::Instr::IsType(ty) => {
                        if matches!(&module.types[*ty as usize], lm_bytecode::BcType::Inst(c, a) if *c as usize == class && *a == args)
                        {
                            *ty = forged;
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    expect_verify_reject(
        &lm_bytecode::encode(&module),
        "changes the generic arguments",
    );
}

/// A digest program that a corrupted artifact turns into a scalar
/// digest rejects. The verifier proves the operand is a heap value,
/// so the VM never meets a value that carries no graph.
#[test]
fn digest_on_a_scalar_is_rejected_by_the_verifier() {
    let bytes = compile_to_bytes("corrupt.lm", DIGEST_SOURCE).unwrap();
    let mut module = lm_bytecode::decode(&bytes).unwrap();
    // Feed the digest instruction a constant instead of the graph.
    let mut patched = false;
    'outer: for func in &mut module.funcs {
        for block in &mut func.blocks {
            for i in 0..block.len() {
                if matches!(block[i], Instr::Digest { .. }) {
                    block[i - 1] = Instr::ConstInt(1);
                    block[i] = Instr::Digest { ty: 2 };
                    patched = true;
                    break 'outer;
                }
            }
        }
    }
    assert!(patched, "the sample carries one digest instruction");
    expect_verify_reject(&lm_bytecode::encode(&module), "digest on non-object type");
}

/// A corrupted artifact that compares two integers with the digest
/// comparison rejects. A digest compares by value, so the VM reads
/// the payload of both operands and needs the proof.
#[test]
fn digest_comparison_on_other_types_is_rejected_by_the_verifier() {
    for (name, forged) in [("EqDigest", Instr::EqDigest), ("NeDigest", Instr::NeDigest)] {
        let bytes = compile_to_bytes("corrupt.lm", DIGEST_SOURCE).unwrap();
        let mut module = lm_bytecode::decode(&bytes).unwrap();
        // Replace the integer comparison with the digest comparison.
        let mut patched = false;
        'outer: for func in &mut module.funcs {
            for block in &mut func.blocks {
                for instr in block.iter_mut() {
                    if matches!(instr, Instr::EqInt) {
                        *instr = forged;
                        patched = true;
                        break 'outer;
                    }
                }
            }
        }
        assert!(patched, "{name}: the sample compares two integers");
        expect_verify_reject(
            &lm_bytecode::encode(&module),
            "digest comparison on non-digest types",
        );
    }
}

// ---------------------------------------------------------------
// Week-8 formats: the parent type arguments and the proc rules.
// ---------------------------------------------------------------

/// The proc sample: a generic parent, a mailbox, and one spawn.
const PROC_SOURCE: &str = "class Worker < Proc[Int]
  def on_spawn(self): Int with Proc
    case self.receive()
    in Msg(n)
      n
    in Closed
      0
    end
  end
end

h = Worker.spawn()
h.send(1)
h.close()
h.done()
";

fn proc_bytes() -> Vec<u8> {
    compile_to_bytes("proc.lm", PROC_SOURCE).unwrap()
}

/// The class entry carries the parent type arguments. A forged count
/// no longer matches the parent arity.
#[test]
fn a_forged_parent_argument_count_is_rejected() {
    let mut module = lm_bytecode::decode(&proc_bytes()).unwrap();
    let worker = class_index(&module, "Worker");
    module.classes[worker].parent_args.clear();
    expect_verify_reject(&lm_bytecode::encode(&module), "parent type argument count");
}

/// A parent type argument out of range rejects before any use.
#[test]
fn a_parent_argument_out_of_range_is_rejected() {
    let mut module = lm_bytecode::decode(&proc_bytes()).unwrap();
    let worker = class_index(&module, "Worker");
    let bad = module.types.len() as u32 + 3;
    module.classes[worker].parent_args[0] = bad;
    let bytes = lm_bytecode::encode(&module);
    assert!(matches!(
        lm_vm::load_bytes(&bytes),
        Err(LoadError::Verify(_))
    ));
}

/// A parent type argument may not hold a type variable: the subclass
/// declares no type parameter to bind it.
#[test]
fn a_parent_argument_with_a_variable_is_rejected() {
    let mut module = lm_bytecode::decode(&proc_bytes()).unwrap();
    let worker = class_index(&module, "Worker");
    let var = module
        .types
        .iter()
        .position(|t| matches!(t, lm_bytecode::BcType::Var(0)))
        .expect("the module interns a type variable") as u32;
    module.classes[worker].parent_args[0] = var;
    expect_verify_reject(&lm_bytecode::encode(&module), "type variable");
}

/// A class without a parent carries no parent type argument.
#[test]
fn a_parent_argument_without_a_parent_is_rejected() {
    let mut module = lm_bytecode::decode(&proc_bytes()).unwrap();
    let proc = class_index(&module, "Proc");
    module.classes[proc].parent_args.push(0);
    expect_verify_reject(
        &lm_bytecode::encode(&module),
        "without a parent carries no parent type argument",
    );
}

/// The `Proc` core role must name the proc parent shape. A role that
/// names another class rejects.
#[test]
fn a_forged_proc_role_is_rejected() {
    let mut module = lm_bytecode::decode(&proc_bytes()).unwrap();
    let role = lm_bytecode::corepin::role_index("Proc").expect("the label is known");
    let worker = class_index(&module, "Worker") as u32;
    module.core_roles[role] = worker;
    expect_verify_reject(&lm_bytecode::encode(&module), "not the proc parent");
}

/// The mailbox type of a send comes from the handle on the stack. A
/// forged handle type no longer accepts the message.
#[test]
fn a_forged_handle_message_type_is_rejected() {
    let mut module = lm_bytecode::decode(&proc_bytes()).unwrap();
    let str_ty = module
        .types
        .iter()
        .position(|t| matches!(t, lm_bytecode::BcType::Str))
        .expect("the module interns String") as u32;
    let mut patched = false;
    for ty in module.types.iter_mut() {
        if let lm_bytecode::BcType::Handle(m, _) = ty {
            *m = str_ty;
            patched = true;
            break;
        }
    }
    assert!(patched, "the sample uses a proc handle");
    let bytes = lm_bytecode::encode(&module);
    assert!(matches!(
        lm_vm::load_bytes(&bytes),
        Err(LoadError::Verify(_))
    ));
}

/// A `Proc.Recv` whose receiver is not a proc instance rejects.
#[test]
fn a_receive_on_a_foreign_receiver_is_rejected() {
    let mut module = lm_bytecode::decode(&proc_bytes()).unwrap();
    let mut patched = false;
    'outer: for func in module.funcs.iter_mut() {
        for block in func.blocks.iter_mut() {
            for instr in block.iter_mut() {
                if let lm_bytecode::Instr::Perform { op, .. } = instr {
                    if *op == lm_abi::OP_PROC_RECV {
                        // Load an integer instead of `self`.
                        *instr = lm_bytecode::Instr::Perform {
                            op: lm_abi::OP_PROC_DONE,
                            argc: 1,
                            reply_ty: 0,
                        };
                        patched = true;
                        break 'outer;
                    }
                }
            }
        }
    }
    assert!(patched, "the sample performs Proc.Recv");
    let bytes = lm_bytecode::encode(&module);
    assert!(matches!(
        lm_vm::load_bytes(&bytes),
        Err(LoadError::Verify(_))
    ));
}

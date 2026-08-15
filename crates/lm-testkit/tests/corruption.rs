//! Corruption tests: hand-corrupted bytecode must be rejected before
//! execution, by the decoder or by the independent verifier.

use lm_bytecode::Instr;
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

fn valid_bytes() -> Vec<u8> {
    compile_to_bytes("corrupt.lm", SOURCE).unwrap()
}

#[test]
fn valid_bytes_load_and_run() {
    let loaded = lm_vm::load_bytes(&valid_bytes()).unwrap();
    let mut vm = lm_vm::Vm::new(&loaded, lm_vm::VmConfig::default());
    let outcome = vm.run();
    assert_eq!(vm.show_outcome(&outcome), "Done(3628800)");
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
    let bytes = lm_bytecode::encode(&module);
    match lm_vm::load_bytes(&bytes) {
        Err(LoadError::Verify(e)) => assert!(e.message.contains("jump target"), "{e}"),
        other => panic!("expected a verifier rejection, got {other:?}"),
    }
}

#[test]
fn wrong_stack_shape_is_rejected_by_the_verifier() {
    let mut module = lm_bytecode::decode(&valid_bytes()).unwrap();
    // An extra pop at the start of the entry block underflows the
    // reconstructed stack.
    let entry = module.entry as usize;
    module.funcs[entry].blocks[0].insert(0, Instr::Pop);
    let bytes = lm_bytecode::encode(&module);
    match lm_vm::load_bytes(&bytes) {
        Err(LoadError::Verify(e)) => assert!(e.message.contains("empty stack"), "{e}"),
        other => panic!("expected a verifier rejection, got {other:?}"),
    }
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
fn every_truncated_stream_is_rejected_by_the_decoder() {
    let bytes = valid_bytes();
    for len in 0..bytes.len() {
        match lm_vm::load_bytes(&bytes[..len]) {
            Err(LoadError::Decode(_)) => {}
            other => panic!("prefix length {len}: expected a decode error, got {other:?}"),
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

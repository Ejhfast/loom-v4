use lm_bytecode::{
    corepin::{ROLE_BYTES, ROLE_BYTE_BUFFER, ROLE_STRING_BUILDER},
    BcType, Instr,
};
use lm_testkit::{compile_text, run_text, run_world};
use lm_vm::{Vm, VmConfig};

fn core_method(module: &lm_bytecode::Module, role: usize, name: &str) -> (u32, u32) {
    let class = module.core_roles[role];
    assert_ne!(class, lm_bytecode::NO_ROLE);
    module.classes[class as usize]
        .methods
        .iter()
        .find(|(selector, _)| module.selectors[*selector as usize] == name)
        .copied()
        .expect("the core method exists")
}

#[test]
fn bytes_and_builders_cover_binary_data() {
    let source = r#"
bb = ByteBuffer()
bb.reserve(4).append(0).append(255).extend(Bytes("Hi"))
data = bb.build()
part = case data.slice(2, 2)
in Ok(value) then value.hex()
in Err(_) then "bad"
end
utf8 = case data.utf8()
in Ok(_) then "valid"
in Err(_) then "invalid"
end
sb = StringBuilder()
sb.append("discard")
text = sb.clear().append("ok").build()
lookup: {Bytes: Int} = {Bytes("key"): 7}
(
  data.len(),
  data.at(1),
  data.get(9),
  data.starts_with(Bytes()),
  data.find(Bytes("Hi")),
  data.hex(),
  part,
  (data + Bytes("!")).hex(),
  utf8,
  Bytes().is_empty(),
  Bytes("a") + Bytes("b") == Bytes("ab"),
  bb.len(),
  (
    bb.at(2),
    bb.at(9),
    bb.find_from(Bytes("Hi"), 1),
    bb.find_from(Bytes("Hi"), 3)
  ),
  text,
  lookup.at(Bytes("key"))
)
"#;
    assert_eq!(
        run_text("bytes_methods.lm", source, VmConfig::default()).unwrap(),
        "Done((4, 255, None, true, Some(2), \"00ff4869\", \"4869\", \
         \"00ff486921\", \"invalid\", true, true, 4, (Some(72), None, Some(2), None), \"ok\", 7))"
    );
}

#[test]
fn bytes_and_builder_intrinsics_use_native_instructions() {
    let source = r#"
bb = ByteBuffer()
bb.append(1).extend(Bytes("x")).reserve(2).clear().append(2)
bb.at(0)
bb.find_from(Bytes("x"), 0)
sb = StringBuilder()
sb.append("x").clear().append("y")
bytes = bb.build()
(
  bytes.at(0), bytes.get(1), bytes.concat(Bytes("z")),
  bytes.starts_with(Bytes()), bytes.find(Bytes("z")), bytes.hex(),
  bytes.utf8(), bytes == Bytes("x"), bytes != Bytes("y"),
  sb.len(), sb.build(), bb.len()
)
"#;
    let module = compile_text("bytes_instructions.lm", source).expect("the program compiles");
    for role in [ROLE_BYTES, ROLE_STRING_BUILDER, ROLE_BYTE_BUFFER] {
        let class = module.core_roles[role];
        assert!(module.classes[class as usize].is_final);
        assert!(module.classes[class as usize].fields.is_empty());
    }
    let instructions: Vec<Instr> = module
        .funcs
        .iter()
        .flat_map(|func| func.blocks.iter().flatten().copied())
        .collect();
    for expected in [
        Instr::Native(lm_bytecode::NativeInstr::BytesAt),
        Instr::Native(lm_bytecode::NativeInstr::BytesGet),
        Instr::Native(lm_bytecode::NativeInstr::BytesConcat),
        Instr::Native(lm_bytecode::NativeInstr::BytesStartsWith),
        Instr::Native(lm_bytecode::NativeInstr::BytesFindIndex),
        Instr::Native(lm_bytecode::NativeInstr::BytesHex),
        Instr::Native(lm_bytecode::NativeInstr::BytesIsUtf8),
        Instr::Native(lm_bytecode::NativeInstr::BytesText),
        Instr::Native(lm_bytecode::NativeInstr::EqBytes),
        Instr::Native(lm_bytecode::NativeInstr::NeBytes),
        Instr::Native(lm_bytecode::NativeInstr::SbLen),
        Instr::Native(lm_bytecode::NativeInstr::SbClear),
        Instr::Native(lm_bytecode::NativeInstr::BbExtend),
        Instr::Native(lm_bytecode::NativeInstr::BbReserve),
        Instr::Native(lm_bytecode::NativeInstr::BbClear),
        Instr::Native(lm_bytecode::NativeInstr::BbBuild),
        Instr::Native(lm_bytecode::NativeInstr::BbAt),
        Instr::Native(lm_bytecode::NativeInstr::BbFindFrom),
    ] {
        assert!(instructions.contains(&expected), "missing {expected:?}");
    }
    for role in [ROLE_STRING_BUILDER, ROLE_BYTE_BUFFER] {
        let class = module.core_roles[role];
        let ty = module
            .types
            .iter()
            .position(|ty| *ty == BcType::Class(class))
            .expect("the nominal builder type exists");
        assert!(ty >= 4);
    }
}

#[test]
fn bytes_views_ordering_and_builder_moves_have_value_semantics() {
    let source = r#"
sb = StringBuilder()
built = sb.append("é").build()
string_lengths = (sb.len(), sb.byte_len())
finished = sb.finish()
bb = ByteBuffer()
copy = bb.append(0).append(255).build()
moved = bb.finish()
bytes = Bytes("é")
(
  built,
  string_lengths,
  finished,
  copy.hex(),
  moved.hex(),
  bytes.compact() == bytes,
  bytes.utf8(),
  bytes.utf8_view(),
  Bytes("a") < Bytes("b"),
  Bytes("b") >= Bytes("b")
)
"#;
    assert_eq!(
        run_text("bytes_views.lm", source, VmConfig::default()).unwrap(),
        "Done((\"é\", (1, 2), \"é\", \"00ff\", \"00ff\", true, Ok(\"é\"), Ok(\"é\"), true, true))"
    );
}

#[test]
fn finished_builders_reject_later_use() {
    let string_source = r#"
builder = StringBuilder()
builder.append("done").finish()
builder.len()
"#;
    assert_eq!(
        run_text(
            "finished_string_builder.lm",
            string_source,
            VmConfig::default()
        )
        .unwrap(),
        "Fault(InvalidVmState)"
    );

    let byte_source = r#"
buffer = ByteBuffer()
buffer.append(1).finish()
buffer.len()
"#;
    assert_eq!(
        run_text("finished_byte_buffer.lm", byte_source, VmConfig::default()).unwrap(),
        "Fault(InvalidVmState)"
    );

    let scan_source = r#"
buffer = ByteBuffer()
buffer.append(1).finish()
buffer.find_from(Bytes("x"), 0)
"#;
    assert_eq!(
        run_text("finished_byte_scan.lm", scan_source, VmConfig::default()).unwrap(),
        "Fault(InvalidVmState)"
    );
}

#[test]
fn a_bytes_tag_supports_verified_virtual_dispatch() {
    let mut module =
        compile_text("bytes_virtual.lm", "Bytes(\"abc\").len()\n").expect("the program compiles");
    let (selector, _) = core_method(&module, ROLE_BYTES, "len");
    let literal = module
        .strings
        .iter()
        .position(|text| text == "abc")
        .expect("the literal exists") as u32;
    module.funcs[module.entry as usize].blocks = vec![vec![
        Instr::ConstStr(literal),
        Instr::Native(lm_bytecode::NativeInstr::BytesNew),
        Instr::CallVirtual { selector, argc: 0 },
        Instr::Return,
    ]];
    lm_verify::verify_module(&module).expect("the virtual call verifies");
    let loaded = lm_vm::load(module).expect("the module loads");
    let mut vm = Vm::new(&loaded, VmConfig::default());
    let outcome = vm.run();
    assert_eq!(vm.show_outcome(&outcome), "Done(3)");
}

#[test]
fn the_verifier_rejects_native_class_allocation() {
    let mut module = compile_text("bytes_new.lm", "Bytes()\n").expect("the program compiles");
    let class = module.core_roles[ROLE_BYTES];
    module.funcs[module.entry as usize].blocks = vec![vec![Instr::New(class), Instr::Return]];
    let error = lm_verify::verify_module(&module).expect_err("native allocation rejects");
    assert!(error
        .message
        .contains("New cannot allocate a native core class"));
}

#[test]
fn file_operations_preserve_arbitrary_bytes() {
    let source = r#"
def round_trip(): String with Fs.Open, Fs.Write, Fs.Seek, Fs.Read, Fs.Close
  case sys.fs.open("binary.dat", CreateTruncate)
  in Ok(file)
    buffer = ByteBuffer()
    buffer.append(0).append(255).append(65)
    case file.write(buffer.build())
    in Ok(_)
      case file.seek(Start(0))
      in Ok(_)
        answer = case file.read(3)
        in Ok(bytes) then bytes.hex()
        in Err(_) then "read error"
        end
        file.close()
        answer
      in Err(_) then "seek error"
      end
    in Err(_) then "write error"
    end
  in Err(_) then "open error"
  end
end

round_trip()
"#;
    let (outcome, host) =
        run_world("binary_fs.lm", source, &["Fs"], VmConfig::default()).expect("the program runs");
    assert_eq!(outcome, "Done(\"00ff41\")");
    assert_eq!(host.borrow().file("binary.dat"), Some(&[0, 255, 65][..]));
}

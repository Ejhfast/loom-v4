use lm_bytecode::{
    corepin::{ROLE_BYTES, ROLE_BYTE_BUFFER, ROLE_STRING_BUILDER},
    BcType, ExtendedInstr, Instr,
};
use lm_testkit::{compile_module_text, compile_verifier_fixture_text, run_text, run_world};
use lm_vm::{Vm, VmConfig};

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
    let core = lm_compiler::core_link_unit().expect("the core unit builds");
    let module = core.module();
    for role in [ROLE_BYTES, ROLE_STRING_BUILDER, ROLE_BYTE_BUFFER] {
        let class = module.core_roles[role];
        assert!(module.classes[class as usize].is_final);
        assert!(module.classes[class as usize].fields.is_empty());
    }
    let mut instructions: Vec<Instr> = module
        .funcs
        .iter()
        .flat_map(|func| func.blocks.iter().flatten().copied())
        .collect();
    let direct = compile_module_text("bytes_not_equal.lm", "Bytes(\"x\") != Bytes(\"y\")\n")
        .expect("the direct bytes comparison compiles");
    instructions.extend(
        direct
            .funcs
            .iter()
            .flat_map(|func| func.blocks.iter().flatten().copied()),
    );
    for expected in [
        Instr::Native(lm_bytecode::NativeInstr::BytesAt),
        Instr::Native(lm_bytecode::NativeInstr::BytesGet),
        Instr::Native(lm_bytecode::NativeInstr::BytesReadU32Be),
        Instr::Native(lm_bytecode::NativeInstr::BytesReadU32Le),
        Instr::Native(lm_bytecode::NativeInstr::BytesConcat),
        Instr::Native(lm_bytecode::NativeInstr::BytesStartsWith),
        Instr::Native(lm_bytecode::NativeInstr::BytesFindIndex),
        Instr::Native(lm_bytecode::NativeInstr::BytesHex),
        Instr::Native(lm_bytecode::NativeInstr::BytesIsUtf8),
        Instr::Native(lm_bytecode::NativeInstr::BytesText),
        Instr::Native(lm_bytecode::NativeInstr::BytesTextRange),
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
fn bytes_read_unsigned_words_in_both_byte_orders() {
    let source = r#"
bytes = b"\x01\x23\x45\x67\x89"
(
  bytes.read_u32_be(0),
  bytes.read_u32_le(0),
  bytes.read_u32_be(1),
  bytes.read_u32_le(1)
)
"#;
    assert_eq!(
        run_text("bytes_words.lm", source, VmConfig::default()).unwrap(),
        "Done((19088743, 1732584193, 591751049, 2305246499))"
    );
}

#[test]
fn bytes_get_unsigned_words_reports_invalid_ranges() {
    let source = r#"
bytes = b"\x01\x23\x45\x67\x89"
(
  bytes.get_u32_be(0),
  bytes.get_u32_le(1),
  bytes.get_u32_be(-1),
  bytes.get_u32_le(2),
  b"abc".get_u32_be(0)
)
"#;
    assert_eq!(
        run_text("bytes_get_words.lm", source, VmConfig::default()).unwrap(),
        "Done((Some(19088743), Some(2305246499), None, None, None))"
    );
}

#[test]
fn bytes_word_reads_check_the_complete_range() {
    for (name, source) in [
        ("bytes_word_negative.lm", "b\"abcd\".read_u32_be(-1)\n"),
        ("bytes_word_short.lm", "b\"abc\".read_u32_le(0)\n"),
        ("bytes_word_end.lm", "b\"abcd\".read_u32_be(1)\n"),
    ] {
        assert_eq!(
            run_text(name, source, VmConfig::default()).unwrap(),
            "Fault(IndexOutOfBounds)"
        );
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
  Bytes("xéz").text_range(1, 2),
  Bytes("a") < Bytes("b"),
  Bytes("b") >= Bytes("b")
)
"#;
    assert_eq!(
        run_text("bytes_views.lm", source, VmConfig::default()).unwrap(),
        "Done((\"é\", (1, 2), \"é\", \"00ff\", \"00ff\", true, Ok(\"é\"), Ok(\"é\"), \"é\", true, true))"
    );
}

#[test]
fn bytes_text_range_checks_bounds_and_encoding() {
    assert_eq!(
        run_text(
            "bytes_text_range_bounds.lm",
            "Bytes(\"abc\").text_range(2, 2)\n",
            VmConfig::default(),
        )
        .unwrap(),
        "Fault(IndexOutOfBounds)"
    );
    assert_eq!(
        run_text(
            "bytes_text_range_utf8.lm",
            "b\"\\xff\".text_range(0, 1)\n",
            VmConfig::default(),
        )
        .unwrap(),
        "Fault(BadCast)"
    );
}

#[test]
fn bytes_text_ranges_intern_owned_strings_only_on_misses() {
    let source = r#"
bytes = Bytes("_key_key_new_")
pool = Map[String, String]()
first = bytes.intern_text_range(pool, 1, 3)
second = bytes.intern_text_range(pool, 5, 3)
third = bytes.intern_text_range(pool, 9, 3)
(first, second, third, pool.len(), pool.keys_list())
"#;
    assert_eq!(
        run_text("bytes_range_intern.lm", source, VmConfig::default()).unwrap(),
        "Done((\"key\", \"key\", \"new\", 2, [\"key\", \"new\"]))"
    );
}

#[test]
fn borrowed_string_key_forms_share_one_map_relation() {
    let source = r#"
bytes = Bytes("_key_")
text = "_key_"
view = text.slice(1, 3).expect("the view exists")

left = Map[String, String]()
left.put(view, "value")
left_hit = bytes.intern_text_range(left, 1, 3)

right = Map[String, String]()
right_first = bytes.intern_text_range(right, 1, 3)
right_previous = right.put(view, "other")

(left_hit, left.len(), left.at("key"), right_first, right_previous, right.keys_list())
"#;
    assert_eq!(
        run_text("borrowed_string_key_forms.lm", source, VmConfig::default()).unwrap(),
        "Done((\"key\", 1, \"value\", \"key\", Some(\"key\"), [\"key\"]))"
    );
}

#[test]
fn bytes_text_range_interning_checks_bounds_and_encoding() {
    assert_eq!(
        run_text(
            "bytes_range_intern_bounds.lm",
            "pool = Map[String, String]()\nBytes(\"abc\").intern_text_range(pool, 2, 2)\n",
            VmConfig::default(),
        )
        .unwrap(),
        "Fault(IndexOutOfBounds)"
    );
    assert_eq!(
        run_text(
            "bytes_range_intern_utf8.lm",
            "pool = Map[String, String]()\nb\"\\xff\".intern_text_range(pool, 0, 1)\n",
            VmConfig::default(),
        )
        .unwrap(),
        "Fault(BadCast)"
    );
}

#[test]
fn the_verifier_checks_text_range_interning_operands() {
    let mut module = compile_verifier_fixture_text(
        "bytes_range_intern_verify.lm",
        "pool = Map[String, String]()\nBytes(\"abc\").intern_text_range(pool, 0, 2)\n",
    )
    .expect("the valid range interning compiles");
    let (function, block, instruction) = module
        .funcs
        .iter()
        .enumerate()
        .find_map(|(function, func)| {
            func.blocks
                .iter()
                .enumerate()
                .find_map(|(block, instructions)| {
                    instructions
                        .iter()
                        .position(|instruction| {
                            matches!(
                                instruction,
                                Instr::Extended(ExtendedInstr::MapInternTextRange)
                            )
                        })
                        .map(|instruction| (function, block, instruction))
                })
        })
        .expect("the range interning lowers");
    module.funcs[function].blocks[block][instruction - 1] = Instr::ConstBool(false);
    let error = lm_verify::verify_module(&module).expect_err("the wrong operand type rejects");
    assert!(error.to_string().contains("expected type"), "{error}");
}

#[test]
fn the_verifier_checks_bytes_text_range_operands() {
    let mut module = compile_module_text(
        "bytes_text_range_verify.lm",
        "Bytes(\"abc\").text_range(0, 2)\n",
    )
    .expect("the valid range conversion compiles");
    let entry = module.entry as usize;
    let block = &mut module.funcs[entry].blocks[0];
    let length = block
        .iter()
        .position(|instruction| {
            matches!(
                instruction,
                Instr::Native(lm_bytecode::NativeInstr::BytesTextRange)
            )
        })
        .expect("the range conversion lowers")
        - 1;
    block[length] = Instr::ConstBool(false);
    let error = lm_verify::verify_module(&module).expect_err("the wrong operand type rejects");
    assert!(error.to_string().contains("expected type"), "{error}");
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
    let mut module = lm_compiler::core_link_unit()
        .expect("the core unit builds")
        .module()
        .clone();
    let selector = module
        .selectors
        .iter()
        .position(|name| name == "len")
        .expect("the core selector exists") as u32;
    let literal = module.strings.len() as u32;
    module.strings.push("abc".to_string());
    let int_type = module
        .types
        .iter()
        .position(|ty| *ty == BcType::Int)
        .expect("the core Int type exists") as u32;
    let entry = module.funcs.len() as u32;
    module.funcs.push(lm_bytecode::Func {
        name: "<entry>".to_string(),
        type_params: 0,
        effect_params: 0,
        params: vec![],
        param_muts: vec![],
        ret: int_type,
        row: vec![],
        captures: vec![],
        local_types: vec![],
        blocks: vec![vec![
            Instr::ConstStr(literal),
            Instr::Native(lm_bytecode::NativeInstr::BytesNew),
            Instr::CallVirtual { selector, argc: 0 },
            Instr::Return,
        ]],
        param_names: vec![],
    });
    module.func_bounds.push(vec![]);
    module.entry = entry;
    lm_verify::verify_module(&module).expect("the virtual call verifies");
    let (arena, namespace) = lm_testkit::unit_from_module(module).expect("the unit publishes");
    let mut vm = Vm::new(arena, namespace, VmConfig::default());
    let outcome = vm.run();
    assert_eq!(vm.show_outcome(&outcome), "Done(3)");
}

#[test]
fn the_verifier_rejects_native_class_allocation() {
    let mut module =
        compile_module_text("bytes_new.lm", "Bytes()\n").expect("the program compiles");
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
  case sys.fs.open(Path("binary.dat", PathStyle.Posix), CreateTruncate)
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

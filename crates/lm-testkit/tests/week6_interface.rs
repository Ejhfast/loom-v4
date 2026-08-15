//! The interface artifact: the structural signatures, the two
//! hashes, the readable dump, and the corruption gates.

use lm_bytecode::interface::{
    decode_interface, dump_interface, encode_interface, ExportKind, IfaceItem, IfaceType, Interface,
};
use lm_compiler::{compile_module, CompileEnv};
use lm_source::SourceFile;

const SAMPLE: &str = "class Point\n\
                      \x20 x: Int = 0\n\
                      \x20 y: Int = 0\n\
                      \n\
                      \x20 def sum(self): Int\n\
                      \x20   self.x + self.y\n\
                      \x20 end\n\
                      end\n\
                      \n\
                      enum Shape\n\
                      \x20 Dot\n\
                      \x20 Line(len: Int)\n\
                      end\n\
                      \n\
                      def area(s: Shape): Int with Io.Print\n\
                      \x20 sys.io.print(\"x\")\n\
                      \x20 case s\n\
                      \x20 in Dot then 0\n\
                      \x20 in Line(l) then l\n\
                      \x20 end\n\
                      end\n";

fn compile(path: &str, text: &str) -> lm_compiler::CompiledModule {
    let source = SourceFile::new(path, text.to_string());
    compile_module(path, &source, &CompileEnv::new().freeze(), false).expect("compiles")
}

fn sample() -> Interface {
    compile("shapes", SAMPLE).interface
}

#[test]
fn the_interface_round_trips_and_dumps() {
    let interface = sample();
    let bytes = encode_interface(&interface);
    let back = decode_interface(&bytes).expect("decodes");
    assert_eq!(back, interface);
    assert_eq!(encode_interface(&back), bytes);
    // Every top-level definition is exported, arms included.
    let names: Vec<&str> = interface.exports.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["Point", "Shape", "Shape.Dot", "Shape.Line", "area"]
    );
    let area = interface.find("area").expect("area is exported");
    assert_eq!(area.kind, ExportKind::Function);
    let IfaceItem::Func(sig) = &area.item else {
        panic!("area is a function");
    };
    // The signature is structural: the parameter names the exporting
    // module and the class, and the row names the operation.
    match &sig.params[0] {
        IfaceType::Named { class, .. } => {
            assert_eq!(class.module, "shapes");
            assert_eq!(class.name, "Shape");
        }
        other => panic!("the parameter is not a class: {other:?}"),
    }
    assert_eq!(sig.row.len(), 1);
    let dump = dump_interface(&interface);
    assert!(dump.contains("interface shapes"), "{dump}");
    assert!(dump.contains("fn area"), "{dump}");
    assert!(dump.contains("with Io.Print"), "{dump}");
    assert!(dump.contains("def sum(self)"), "{dump}");
}

/// The interface hash covers the surface and no body. The definition
/// hash covers the implementation.
#[test]
fn a_body_edit_moves_the_definition_hash_and_no_interface_hash() {
    let before = sample();
    let after = compile(
        "shapes",
        &SAMPLE.replace("self.x + self.y", "self.x * self.y"),
    )
    .interface;
    let point_before = before.find("Point").unwrap();
    let point_after = after.find("Point").unwrap();
    assert_eq!(
        point_before.iface_hash, point_after.iface_hash,
        "a method body edit moved the interface hash"
    );
    assert_ne!(
        point_before.def_hash, point_after.def_hash,
        "a method body edit kept the definition hash"
    );
    assert_ne!(before.semantic_hash, after.semantic_hash);
}

/// A signature edit moves both hashes.
#[test]
fn a_signature_edit_moves_the_interface_hash() {
    let before = sample();
    let after = compile(
        "shapes",
        &SAMPLE.replace("def sum(self): Int", "def sum(self, k: Int): Int"),
    )
    .interface;
    let point_before = before.find("Point").unwrap();
    let point_after = after.find("Point").unwrap();
    assert_ne!(point_before.iface_hash, point_after.iface_hash);
    assert_ne!(point_before.def_hash, point_after.def_hash);
}

/// The module path is part of the published surface, because a
/// signature names a class by qualified name.
#[test]
fn the_module_path_is_part_of_the_interface() {
    let here = compile("shapes", SAMPLE).interface;
    let there = compile("other", SAMPLE).interface;
    assert_ne!(
        here.find("area").unwrap().iface_hash,
        there.find("area").unwrap().iface_hash
    );
}

/// Building one interface twice is byte-identical.
#[test]
fn the_interface_bytes_are_deterministic() {
    let a = encode_interface(&sample());
    let b = encode_interface(&sample());
    assert_eq!(a, b);
}

/// Every truncation, every trailing byte, and every bad tag rejects.
#[test]
fn every_interface_corruption_is_rejected() {
    let bytes = encode_interface(&sample());
    for len in 0..bytes.len() {
        assert!(
            decode_interface(&bytes[..len]).is_err(),
            "interface prefix {len} was accepted"
        );
    }
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(decode_interface(&trailing).is_err());
    // The export kind byte follows the header: magic, version, two
    // ABI versions, the module path, the semantic hash, and the count.
    let kind_at = 4 + 2 + 4 + 4 + (4 + "shapes".len()) + 32 + 4;
    assert_eq!(bytes[kind_at], ExportKind::Class.tag());
    let mut bad_kind = bytes.clone();
    bad_kind[kind_at] = 9;
    assert!(decode_interface(&bad_kind).is_err());
    // A kind that disagrees with the item rejects: a class entry with
    // a function item.
    let mut swapped = bytes;
    swapped[kind_at] = ExportKind::Function.tag();
    assert!(decode_interface(&swapped).is_err());
}

/// A crafted deep type rejects instead of growing the host stack.
#[test]
fn a_deeply_nested_interface_type_is_rejected() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"LMIF");
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&lm_abi::ABI_VERSION.to_le_bytes());
    bytes.extend_from_slice(&lm_bytecode::identity::COMPILER_ABI_VERSION.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes()); // the module path
    bytes.extend_from_slice(&[0u8; 32]); // the semantic hash
    bytes.extend_from_slice(&1u32.to_le_bytes()); // one export
    bytes.push(ExportKind::Function.tag());
    bytes.extend_from_slice(&0u32.to_le_bytes()); // the name
    bytes.extend_from_slice(&[0u8; 32]); // the interface hash
    bytes.extend_from_slice(&[0u8; 32]); // the definition hash
    bytes.push(0); // a function item
    bytes.extend_from_slice(&0u32.to_le_bytes()); // type parameters
    bytes.extend_from_slice(&0u32.to_le_bytes()); // effect parameters
    bytes.extend_from_slice(&0u32.to_le_bytes()); // parameters
                                                  // The result type is a list nested past the depth cap.
    bytes.resize(bytes.len() + 2000, 12);
    bytes.push(2);
    bytes.extend_from_slice(&0u32.to_le_bytes());
    assert!(
        decode_interface(&bytes).is_err(),
        "a deep interface type was accepted"
    );
}

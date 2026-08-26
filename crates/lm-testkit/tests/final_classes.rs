use lm_bytecode::interface::IfaceItem;
use lm_compiler::{compile_module, CompileEnv};
use lm_source::SourceFile;
use lm_testkit::{compile_text, run_text};
use lm_vm::VmConfig;

fn library(path: &str, source: &str) -> lm_compiler::CompiledModule {
    compile_module(
        path,
        &SourceFile::new(format!("{path}.lm"), source),
        &CompileEnv::new().freeze(),
        false,
    )
    .expect("the library compiles")
}

#[test]
fn a_final_class_compiles_and_runs() {
    let source = "final class Token\n  value: Int = 42\nend\nToken().value\n";
    let module = compile_text("final.lm", source).expect("the program compiles");
    let class = module
        .classes
        .iter()
        .find(|class| class.name == "Token")
        .expect("Token exists");
    assert!(class.is_final);
    let decoded = lm_bytecode::decode(&lm_bytecode::encode(&module)).unwrap();
    assert!(decoded.classes.iter().any(|class| class.is_final));
    let result = run_text("final.lm", source, VmConfig::default()).unwrap();
    assert_eq!(result, "Done(42)");
}

#[test]
fn a_final_class_rejects_a_subclass() {
    let source = "final class Base\nend\nclass Child < Base\nend\n1\n";
    let error = compile_text("final.lm", source).expect_err("the subclass rejects");
    assert!(error.contains("E1040"), "{error}");
    assert!(error.contains("final and cannot be a parent"), "{error}");
}

#[test]
fn final_changes_both_class_contract_hashes() {
    let plain = library("lib.base", "class Base\nend\n");
    let final_class = library("lib.base", "final class Base\nend\n");
    let class_hash = |unit: &lm_compiler::CompiledModule| {
        let index = unit
            .module
            .classes
            .iter()
            .position(|class| class.name == "Base")
            .unwrap();
        lm_bytecode::identity::module_identity(&unit.module)
            .unwrap()
            .class_hashes[index]
    };
    assert_ne!(class_hash(&plain), class_hash(&final_class));
    let plain_export = plain.interface.find("Base").unwrap();
    let final_export = final_class.interface.find("Base").unwrap();
    assert_ne!(plain_export.iface_hash, final_export.iface_hash);
    let IfaceItem::Class(surface) = &final_export.item else {
        panic!("Base is a class");
    };
    assert!(surface.is_final);
}

#[test]
fn a_dependent_checker_reads_final_from_the_interface() {
    let base = library("lib.base", "final class Base\nend\n");
    let mut env = CompileEnv::new();
    env.bind_interface(base.interface).unwrap();
    env.bind_root("lib", "lib").unwrap();
    let source = SourceFile::new(
        "app/main.lm",
        "use lib.base.Base\nclass Child < Base\nend\n1\n",
    );
    let error =
        compile_module("app.main", &source, &env.freeze(), true).expect_err("the subclass rejects");
    assert!(error.contains("final and cannot be a parent"), "{error}");
}

#[test]
fn the_verifier_rejects_a_final_parent() {
    let source = "class Base\n  def value(self): Int\n    1\n  end\nend\n\
                  class Child < Base\nend\nChild().value()\n";
    let mut module = compile_text("final.lm", source).expect("the program compiles");
    let base = module
        .classes
        .iter()
        .position(|class| class.name == "Base")
        .unwrap();
    module.classes[base].is_final = true;
    let error = lm_verify::verify_module(&module).expect_err("the module rejects");
    assert!(error.message.contains("inherit a final class"), "{error}");
}

#[test]
fn a_final_generic_method_resolves_directly() {
    let source = "final class Box[T]\n  value: T\n  def init(mut self, value: T)\n    \
                  self.value = value\n  end\n  def get(self): T\n    self.value\n  end\nend\n\
                  Box[Int](42).get()\n";
    let module = compile_text("final_generic.lm", source).expect("the program compiles");
    let target = module
        .funcs
        .iter()
        .position(|func| func.name == "Box.get")
        .expect("Box.get exists") as u32;
    let entry = &module.funcs[module.entry as usize];
    assert!(entry.blocks.iter().flatten().all(|instr| {
        !matches!(instr, lm_bytecode::Instr::CallVirtual { .. })
            && !matches!(instr, lm_bytecode::Instr::CallVirtualG { .. })
    }));
    assert!(entry
        .blocks
        .iter()
        .flatten()
        .any(|instr| matches!(instr, lm_bytecode::Instr::CallG { func, .. } if *func == target)));
    assert_eq!(
        run_text("final_generic.lm", source, VmConfig::default()).unwrap(),
        "Done(42)"
    );
}

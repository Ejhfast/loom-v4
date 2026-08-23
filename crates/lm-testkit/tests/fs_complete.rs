//! Complete file-system effects and portable boundary values.

use lm_bytecode::corepin::ROLE_FILE_INFO;
use lm_testkit::{compile_text, compile_to_bytes, repo_root};
use lm_vm::{load_bytes, RecordingHost, VmConfig, World};
use std::cell::RefCell;
use std::rc::Rc;

const DURABLE_FLOW: &str = r#"
use std.fs.durable_replace
use std.fs.read_dir_sorted

def go(): Result[(Int, Bool), FsError] with Fs.CreateDir, Fs.Open, Fs.Write, Fs.Flush, Fs.Sync, Fs.Close, Fs.RemoveFile, Fs.RemoveDir, Fs.Rename, Fs.SyncDir, Fs.Stat, Fs.ReadDir
  sys.fs.create_dir("data")?
  durable_replace("data", "data/.message.tmp", "data/message.bin", b"hello")?
  info = sys.fs.stat("data/message.bin")?
  entries = read_dir_sorted("data", 16)?
  found = false
  for item in entries
    case item
    in Ok(entry)
      if entry.name == "message.bin"
        found = true
      end
    in Err(_) then ()
    end
  end
  sys.fs.rename("data/message.bin", "data/final.bin", RenameMode.NoReplace)?
  sys.fs.remove_file("data/final.bin")?
  sys.fs.remove_dir("data")?
  Ok((info.byte_length, found))
end

go()
"#;

#[test]
fn durable_replacement_uses_the_required_sync_order() {
    let bytes = compile_to_bytes("durable_fs.lm", DURABLE_FLOW).expect("the flow compiles");
    let loaded = load_bytes(&bytes).expect("the flow loads");
    let host = Rc::new(RefCell::new(RecordingHost::new(1)));
    let mut world = World::new(&loaded, VmConfig::default(), Box::new(host.clone()));
    world.allow("Fs").expect("the file-system grant exists");

    let outcome = lm_proc::run_world(&mut world);

    assert_eq!(world.show_outcome(&outcome), "Done(Ok((5, true)))");
    assert_eq!(
        host.borrow().operations,
        vec![
            lm_abi::OP_FS_CREATE_DIR,
            lm_abi::OP_FS_OPEN,
            lm_abi::OP_FS_WRITE,
            lm_abi::OP_FS_FLUSH,
            lm_abi::OP_FS_SYNC,
            lm_abi::OP_FS_CLOSE,
            lm_abi::OP_FS_RENAME,
            lm_abi::OP_FS_SYNC_DIR,
            lm_abi::OP_FS_STAT,
            lm_abi::OP_FS_READ_DIR,
            lm_abi::OP_FS_RENAME,
            lm_abi::OP_FS_REMOVE_FILE,
            lm_abi::OP_FS_REMOVE_DIR,
        ]
    );
    assert_eq!(host.borrow().file("data/.message.tmp"), None);
    assert_eq!(host.borrow().file("data/final.bin"), None);
}

#[test]
fn create_new_reports_an_existing_path() {
    let source = r#"
def go(): Result[Bool, FsError] with Fs.Open, Fs.Close
  first = sys.fs.open("one.bin", CreateNew)?
  first.close()?
  case sys.fs.open("one.bin", CreateNew)
  in Err(FsError.AlreadyExists(_)) then Ok(true)
  in Err(error) then Err(error)
  in Ok(file)
    file.close()
    Ok(false)
  end
end

go()
"#;
    let (outcome, _) = lm_testkit::run_world("create_new.lm", source, &["Fs"], VmConfig::default())
        .expect("the flow runs");
    assert_eq!(outcome, "Done(Ok(true))");
}

#[test]
fn the_recording_host_renames_a_directory_tree() {
    let source = r#"
def go(): Result[Bool, FsError] with Fs.CreateDir, Fs.Open, Fs.Close, Fs.Rename, Fs.Stat
  sys.fs.create_dir("old")?
  sys.fs.create_dir("old/child")?
  file = sys.fs.open("old/child/value.bin", CreateNew)?
  file.close()?
  sys.fs.rename("old", "new", RenameMode.NoReplace)?
  info = sys.fs.stat("new/child/value.bin")?
  case info.kind
  in FileKind.File then Ok(true)
  in _ then Ok(false)
  end
end

go()
"#;
    let (outcome, _) =
        lm_testkit::run_world("rename_tree.lm", source, &["Fs"], VmConfig::default())
            .expect("the flow runs");
    assert_eq!(outcome, "Done(Ok(true))");
}

#[test]
fn the_file_system_example_uses_portable_metadata() {
    let path = repo_root().join("examples/09-handles-and-supervision/15-filesystem-metadata.lm");
    let source = std::fs::read_to_string(path).expect("the example reads");
    let bytes =
        compile_to_bytes("15-filesystem-metadata.lm", &source).expect("the example compiles");
    let loaded = load_bytes(&bytes).expect("the example loads");
    let host = Rc::new(RefCell::new(RecordingHost::new(1)));
    host.borrow_mut().set_file("entry.bin", vec![1, 2, 3]);
    let mut world = World::new(&loaded, VmConfig::default(), Box::new(host));
    world.allow("Fs").expect("the file-system grant exists");

    let outcome = lm_proc::run_world(&mut world);

    assert_eq!(world.show_outcome(&outcome), "Done(Ok((true, true)))");
}

#[test]
fn the_verifier_checks_new_file_boundary_roles() {
    let mut module =
        compile_text("file_roles.lm", "sys.fs.stat(\".\")\n").expect("the probe compiles");
    let file_info = module.core_roles[ROLE_FILE_INFO] as usize;
    let int_ty = module
        .types
        .iter()
        .position(|ty| *ty == lm_bytecode::BcType::Int)
        .expect("the integer type exists") as u32;
    module.classes[file_info]
        .fields
        .push(("extra".to_string(), int_ty));

    let error = lm_verify::verify_module(&module).expect_err("the forged role rejects");

    assert!(
        error
            .message
            .contains("the FileInfo role does not name its final value class"),
        "{error}"
    );
}

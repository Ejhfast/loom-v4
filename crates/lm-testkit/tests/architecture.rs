//! Mechanical checks for pure runtime crate boundaries.

use std::fs;
use std::path::{Path, PathBuf};

fn rust_files(path: &Path, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(path).expect("the source directory opens") {
        let path = entry.expect("the source entry is valid").path();
        if path.is_dir() {
            rust_files(&path, files);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
}

#[test]
fn pure_runtime_crates_do_not_read_the_ambient_clock() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("the workspace root exists");
    let mut files = Vec::new();
    for name in ["lm-bytecode", "lm-heap", "lm-vm"] {
        rust_files(&root.join("crates").join(name).join("src"), &mut files);
    }
    for file in files {
        let source = fs::read_to_string(&file).expect("the Rust source is valid UTF-8");
        for forbidden in [
            "std::time::Instant",
            "std::time::SystemTime",
            "Instant::now(",
            "SystemTime::now(",
        ] {
            assert!(
                !source.contains(forbidden),
                "{} contains forbidden clock access `{forbidden}`",
                file.display()
            );
        }
    }
}

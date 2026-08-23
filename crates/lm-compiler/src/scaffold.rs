//! `lm new`: scaffold one package.
//!
//! The layout is the whole convention: one manifest and one `src`
//! tree with `src/main.lm`.

use crate::manifest::{render_manifest, valid_name, Manifest};
use std::path::Path;

/// The first program of a new package.
const MAIN: &str = "def greeting(name: String): String\n\
                    \x20 \"Hello #{name}!\"\n\
                    end\n\
                    \n\
                    def main() with Io.Write\n\
                    \x20 line = greeting(\"world\")\n\
                    \x20 println(line).expect(\"the output writes\")\n\
                    end\n\
                    \n\
                    main()\n";

/// Create one package directory with a manifest and `src/main.lm`.
pub fn new_package(dir: &Path, name: &str) -> Result<(), String> {
    if !valid_name(name) {
        return Err(format!(
            "error: `{name}` is not a package name; use a lowercase letter, \
             then letters, digits, or underscores\n"
        ));
    }
    if dir.exists() {
        return Err(format!("error: `{}` exists already\n", dir.display()));
    }
    let src = dir.join("src");
    std::fs::create_dir_all(&src)
        .map_err(|e| format!("error: cannot create `{}`: {e}\n", src.display()))?;
    let manifest = Manifest {
        name: name.to_string(),
        version: "0.1.0".to_string(),
        dependencies: Vec::new(),
    };
    std::fs::write(dir.join("lm.package"), render_manifest(&manifest))
        .map_err(|e| format!("error: cannot write the manifest: {e}\n"))?;
    std::fs::write(src.join("main.lm"), MAIN)
        .map_err(|e| format!("error: cannot write `src/main.lm`: {e}\n"))?;
    Ok(())
}

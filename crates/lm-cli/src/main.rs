//! The `lm` command-line tool.
//!
//! Subcommands:
//! - `lm check <file>`: parse and type-check one module.
//! - `lm build <file.lm>`: write `build/debug/<name>.lma` and
//!   `<name>.lmi` and print the semantic and container hashes. The
//!   build directory is created relative to the current working
//!   directory.
//! - `lm run [--show-result] [--allow LIST] <file.lm | file.lma>`:
//!   compile or load, verify, and run with an explicit root policy.
//! - `lm disasm <file.lm | file.lma>`: print the bytecode listing.
//! - `lm inspect <file.lmi | file.lma>`: dump an interface or an
//!   artifact summary; `--live` runs a program and dumps the machine.

use lm_source::SourceFile;
use lm_vm::{VmConfig, World};
use std::path::Path;
use std::process::ExitCode;

const USAGE: &str = "usage:
  lm check <file.lm>
  lm build <file.lm>
  lm run [--show-result] [--allow Op1,Group2,...] [--rand-seed N]
         [--fuel N] [--max-frames N] [--heap-bytes N] <file.lm | file.lma>
  lm disasm <file.lm | file.lma>
  lm inspect <file.lmi | file.lma>
  lm inspect --live [--fuel N] [--max-frames N] [--heap-bytes N] <file.lm>";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run_cli(&args) {
        Ok(code) => code,
        Err(message) => {
            eprint!("{message}");
            ExitCode::from(1)
        }
    }
}

/// Run the tool. `Err` carries fully rendered error text.
fn run_cli(args: &[String]) -> Result<ExitCode, String> {
    let Some((command, rest)) = args.split_first() else {
        return Err(format!("{USAGE}\n"));
    };
    match command.as_str() {
        "check" => {
            // `check` runs the full admission path: parse, check,
            // lower, and verify. A success means `run` accepts the
            // program.
            let options = parse_options(rest)?;
            let source = read_source(&options.file)?;
            let module = compile(&source)?;
            lm_verify::verify_module(&module)
                .map_err(|e| format!("error: the verifier rejected the module: {e}\n"))?;
            Ok(ExitCode::SUCCESS)
        }
        "build" => {
            let options = parse_options(rest)?;
            build_artifact(&options.file)
        }
        "run" => {
            let options = parse_options(rest)?;
            let loaded = load_program(&options.file)?;
            let host = Box::new(lm_host::CliHost::new(options.rand_seed));
            let mut world = World::new(&loaded, options.config, host);
            for grant in &options.allow {
                world
                    .allow(grant)
                    .map_err(|e| format!("error: --allow: {e}\n{USAGE}\n"))?;
            }
            let outcome = world.run_root();
            let text = world.show_outcome(&outcome);
            if options.show_result {
                println!("{text}");
            } else if matches!(outcome, lm_vm::Outcome::Fault(_)) {
                eprintln!("{text}");
            }
            match outcome {
                lm_vm::Outcome::Done(_) => Ok(ExitCode::SUCCESS),
                lm_vm::Outcome::Fault(_) => Ok(ExitCode::from(1)),
            }
        }
        "disasm" => {
            let options = parse_options(rest)?;
            let module = read_module(&options.file)?;
            print!("{}", lm_hir::dump_cfg(&module));
            Ok(ExitCode::SUCCESS)
        }
        "inspect" => {
            let options = parse_options(rest)?;
            if options.live {
                let loaded = load_program(&options.file)?;
                let host = Box::new(lm_host::CliHost::new(options.rand_seed));
                let mut world = World::new(&loaded, options.config, host);
                for grant in &options.allow {
                    world
                        .allow(grant)
                        .map_err(|e| format!("error: --allow: {e}\n{USAGE}\n"))?;
                }
                let outcome = world.run_root();
                print!("{}", world.dump_live(&outcome));
                return Ok(ExitCode::SUCCESS);
            }
            match extension(&options.file) {
                "lmi" => {
                    let bytes = read_bytes(&options.file)?;
                    let interface = lm_bytecode::interface::decode_interface(&bytes)
                        .map_err(|e| format!("error: cannot decode the interface: {e}\n"))?;
                    print!("{}", lm_bytecode::interface::dump_interface(&interface));
                    Ok(ExitCode::SUCCESS)
                }
                "lma" => {
                    let bytes = read_bytes(&options.file)?;
                    let module = lm_bytecode::decode(&bytes)
                        .map_err(|e| format!("error: cannot decode the artifact: {e}\n"))?;
                    let identity = lm_bytecode::identity::module_identity(&module)
                        .map_err(|e| format!("error: {e}\n"))?;
                    println!("module   {}", hex(&identity.semantic_hash));
                    println!(
                        "container {}",
                        hex(&lm_bytecode::identity::container_hash(&bytes))
                    );
                    println!(
                        "classes {} functions {} entry fn{}",
                        module.classes.len(),
                        module.funcs.len(),
                        module.entry
                    );
                    Ok(ExitCode::SUCCESS)
                }
                _ => Err(format!(
                    "error: `lm inspect` reads `.lmi` and `.lma` files, or a \
                     source file with `--live`\n{USAGE}\n"
                )),
            }
        }
        other => Err(format!("error: unknown command `{other}`\n{USAGE}\n")),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn extension(path: &str) -> &str {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
}

/// Load a runnable program: a prebuilt artifact for `.lma`, a source
/// compilation otherwise. Both paths admit code through the one
/// verifier.
fn load_program(path: &str) -> Result<lm_vm::LoadedModule, String> {
    if extension(path) == "lma" {
        let bytes = read_bytes(path)?;
        let mut cache = lm_vm::VerifiedCache::new();
        lm_vm::load_bytes_cached(&bytes, &mut cache)
            .map_err(|e| format!("error: the loader rejected the artifact: {e}\n"))
    } else {
        let source = read_source(path)?;
        let module = compile(&source)?;
        lm_vm::load(module).map_err(|e| format!("error: the verifier rejected the module: {e}\n"))
    }
}

/// Read a decoded module from an artifact or a source file.
fn read_module(path: &str) -> Result<lm_bytecode::Module, String> {
    if extension(path) == "lma" {
        let bytes = read_bytes(path)?;
        lm_bytecode::decode(&bytes).map_err(|e| format!("error: cannot decode the artifact: {e}\n"))
    } else {
        let source = read_source(path)?;
        compile(&source)
    }
}

/// Build one source file into `build/debug/<name>.lma` plus
/// `<name>.lmi`, with atomic writes and printed hashes.
fn build_artifact(path: &str) -> Result<ExitCode, String> {
    if extension(path) != "lm" {
        return Err(format!(
            "error: `lm build` takes a `.lm` source file\n{USAGE}\n"
        ));
    }
    let source = read_source(path)?;
    let ast = lm_source::parse::parse(&source.text).map_err(|d| d.render(&source))?;
    let hir = lm_hir::check_module(&ast).map_err(|d| d.render(&source))?;
    let module = lm_hir::lower_module(&hir);
    lm_verify::verify_module(&module)
        .map_err(|e| format!("error: the verifier rejected the module: {e}\n"))?;
    let identity =
        lm_bytecode::identity::module_identity(&module).map_err(|e| format!("error: {e}\n"))?;
    let container = lm_bytecode::encode(&module);
    let container_hash = lm_bytecode::identity::container_hash(&container);
    // The exported top-level definitions, in declaration order.
    use lm_bytecode::interface::ExportKind;
    let mut exports: Vec<(ExportKind, String)> = Vec::new();
    for class in &ast.classes {
        exports.push((ExportKind::Class, class.name.clone()));
    }
    for enum_def in &ast.enums {
        exports.push((ExportKind::Enum, enum_def.name.clone()));
        for arm in &enum_def.arms {
            exports.push((
                ExportKind::EnumCase,
                format!("{}.{}", enum_def.name, arm.name),
            ));
        }
    }
    for func in &ast.funcs {
        exports.push((ExportKind::Function, func.name.clone()));
    }
    let interface = lm_bytecode::interface::build_interface(&module, &identity, &exports)
        .map_err(|e| format!("error: {e}\n"))?;
    let interface_bytes = lm_bytecode::interface::encode_interface(&interface);
    let stem = Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| format!("error: `{path}` has no file name\n"))?;
    let dir = Path::new("build").join("debug");
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("error: cannot create `{}`: {e}\n", dir.display()))?;
    write_atomic(&dir.join(format!("{stem}.lma")), &container)?;
    write_atomic(&dir.join(format!("{stem}.lmi")), &interface_bytes)?;
    println!("built {stem}");
    println!("  semantic  {}", hex(&identity.semantic_hash));
    println!("  container {}", hex(&container_hash));
    Ok(ExitCode::SUCCESS)
}

/// Write a file atomically: write a temporary file in the same
/// directory, then rename it over the final path.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = Path::new(&tmp);
    std::fs::write(tmp, bytes)
        .map_err(|e| format!("error: cannot write `{}`: {e}\n", tmp.display()))?;
    std::fs::rename(tmp, path)
        .map_err(|e| format!("error: cannot rename to `{}`: {e}\n", path.display()))
}

struct Options {
    file: String,
    show_result: bool,
    live: bool,
    allow: Vec<String>,
    rand_seed: u64,
    config: VmConfig,
}

fn parse_options(args: &[String]) -> Result<Options, String> {
    let mut file = None;
    let mut show_result = false;
    let mut live = false;
    let mut allow = Vec::new();
    let mut rand_seed = 1;
    let mut config = VmConfig::default();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--show-result" => show_result = true,
            "--live" => live = true,
            "--allow" => {
                let list = iter
                    .next()
                    .ok_or_else(|| "error: `--allow` needs a list of grants\n".to_string())?;
                allow.extend(list.split(',').map(|s| s.trim().to_string()));
            }
            "--rand-seed" => rand_seed = flag_value(&mut iter, "--rand-seed")?,
            "--fuel" => config.fuel = flag_value(&mut iter, "--fuel")?,
            "--max-frames" => config.max_frames = flag_value(&mut iter, "--max-frames")?,
            "--heap-bytes" => config.heap_bytes = flag_value(&mut iter, "--heap-bytes")?,
            other if other.starts_with("--") => {
                return Err(format!("error: unknown option `{other}`\n{USAGE}\n"));
            }
            other => {
                if file.replace(other.to_string()).is_some() {
                    return Err(format!("error: more than one input file\n{USAGE}\n"));
                }
            }
        }
    }
    let file = file.ok_or_else(|| format!("error: no input file\n{USAGE}\n"))?;
    Ok(Options {
        file,
        show_result,
        live,
        allow,
        rand_seed,
        config,
    })
}

fn flag_value<T: std::str::FromStr>(
    iter: &mut std::slice::Iter<'_, String>,
    flag: &str,
) -> Result<T, String> {
    let value = iter
        .next()
        .ok_or_else(|| format!("error: `{flag}` needs a number\n"))?;
    value
        .parse()
        .map_err(|_| format!("error: `{flag}` needs a number, found `{value}`\n"))
}

fn read_source(path: &str) -> Result<SourceFile, String> {
    let bytes = read_bytes(path)?;
    let text = String::from_utf8(bytes)
        .map_err(|_| format!("error[E0000]: `{path}` is not valid UTF-8\n"))?;
    Ok(SourceFile::new(path, text))
}

fn read_bytes(path: &str) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|e| format!("error: cannot read `{path}`: {e}\n"))
}

/// Compile one source file to decoded bytecode.
fn compile(source: &SourceFile) -> Result<lm_bytecode::Module, String> {
    let ast = lm_source::parse::parse(&source.text).map_err(|d| d.render(source))?;
    let hir = lm_hir::check_module(&ast).map_err(|d| d.render(source))?;
    Ok(lm_hir::lower_module(&hir))
}

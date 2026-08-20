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
  lm new <name>
  lm check <file.lm>
  lm build [file.lm | package directory]
  lm run [--show-result] [--allow Op1,Group2,...] [--rand-seed N]
         [--fuel N] [--max-frames N] [--heap-bytes N]
         [file.lm | file.lma | package directory]
  (`lm build` and `lm run` default to the current directory)
  lm disasm <file.lm | file.lma>
  lm inspect --shapes
  lm inspect <file.lmi | file.lma>
  lm inspect --live [--fuel N] [--max-frames N] [--heap-bytes N] <file.lm>
  lm inspect <file.lms> [--program <file.lm | file.lma>]
  lm snapshot save [--allow LIST] <file.lm> <out.lms>
  lm snapshot verify [--program <file.lm | file.lma>] <file.lms>
  lm snapshot run [--allow LIST] [--program <file>] <file.lms>";

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
            // lower, and verify. It compiles exactly what `run` and
            // `build` compile, so a success means `run` accepts the
            // program and `build` writes it.
            let options = parse_options(rest)?;
            let source = read_source(&options.file)?;
            let module = compile(&source)?;
            lm_verify::verify_module(&module)
                .map_err(|e| format!("error: the verifier rejected the module: {e}\n"))?;
            Ok(ExitCode::SUCCESS)
        }
        "new" => {
            let options = parse_options(rest)?;
            let dir = Path::new(&options.file);
            let name = dir
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| format!("error: `{}` has no name\n", options.file))?;
            lm_compiler::scaffold::new_package(dir, name)?;
            println!("created {}", dir.display());
            println!("  {}", dir.join("lm.package").display());
            println!("  {}", dir.join("src").join("main.lm").display());
            Ok(ExitCode::SUCCESS)
        }
        "build" => {
            let options = parse_options_with(rest, Some("."))?;
            if is_package(&options.file) {
                build_package(&options.file, false)?;
                return Ok(ExitCode::SUCCESS);
            }
            build_artifact(&options.file)
        }
        "run" => {
            let options = parse_options_with(rest, Some("."))?;
            if is_package(&options.file) {
                let report = build_package(&options.file, true)?;
                let program = report.program.ok_or_else(|| {
                    format!(
                        "error: the package `{}` has no `src/main.lm`, so it \
                         builds no program\n",
                        report.root
                    )
                })?;
                let mut options = options;
                options.file = program.display().to_string();
                return run_program(options);
            }
            run_program(options)
        }
        "disasm" => {
            let options = parse_options(rest)?;
            let module = read_module(&options.file)?;
            print!("{}", lm_hir::dump_cfg(&module));
            Ok(ExitCode::SUCCESS)
        }
        "inspect" => {
            let options = parse_options_with(rest, Some("."))?;
            if options.shapes {
                // The native shape table: the one declaration point
                // for child order, boundary policy, digestibility,
                // and snapshot classification.
                print!("{}", lm_vm::dump_shapes());
                return Ok(ExitCode::SUCCESS);
            }
            if options.live {
                let loaded = load_program(&options.file)?;
                let host = Box::new(lm_host::CliHost::new(options.rand_seed));
                let mut world = World::new(&loaded, options.config, host);
                for grant in &options.allow {
                    world
                        .allow(grant)
                        .map_err(|e| format!("error: --allow: {e}\n{USAGE}\n"))?;
                }
                let outcome = lm_proc::run_world(&mut world);
                print!("{}", world.dump_live(&outcome));
                return Ok(ExitCode::SUCCESS);
            }
            if extension(&options.file) == "lms" {
                let image = load_image(&options)?;
                print!("{}", lm_vm::snapshot::dump::dump(&image));
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
                        "classes {} functions {} bindings {} entry fn{}",
                        module.classes.len(),
                        module.funcs.len(),
                        module.bindings.len(),
                        module.entry
                    );
                    Ok(ExitCode::SUCCESS)
                }
                _ => Err(format!(
                    "error: `lm inspect` reads `.lmi` and `.lma` files, a \
                     source file with `--live`, or the shape table with \
                     `--shapes`\n{USAGE}\n"
                )),
            }
        }
        "snapshot" => {
            let Some((action, rest)) = rest.split_first() else {
                return Err(format!("error: `lm snapshot` needs an action\n{USAGE}\n"));
            };
            match action.as_str() {
                "save" => snapshot_save(rest),
                "verify" => {
                    let options = parse_options(rest)?;
                    let image = load_image(&options)?;
                    println!("{}", lm_vm::snapshot::dump::verdict(image.world()));
                    Ok(ExitCode::SUCCESS)
                }
                "run" => snapshot_run(rest),
                other => Err(format!(
                    "error: unknown snapshot action `{other}`\n{USAGE}\n"
                )),
            }
        }
        other => Err(format!("error: unknown command `{other}`\n{USAGE}\n")),
    }
}

/// The program one snapshot container belongs to.
///
/// A container names its program by semantic hash, and admission
/// (specification 17.8) reads the code hashes of that program. The
/// tool therefore needs the program beside the container: `--program`
/// names it, and the default is the file with the same stem.
fn program_of(options: &Options) -> Result<String, String> {
    if let Some(path) = &options.program {
        return Ok(path.clone());
    }
    let base = Path::new(&options.file).with_extension("");
    for ext in ["lma", "lm"] {
        let candidate = base.with_extension(ext);
        if candidate.is_file() {
            return Ok(candidate.display().to_string());
        }
    }
    Err(format!(
        "error: no program found beside `{}`; name it with `--program`\n",
        options.file
    ))
}

/// Load and check one snapshot container against its program.
fn load_image(options: &Options) -> Result<lm_vm::snapshot::SnapshotImage, String> {
    let program = program_of(options)?;
    let loaded = load_program(&program)?;
    let bytes = read_bytes(&options.file)?;
    lm_vm::snapshot::load_external(&bytes, &loaded, lm_vm::snapshot::LoadLimits::default())
        .map_err(|e| format!("error: the snapshot did not load: {e}\n"))
}

/// Run one program and write the last snapshot it captured.
fn snapshot_save(args: &[String]) -> Result<ExitCode, String> {
    let mut options = parse_options(args)?;
    let out = options
        .extra
        .take()
        .ok_or_else(|| format!("error: `lm snapshot save` needs an output file\n{USAGE}\n"))?;
    let loaded = load_program(&options.file)?;
    let host = Box::new(lm_host::CliHost::new(options.rand_seed));
    let mut world = World::new(&loaded, options.config, host);
    for grant in &options.allow {
        world
            .allow(grant)
            .map_err(|e| format!("error: --allow: {e}\n{USAGE}\n"))?;
    }
    let outcome = lm_proc::run_world(&mut world);
    let Some(image) = world.last_snapshot() else {
        return Err(format!(
            "error: `{}` captured no snapshot; the outcome was {}\n",
            options.file,
            world.show_outcome(&outcome)
        ));
    };
    let bytes = image.bytes().to_vec();
    let verdict = lm_vm::snapshot::dump::verdict(image.world());
    write_atomic(Path::new(&out), &bytes)?;
    println!("wrote {out}");
    println!("  {} bytes", bytes.len());
    println!("  {verdict}");
    Ok(ExitCode::SUCCESS)
}

/// Restore one snapshot container and drive the restored world.
fn snapshot_run(args: &[String]) -> Result<ExitCode, String> {
    let options = parse_options(args)?;
    let program = program_of(&options)?;
    let loaded = load_program(&program)?;
    let bytes = read_bytes(&options.file)?;
    let host = Box::new(lm_host::CliHost::new(options.rand_seed));
    let mut world = World::new(&loaded, options.config, host);
    // The external byte path decodes and admits the container once.
    // The restore below reads the admitted image and repeats nothing.
    let image = world
        .load_snapshot_bytes(&bytes)
        .map_err(|e| format!("error: the snapshot did not load: {e}\n"))?;
    let target = world
        .new_child(0)
        .ok_or_else(|| "error: the world has no machine budget left\n".to_string())?;
    let root = world
        .restore_image(0, target, &image)
        .map_err(|e| format!("error: the restore failed: {e:?}\n"))?;
    for grant in &options.allow {
        world
            .allow_on(root, grant)
            .map_err(|e| format!("error: --allow: {e}\n{USAGE}\n"))?;
        world
            .allow(grant)
            .map_err(|e| format!("error: --allow: {e}\n{USAGE}\n"))?;
    }
    // The restored world runs beside the machine that restored it, so
    // the tool drives the restored root and the scheduler drives the
    // restored procs.
    loop {
        match world.run_machine(root) {
            lm_vm::RootEvent::Done(value) => {
                println!("Done({})", world.show_result_of(root, value));
                return Ok(ExitCode::SUCCESS);
            }
            lm_vm::RootEvent::Fault(rec) => {
                println!("Fault({})", rec.code);
                return Ok(ExitCode::from(1));
            }
            lm_vm::RootEvent::Asked(_) => {
                let op = world
                    .pending_op_of(root)
                    .expect("an asked machine holds its request");
                println!("Asked({})", lm_abi::op_name(op));
                return Ok(ExitCode::SUCCESS);
            }
            lm_vm::RootEvent::Blocked => {
                if lm_proc::drain_procs(&mut world) > 0 {
                    continue;
                }
                println!("Fault(HostFault)");
                return Ok(ExitCode::from(1));
            }
            lm_vm::RootEvent::Ran | lm_vm::RootEvent::Waiting => {
                println!("Fault(HostFault)");
                return Ok(ExitCode::from(1));
            }
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The first twelve hex digits of a hash, for a readable report line.
fn short(bytes: &[u8]) -> String {
    hex(&bytes[..6])
}

/// True when the path names a package directory instead of a file.
fn is_package(path: &str) -> bool {
    Path::new(path).is_dir()
}

/// Shorten one path against the current directory, for the report.
fn relative_to_here(path: &Path) -> &Path {
    let here = std::env::current_dir().unwrap_or_default();
    path.strip_prefix(&here).unwrap_or(path)
}

/// Build one package and print the per-module report.
fn build_package(path: &str, to_stderr: bool) -> Result<lm_compiler::BuildReport, String> {
    // The build directory belongs to the package, not to the current
    // directory. Two builds from two directories then share one
    // cache and write one program.
    let root = lm_compiler::graph::find_package_dir(Path::new(path))?;
    let report = lm_compiler::build_package(Path::new(path), &root.join("build"))?;
    let mut lines: Vec<String> = Vec::new();
    for module in &report.modules {
        let verb = if module.cached { "cached" } else { "built " };
        lines.push(format!(
            "{verb} {}  {}",
            module.path,
            short(&module.semantic_hash)
        ));
    }
    match (&report.program, report.program_semantic) {
        (Some(program), Some(semantic)) => {
            let verb = if report.program_cached {
                "cached"
            } else {
                "linked"
            };
            lines.push(format!(
                "{verb} {}  sem={} container={}",
                report.root,
                short(&semantic),
                short(
                    &report
                        .program_container
                        .expect("a linked program has bytes")
                )
            ));
            lines.push(format!("  {}", relative_to_here(program).display()));
        }
        _ => lines.push(format!("library {} builds no program", report.root)),
    }
    for line in lines {
        if to_stderr {
            eprintln!("{line}");
        } else {
            println!("{line}");
        }
    }
    Ok(report)
}

/// Load and run one program with the given policy grants.
fn run_program(options: Options) -> Result<ExitCode, String> {
    let loaded = load_program(&options.file)?;
    // The whole machine world lives on one worker thread with a
    // bounded stack, and only the rendered outcome comes back. That
    // is the thread-backed baseline of specification 22.12.
    let seed = options.rand_seed;
    let grants: Vec<&str> = options.allow.iter().map(|g| g.as_str()).collect();
    let result = lm_proc::run_on_worker(
        &loaded,
        options.config,
        &grants,
        Box::new(move || Box::new(lm_host::CliHost::new(seed))),
    )
    .map_err(|e| format!("error: --allow: {e}\n{USAGE}\n"))?;
    let (faulted, text) = (result.faulted, result.text);
    if options.show_result {
        println!("{text}");
    } else if faulted {
        eprintln!("{text}");
    }
    if faulted {
        Ok(ExitCode::from(1))
    } else {
        Ok(ExitCode::SUCCESS)
    }
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
        let module = lm_bytecode::decode(&bytes)
            .map_err(|e| format!("error: cannot decode the artifact: {e}\n"))?;
        load_stored(Path::new(path), module)
            .map_err(|e| format!("error: the loader rejected the artifact: {e}\n"))
    } else {
        let source = read_source(path)?;
        let module = compile(&source)?;
        lm_vm::load(module).map_err(|e| format!("error: the verifier rejected the module: {e}\n"))
    }
}

/// Admit one decoded artifact through the persistent verified-code
/// store.
///
/// A hit skips the verifier pass. The key comes
/// from the decoded content on every load, so no stored value enters
/// it, and `lm_vm::load_with_record` rejects a record filed under any
/// other key.
fn load_stored(
    path: &Path,
    module: lm_bytecode::Module,
) -> Result<lm_vm::LoadedModule, lm_vm::VerifyError> {
    let store = lm_compiler::VerifiedStore::for_artifact(path);
    let key = lm_vm::verified_key(&module);
    lm_compiler::load_through_store(
        &store,
        &key,
        module,
        |module, _verdict| lm_vm::load_with_record(module, &key, &lm_vm::VerifiedRecord),
        |module| {
            let loaded = lm_vm::load(module)?;
            Ok((loaded, lm_compiler::Verdict))
        },
    )
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
    let stem = Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| format!("error: `{path}` has no file name\n"))?;
    // The file name names the output files only. It never names the
    // module: a single file has no module path.
    let compiled = compile_file(&source)?;
    let module = compiled.program;
    lm_verify::verify_module(&module)
        .map_err(|e| format!("error: the verifier rejected the module: {e}\n"))?;
    let identity =
        lm_bytecode::identity::module_identity(&module).map_err(|e| format!("error: {e}\n"))?;
    let container = compiled.artifact;
    let container_hash = lm_bytecode::identity::container_hash(&container);
    let interface_bytes = compiled.root.interface_bytes;
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

/// Write a file atomically. The tool keeps one implementation, in
/// `lm_compiler::write_atomic`, so the exclusive-create rule holds on
/// every path the tool writes.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    lm_compiler::write_atomic(path, bytes)
}

struct Options {
    file: String,
    /// A second positional input, for example the output of
    /// `lm snapshot save`.
    extra: Option<String>,
    /// The program one snapshot container belongs to.
    program: Option<String>,
    show_result: bool,
    live: bool,
    shapes: bool,
    allow: Vec<String>,
    rand_seed: u64,
    config: VmConfig,
}

fn parse_options(args: &[String]) -> Result<Options, String> {
    parse_options_with(args, None)
}

/// Parse the options of one command. `default_file` names the input
/// a command uses when the user gives none. `lm build` and `lm run`
/// default to the current directory, so both work from any directory
/// inside a package.
fn parse_options_with(args: &[String], default_file: Option<&str>) -> Result<Options, String> {
    let mut file = None;
    let mut extra = None;
    let mut program = None;
    let mut show_result = false;
    let mut live = false;
    let mut shapes = false;
    let mut allow = Vec::new();
    let mut rand_seed = 1;
    let mut config = VmConfig::default();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--show-result" => show_result = true,
            "--live" => live = true,
            "--shapes" => shapes = true,
            "--allow" => {
                let list = iter
                    .next()
                    .ok_or_else(|| "error: `--allow` needs a list of grants\n".to_string())?;
                allow.extend(list.split(',').map(|s| s.trim().to_string()));
            }
            "--program" => {
                program = Some(
                    iter.next()
                        .ok_or_else(|| "error: `--program` needs a file\n".to_string())?
                        .clone(),
                );
            }
            "--rand-seed" => rand_seed = flag_value(&mut iter, "--rand-seed")?,
            "--fuel" => config.fuel = flag_value(&mut iter, "--fuel")?,
            "--max-frames" => config.max_frames = flag_value(&mut iter, "--max-frames")?,
            "--heap-bytes" => config.heap_bytes = flag_value(&mut iter, "--heap-bytes")?,
            other if other.starts_with("--") => {
                return Err(format!("error: unknown option `{other}`\n{USAGE}\n"));
            }
            other => {
                if file.is_none() {
                    file = Some(other.to_string());
                } else if extra.is_none() {
                    extra = Some(other.to_string());
                } else {
                    return Err(format!("error: more than two input files\n{USAGE}\n"));
                }
            }
        }
    }
    let file = file
        .or_else(|| default_file.map(|f| f.to_string()))
        .ok_or_else(|| format!("error: no input file\n{USAGE}\n"))?;
    Ok(Options {
        file,
        extra,
        program,
        show_result,
        live,
        shapes,
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

/// The module path of one source file outside a package.
///
/// **The rule: a single source file has no module path.** A module
/// path comes from a package: the manifest supplies the root and the
/// directory tree supplies the rest. One file has neither, so `lm
/// check`, `lm run <file>.lm`, and `lm build <file>.lm` all compile it
/// with the empty path. One file therefore gives one set of qualified
/// keys, one semantic hash, and one admission answer, whichever
/// command a user runs. A file name is not a module name: it may hold
/// characters a module name cannot, and it may be `core`, which the
/// core image reserves.
const SINGLE_FILE_MODULE_PATH: &str = "";

/// Compile one source file and its selected standard modules.
fn compile_file(source: &SourceFile) -> Result<lm_compiler::CompiledSource, String> {
    lm_compiler::compile_source(SINGLE_FILE_MODULE_PATH, source, true)
}

/// Compile one source file to decoded bytecode.
fn compile(source: &SourceFile) -> Result<lm_bytecode::Module, String> {
    lm_compiler::compile_program(SINGLE_FILE_MODULE_PATH, source)
}

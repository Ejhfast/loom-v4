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
use lm_vm::{VmConfig, World, WorldLimits};
use std::io::Write as _;
use std::path::Path;
use std::process::ExitCode;

fn report_stdout(args: std::fmt::Arguments<'_>) {
    let _ = std::io::stdout().lock().write_fmt(args);
}

fn report_stderr(args: std::fmt::Arguments<'_>) {
    let _ = std::io::stderr().lock().write_fmt(args);
}

macro_rules! out {
    ($($arg:tt)*) => {
        report_stdout(format_args!($($arg)*))
    };
}

macro_rules! outln {
    ($($arg:tt)*) => {
        report_stdout(format_args!("{}\n", format_args!($($arg)*)))
    };
}

macro_rules! errln {
    ($($arg:tt)*) => {
        report_stderr(format_args!("{}\n", format_args!($($arg)*)))
    };
}

const USAGE: &str = "usage:
  lm new <name>
  lm check <file.lm>
  lm build [file.lm | package directory]
  lm run [--show-result] [--allow Op1,Group2,...] [--rand-seed N]
         [--fuel N] [--max-frames N] [--heap-bytes N]
         [--max-machines N] [--max-images N]
         [--max-children N] [--max-waits N]
         [--scheduler deterministic|parallel] [--threads N]
         [file.lm | file.lma | package directory] [-- arguments...]
  (`lm build` and `lm run` default to the current directory)
  lm disasm <file.lm | file.lma>
  lm inspect --shapes
  lm inspect <file.lmi | file.lma>
  lm inspect --live [--fuel N] [--max-frames N] [--heap-bytes N] <file.lm>
  lm inspect <file.lms>
  lm snapshot save [--allow LIST] <file.lm> <out.lms>
  lm snapshot verify <file.lms>
  lm snapshot run [--allow LIST] <file.lms>";

fn main() -> ExitCode {
    let args: Result<Vec<String>, _> = std::env::args_os()
        .skip(1)
        .map(|argument| argument.into_string())
        .collect();
    let Ok(args) = args else {
        report_stderr(format_args!(
            "error: a command-line argument is not valid UTF-8\n"
        ));
        return ExitCode::from(1);
    };
    match run_cli(&args) {
        Ok(code) => code,
        Err(message) => {
            report_stderr(format_args!("{message}"));
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
        "--help" | "-h" => {
            out!("{USAGE}\n");
            Ok(ExitCode::SUCCESS)
        }
        "--version" | "-V" => {
            outln!("lm {}", env!("CARGO_PKG_VERSION"));
            Ok(ExitCode::SUCCESS)
        }
        "check" => {
            // `check` runs the full admission path: parse, check,
            // lower, and verify. It compiles exactly what `run` and
            // `build` compile, so a success means `run` accepts the
            // program and `build` writes it.
            let options = parse_options(rest)?;
            let source = read_source(&options.file)?;
            let artifact = compile_file(&source)?.artifact;
            publish_artifact(artifact)?;
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
            outln!("created {}", dir.display());
            outln!("  {}", dir.join("lm.package").display());
            outln!("  {}", dir.join("src").join("main.lm").display());
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
            let options = parse_run_options(rest, Some("."))?;
            if is_package(&options.file) {
                let report = build_package(&options.file, true)?;
                let artifact = report.artifact.ok_or_else(|| {
                    format!(
                        "error: the package `{}` has no `src/main.lm`, so it \
                         builds no executable artifact\n",
                        report.root
                    )
                })?;
                let mut options = options;
                options.file = artifact.display().to_string();
                return run_program(options);
            }
            run_program(options)
        }
        "disasm" => {
            let options = parse_options(rest)?;
            let artifact = read_artifact(&options.file)?;
            out!("{}", lm_hir::dump_cfg(artifact.root().module()));
            Ok(ExitCode::SUCCESS)
        }
        "inspect" => {
            let options = parse_options_with(rest, Some("."))?;
            if options.shapes {
                // The native shape table: the one declaration point
                // for child order, boundary policy, digestibility,
                // and snapshot classification.
                out!("{}", lm_vm::dump_shapes());
                return Ok(ExitCode::SUCCESS);
            }
            if options.live {
                let (arena, namespace) = load_artifact(&options.file)?;
                let host = Box::new(lm_host::CliHost::new(options.rand_seed));
                let mut world =
                    World::new_with_limits(arena, namespace, options.config, options.limits, host);
                for grant in &options.allow {
                    world
                        .allow(grant)
                        .map_err(|e| format!("error: --allow: {e}\n{USAGE}\n"))?;
                }
                let outcome = lm_proc::run_world(&mut world);
                out!("{}", world.dump_live(&outcome));
                return Ok(ExitCode::SUCCESS);
            }
            if extension(&options.file) == "lms" {
                let image = load_image(&options)?;
                out!("{}", lm_vm::snapshot::dump::dump(&image));
                return Ok(ExitCode::SUCCESS);
            }
            match extension(&options.file) {
                "lmi" => {
                    let bytes = read_bytes(&options.file)?;
                    let interface = lm_bytecode::interface::decode_interface(&bytes)
                        .map_err(|e| format!("error: cannot decode the interface: {e}\n"))?;
                    out!("{}", lm_bytecode::interface::dump_interface(&interface));
                    Ok(ExitCode::SUCCESS)
                }
                "lma" => {
                    let bytes = read_bytes(&options.file)?;
                    let artifact = lm_bytecode::artifact::decode(&bytes)
                        .map_err(|e| format!("error: cannot decode the artifact: {e}\n"))?;
                    let artifact_id = artifact.id();
                    let unit_count = artifact.units().len();
                    let unit = artifact.root();
                    let module = unit.module();
                    let identity = lm_bytecode::identity::module_identity(module)
                        .map_err(|e| format!("error: {e}\n"))?;
                    outln!("artifact {}", artifact_id);
                    outln!("module   {}", hex(&identity.semantic_hash));
                    outln!(
                        "container {}",
                        hex(&lm_bytecode::identity::container_hash(&bytes))
                    );
                    outln!(
                        "classes {} functions {} bindings {} entry fn{}",
                        module.classes.len(),
                        module.funcs.len(),
                        module.bindings.len(),
                        module.entry
                    );
                    outln!("units {unit_count}");
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
                    outln!("{}", lm_vm::snapshot::dump::verdict(image.world()));
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

/// Publish the exact runtime core as one artifact namespace.
fn runtime_core() -> Result<(lm_link::CodeArena, lm_link::NamespaceId), String> {
    let unit = lm_compiler::core_link_unit()?;
    let artifact = lm_bytecode::artifact::Artifact::new(unit.as_ref().clone(), Vec::new())
        .map_err(|error| format!("error: the runtime core artifact is invalid: {error}\n"))?;
    let mut arena = lm_link::CodeArena::new();
    let namespace = arena
        .publish(artifact, None)
        .map_err(|error| format!("error: the runtime core did not publish: {error}\n"))?;
    Ok((arena, namespace))
}

/// Load and check one self-describing snapshot container.
fn load_image(options: &Options) -> Result<lm_vm::snapshot::SnapshotImage, String> {
    let (arena, namespace) = runtime_core()?;
    let bytes = read_bytes(&options.file)?;
    let mut world = World::new_with_limits(
        arena,
        namespace,
        options.config,
        options.limits,
        Box::new(lm_vm::NullHost),
    );
    world
        .load_snapshot_bytes(&bytes)
        .map_err(|e| format!("error: the snapshot did not load: {e}\n"))
}

/// Run one program and write the last snapshot it captured.
fn snapshot_save(args: &[String]) -> Result<ExitCode, String> {
    let mut options = parse_options(args)?;
    let out = options
        .extra
        .take()
        .ok_or_else(|| format!("error: `lm snapshot save` needs an output file\n{USAGE}\n"))?;
    let (arena, namespace) = load_artifact(&options.file)?;
    let host = Box::new(lm_host::CliHost::new(options.rand_seed));
    let mut world = World::new_with_limits(arena, namespace, options.config, options.limits, host);
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
    // The capture holds the admitted world. The container appears
    // here, because this command writes it to a file.
    let bytes = image
        .bytes()
        .map_err(|error| format!("error: the snapshot did not encode: {error:?}\n"))?
        .to_vec();
    let verdict = lm_vm::snapshot::dump::verdict(image.world());
    write_atomic(Path::new(&out), &bytes)?;
    outln!("wrote {out}");
    outln!("  {} bytes", bytes.len());
    outln!("  {verdict}");
    Ok(ExitCode::SUCCESS)
}

/// Restore one snapshot container and drive the restored world.
fn snapshot_run(args: &[String]) -> Result<ExitCode, String> {
    let options = parse_options(args)?;
    let (arena, namespace) = runtime_core()?;
    let bytes = read_bytes(&options.file)?;
    let host = Box::new(lm_host::CliHost::new(options.rand_seed));
    let mut world = World::new_with_limits(arena, namespace, options.config, options.limits, host);
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
                outln!("Done({})", world.show_result_of(root, value));
                return Ok(ExitCode::SUCCESS);
            }
            lm_vm::RootEvent::Fault(rec) => {
                outln!("Fault({})", rec.code);
                for line in world.fault_context(&rec) {
                    outln!("{line}");
                }
                return Ok(ExitCode::from(1));
            }
            lm_vm::RootEvent::Asked(_) => {
                let op = world
                    .pending_op_of(root)
                    .expect("an asked machine holds its request");
                outln!("Asked({})", lm_abi::op_name(op));
                return Ok(ExitCode::SUCCESS);
            }
            lm_vm::RootEvent::Blocked => {
                if lm_proc::drain_procs(&mut world) > 0 {
                    continue;
                }
                outln!("Fault(HostFault)");
                return Ok(ExitCode::from(1));
            }
            lm_vm::RootEvent::Ran | lm_vm::RootEvent::Waiting => {
                outln!("Fault(HostFault)");
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
    match (&report.artifact, report.artifact_id) {
        (Some(artifact), Some(id)) => {
            let verb = if report.artifact_cached {
                "cached"
            } else {
                "built "
            };
            lines.push(format!(
                "{verb} {}  artifact={} container={}",
                report.root,
                short(id.as_bytes()),
                short(&report.container_hash.expect("an artifact has bytes"))
            ));
            lines.push(format!("  {}", relative_to_here(artifact).display()));
        }
        _ => lines.push(format!(
            "library {} builds no executable artifact",
            report.root
        )),
    }
    for line in lines {
        if to_stderr {
            errln!("{line}");
        } else {
            outln!("{line}");
        }
    }
    Ok(report)
}

/// Load and run one program with the given policy grants.
fn run_program(options: Options) -> Result<ExitCode, String> {
    let (arena, namespace) = load_artifact(&options.file)?;
    // The whole machine world lives on one worker thread with a
    // bounded stack, and only the rendered outcome comes back. That
    // is the thread-backed baseline of specification 22.12.
    let seed = options.rand_seed;
    let grants: Vec<&str> = options.allow.iter().map(|g| g.as_str()).collect();
    let arguments = options.command_args;
    let result = lm_proc::run_on_worker_with_scheduler_and_limits(
        arena,
        namespace,
        options.config,
        options.limits,
        options.scheduler,
        &grants,
        Box::new(move || Box::new(lm_host::CliHost::with_args(seed, arguments))),
    )
    .map_err(|e| format!("error: {e}\n"))?;
    let (faulted, text, fault_context) = (result.faulted, result.text, result.fault_context);
    if options.show_result {
        outln!("{text}");
    } else if faulted {
        errln!("{text}");
    }
    if faulted {
        for line in fault_context {
            if options.show_result {
                outln!("{line}");
            } else {
                errln!("{line}");
            }
        }
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

/// Read one artifact from a built file or a source file.
fn read_artifact(path: &str) -> Result<lm_bytecode::artifact::Artifact, String> {
    if extension(path) == "lma" {
        let bytes = read_bytes(path)?;
        lm_bytecode::artifact::decode(&bytes)
            .map_err(|e| format!("error: cannot decode the artifact: {e}\n"))
    } else {
        let source = read_source(path)?;
        compile_file(&source).map(|compiled| compiled.artifact)
    }
}

/// Publish one artifact against the runtime core.
fn publish_artifact(
    artifact: lm_bytecode::artifact::Artifact,
) -> Result<(lm_link::CodeArena, lm_link::NamespaceId), String> {
    let core = lm_compiler::core_link_unit()?;
    let mut arena = lm_link::CodeArena::new();
    let namespace = arena
        .publish(artifact, Some(core))
        .map_err(|error| format!("error: {error}\n"))?;
    Ok((arena, namespace))
}

/// Load one artifact into a new code arena.
fn load_artifact(path: &str) -> Result<(lm_link::CodeArena, lm_link::NamespaceId), String> {
    publish_artifact(read_artifact(path)?)
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
    let identity = compiled.artifact.id();
    let container = lm_bytecode::artifact::encode(&compiled.artifact)
        .map_err(|error| format!("error: the artifact did not encode: {error}\n"))?;
    let container_hash = lm_bytecode::identity::container_hash(&container);
    let interface_bytes = lm_bytecode::interface::encode_interface(&compiled.root.interface);
    let dir = Path::new("build").join("debug");
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("error: cannot create `{}`: {e}\n", dir.display()))?;
    write_atomic(&dir.join(format!("{stem}.lma")), &container)?;
    write_atomic(&dir.join(format!("{stem}.lmi")), &interface_bytes)?;
    outln!("built {stem}");
    outln!("  artifact  {identity}");
    outln!("  container {}", hex(&container_hash));
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
    show_result: bool,
    live: bool,
    shapes: bool,
    allow: Vec<String>,
    rand_seed: u64,
    config: VmConfig,
    limits: WorldLimits,
    scheduler: lm_proc::SchedulerConfig,
    /// The tokens after the `lm run` separator.
    command_args: Vec<String>,
}

fn parse_options(args: &[String]) -> Result<Options, String> {
    parse_options_with(args, None)
}

/// Parse the options of one command. `default_file` names the input
/// a command uses when the user gives none. `lm build` and `lm run`
/// default to the current directory, so both work from any directory
/// inside a package.
fn parse_options_with(args: &[String], default_file: Option<&str>) -> Result<Options, String> {
    parse_options_mode(args, default_file, false)
}

fn parse_run_options(args: &[String], default_file: Option<&str>) -> Result<Options, String> {
    parse_options_mode(args, default_file, true)
}

fn parse_options_mode(
    args: &[String],
    default_file: Option<&str>,
    command_mode: bool,
) -> Result<Options, String> {
    let mut file = None;
    let mut extra = None;
    let mut show_result = false;
    let mut live = false;
    let mut shapes = false;
    let mut allow = Vec::new();
    let mut rand_seed = 1;
    let mut config = VmConfig::default();
    let mut limits = WorldLimits::default();
    let mut scheduler_mode = None;
    let mut threads = None;
    let mut command_args = Vec::new();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--" if command_mode => {
                command_args.extend(iter.cloned());
                break;
            }
            "--" => {
                return Err(format!("error: `--` is valid only for `lm run`\n{USAGE}\n"));
            }
            "--show-result" => show_result = true,
            "--live" => live = true,
            "--shapes" => shapes = true,
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
            "--max-machines" => {
                limits.max_machines = positive_flag_value(&mut iter, "--max-machines")?
            }
            "--max-images" => limits.max_vm_images = flag_value(&mut iter, "--max-images")?,
            "--max-children" => config.max_children = flag_value(&mut iter, "--max-children")?,
            "--max-waits" => limits.max_waits = flag_value(&mut iter, "--max-waits")?,
            "--scheduler" if command_mode => {
                if scheduler_mode.is_some() {
                    return Err("error: `--scheduler` occurs more than once\n".to_string());
                }
                scheduler_mode = Some(
                    iter.next()
                        .ok_or_else(|| {
                            "error: `--scheduler` needs `deterministic` or `parallel`\n".to_string()
                        })?
                        .clone(),
                );
            }
            "--threads" if command_mode => {
                if threads.is_some() {
                    return Err("error: `--threads` occurs more than once\n".to_string());
                }
                threads = Some(flag_value(&mut iter, "--threads")?);
            }
            other if other.starts_with("--") => {
                return Err(format!("error: unknown option `{other}`\n{USAGE}\n"));
            }
            other => {
                if file.is_none() {
                    file = Some(other.to_string());
                } else if command_mode {
                    return Err(format!(
                        "error: unexpected argument `{other}` before `--`\n{USAGE}\n"
                    ));
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
    let scheduler = parse_scheduler_options(scheduler_mode.as_deref(), threads)?;
    Ok(Options {
        file,
        extra,
        show_result,
        live,
        shapes,
        allow,
        rand_seed,
        config,
        limits,
        scheduler,
        command_args,
    })
}

fn parse_scheduler_options(
    mode: Option<&str>,
    threads: Option<usize>,
) -> Result<lm_proc::SchedulerConfig, String> {
    match mode {
        Some("deterministic") => {
            if threads.is_some() {
                return Err("error: `--threads` requires `--scheduler parallel`\n".to_string());
            }
            Ok(lm_proc::SchedulerConfig::deterministic())
        }
        Some("parallel") | None => {
            let workers = threads.unwrap_or_else(default_parallel_workers);
            if workers == 0 || workers > lm_proc::MAX_PARALLEL_WORKERS {
                return Err(
                    "error: `--threads` must be between 1 and 256 for parallel scheduling\n"
                        .to_string(),
                );
            }
            Ok(lm_proc::SchedulerConfig::parallel(workers))
        }
        Some(other) => Err(format!(
            "error: unknown scheduler `{other}`; use `deterministic` or `parallel`\n"
        )),
    }
}

fn default_parallel_workers() -> usize {
    std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1)
        .min(lm_proc::MAX_PARALLEL_WORKERS)
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

fn positive_flag_value(iter: &mut std::slice::Iter<'_, String>, flag: &str) -> Result<u32, String> {
    let value = flag_value(iter, flag)?;
    if value == 0 {
        return Err(format!("error: `{flag}` must be greater than zero\n"));
    }
    Ok(value)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_and_version_succeed() {
        assert_eq!(run_cli(&["--help".to_string()]), Ok(ExitCode::SUCCESS));
        assert_eq!(run_cli(&["-h".to_string()]), Ok(ExitCode::SUCCESS));
        assert_eq!(run_cli(&["--version".to_string()]), Ok(ExitCode::SUCCESS));
        assert_eq!(run_cli(&["-V".to_string()]), Ok(ExitCode::SUCCESS));
    }

    #[test]
    fn run_options_select_the_parallel_scheduler() {
        let args = [
            "--scheduler".to_string(),
            "parallel".to_string(),
            "--threads".to_string(),
            "3".to_string(),
            "program.lm".to_string(),
        ];
        let options = parse_run_options(&args, None).expect("the scheduler options parse");
        assert_eq!(
            options.scheduler.mode(),
            lm_proc::SchedulerMode::Parallel { workers: 3 }
        );
    }

    #[test]
    fn run_options_default_to_parallel_scheduling() {
        let options = parse_run_options(&["program.lm".to_string()], None)
            .expect("the default scheduler option parses");
        assert_eq!(
            options.scheduler.mode(),
            lm_proc::SchedulerMode::Parallel {
                workers: default_parallel_workers()
            }
        );
    }

    #[test]
    fn run_options_set_world_and_child_limits() {
        let args = [
            "--max-machines".to_string(),
            "200000".to_string(),
            "--max-images".to_string(),
            "190000".to_string(),
            "--max-children".to_string(),
            "180000".to_string(),
            "--max-waits".to_string(),
            "170000".to_string(),
            "program.lm".to_string(),
        ];
        let options = parse_run_options(&args, None).expect("the limit options parse");
        assert_eq!(options.limits.max_machines, 200_000);
        assert_eq!(options.limits.max_vm_images, 190_000);
        assert_eq!(options.config.max_children, 180_000);
        assert_eq!(options.limits.max_waits, 170_000);
    }

    #[test]
    fn run_options_use_generous_default_limits() {
        let options =
            parse_run_options(&["program.lm".to_string()], None).expect("the default limits parse");
        assert_eq!(options.limits.max_machines, 262_144);
        assert_eq!(options.limits.max_vm_images, 262_144);
        assert_eq!(options.config.max_children, 262_144);
        assert_eq!(options.limits.max_waits, 262_144);
    }

    #[test]
    fn the_machine_limit_keeps_room_for_the_root() {
        let args = [
            "--max-machines".to_string(),
            "0".to_string(),
            "program.lm".to_string(),
        ];
        let error = match parse_run_options(&args, None) {
            Ok(_) => panic!("zero machines must reject"),
            Err(error) => error,
        };
        assert!(error.contains("must be greater than zero"), "{error}");
    }

    #[test]
    fn thread_options_require_valid_parallel_scheduling() {
        let default_parallel = [
            "--threads".to_string(),
            "2".to_string(),
            "program.lm".to_string(),
        ];
        let deterministic = [
            "--scheduler".to_string(),
            "deterministic".to_string(),
            "--threads".to_string(),
            "2".to_string(),
            "program.lm".to_string(),
        ];
        let zero = [
            "--scheduler".to_string(),
            "parallel".to_string(),
            "--threads".to_string(),
            "0".to_string(),
            "program.lm".to_string(),
        ];
        let unknown = [
            "--scheduler".to_string(),
            "random".to_string(),
            "program.lm".to_string(),
        ];
        let options = parse_run_options(&default_parallel, None)
            .expect("the default parallel scheduler accepts a worker count");
        assert_eq!(
            options.scheduler.mode(),
            lm_proc::SchedulerMode::Parallel { workers: 2 }
        );
        assert!(parse_run_options(&deterministic, None).is_err());
        assert!(parse_run_options(&zero, None).is_err());
        assert!(parse_run_options(&unknown, None).is_err());
    }
}

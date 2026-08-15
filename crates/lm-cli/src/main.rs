//! The `lm` command-line tool.
//!
//! Subcommands:
//! - `lm check <file>`: parse and type-check one module.
//! - `lm run [--show-result] [--allow LIST] <file>`: compile, verify,
//!   and run with an explicit root policy.
//! - `lm disasm <file>`: print the lowered bytecode listing.
//! - `lm inspect --live <file>`: run, then dump the live machine state.

use lm_source::SourceFile;
use lm_vm::{VmConfig, World};
use std::process::ExitCode;

const USAGE: &str = "usage:
  lm check <file>
  lm run [--show-result] [--allow Op1,Group2,...] [--rand-seed N]
         [--fuel N] [--max-frames N] [--heap-bytes N] <file>
  lm disasm <file>
  lm inspect --live [--fuel N] [--max-frames N] [--heap-bytes N] <file>";

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
            let options = parse_options(rest)?;
            let source = read_source(&options.file)?;
            compile(&source)?;
            Ok(ExitCode::SUCCESS)
        }
        "run" => {
            let options = parse_options(rest)?;
            let source = read_source(&options.file)?;
            let module = compile(&source)?;
            let loaded = lm_vm::load(module)
                .map_err(|e| format!("error: the verifier rejected the module: {e}\n"))?;
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
            let source = read_source(&options.file)?;
            let module = compile(&source)?;
            print!("{}", lm_hir::dump_cfg(&module));
            Ok(ExitCode::SUCCESS)
        }
        "inspect" => {
            let options = parse_options(rest)?;
            if !options.live {
                return Err(format!(
                    "error: `lm inspect` supports only the `--live` test mode \
                     in this slice\n{USAGE}\n"
                ));
            }
            let source = read_source(&options.file)?;
            let module = compile(&source)?;
            let loaded = lm_vm::load(module)
                .map_err(|e| format!("error: the verifier rejected the module: {e}\n"))?;
            let host = Box::new(lm_host::CliHost::new(options.rand_seed));
            let mut world = World::new(&loaded, options.config, host);
            for grant in &options.allow {
                world
                    .allow(grant)
                    .map_err(|e| format!("error: --allow: {e}\n{USAGE}\n"))?;
            }
            let outcome = world.run_root();
            print!("{}", world.dump_live(&outcome));
            Ok(ExitCode::SUCCESS)
        }
        other => Err(format!("error: unknown command `{other}`\n{USAGE}\n")),
    }
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
    let bytes = std::fs::read(path).map_err(|e| format!("error: cannot read `{path}`: {e}\n"))?;
    let text = String::from_utf8(bytes)
        .map_err(|_| format!("error[E0000]: `{path}` is not valid UTF-8\n"))?;
    Ok(SourceFile::new(path, text))
}

/// Compile one source file to decoded bytecode.
fn compile(source: &SourceFile) -> Result<lm_bytecode::Module, String> {
    let ast = lm_source::parse::parse(&source.text).map_err(|d| d.render(source))?;
    let hir = lm_hir::check_module(&ast).map_err(|d| d.render(source))?;
    Ok(lm_hir::lower_module(&hir))
}

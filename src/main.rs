use std::path::PathBuf;
use std::process::ExitCode;
use teal::loader::{self, BuildOptions, BuildReport};

const HELP: &str = "\
tlc — the Teal compiler front end

USAGE:
    tlc <command> [path] [options]

COMMANDS:
    check       Parse and type-check without writing artifacts
    build       Check and emit `.tli` interface summaries
    emit-tli    Alias for `build`
    deps        Print the package dependency graph
    clean       Remove the build cache
    inspect     Print the contents of a `.tli` summary
    help        Show this message

OPTIONS:
    --target <dir>   Artifact directory (default: <root>/target/teal)
    --jobs <n>       Limit concurrent package builds
    --json           Machine-readable output for check/build/deps
    -h, --help       Show this message

EXAMPLES:
    tlc check examples
    tlc build .
    tlc build . --target /tmp/teal-cache
";

struct Args {
    command: String,
    path: PathBuf,
    target: Option<PathBuf>,
    jobs: Option<usize>,
    json: bool,
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(command) = args.next() else {
        println!("{HELP}");
        return ExitCode::SUCCESS;
    };
    if matches!(command.as_str(), "help" | "-h" | "--help") {
        println!("{HELP}");
        return ExitCode::SUCCESS;
    }

    let mut path: Option<PathBuf> = None;
    let mut target = None;
    let mut jobs = None;
    let mut json = false;
    let mut rest = args.peekable();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--target" => {
                let Some(value) = rest.next() else {
                    eprintln!("error: --target requires a directory");
                    return ExitCode::FAILURE;
                };
                target = Some(PathBuf::from(value));
            }
            "--jobs" => {
                let Some(value) = rest.next() else {
                    eprintln!("error: --jobs requires a number");
                    return ExitCode::FAILURE;
                };
                match value.parse() {
                    Ok(value) => jobs = Some(value),
                    Err(_) => {
                        eprintln!("error: --jobs expects a positive integer");
                        return ExitCode::FAILURE;
                    }
                }
            }
            "--json" => json = true,
            "-h" | "--help" => {
                println!("{HELP}");
                return ExitCode::SUCCESS;
            }
            other if other.starts_with('-') => {
                eprintln!("error: unknown option `{other}`");
                return ExitCode::FAILURE;
            }
            other => path = Some(PathBuf::from(other)),
        }
    }

    let args = Args {
        command,
        path: path.unwrap_or_else(|| PathBuf::from(".")),
        target,
        jobs,
        json,
    };

    match args.command.as_str() {
        "check" | "build" | "emit-tli" => run_build(args),
        "deps" => run_deps(args),
        "clean" => run_clean(args),
        "inspect" => run_inspect(args),
        other => {
            eprintln!("error: unknown command `{other}`\n");
            eprintln!("{HELP}");
            ExitCode::FAILURE
        }
    }
}

fn options(write: bool, args: &Args) -> BuildOptions {
    BuildOptions {
        write,
        target: args.target.clone(),
        jobs: args.jobs,
    }
}

fn run_build(args: Args) -> ExitCode {
    let write = args.command != "check";
    match loader::build(&args.path, options(write, &args)) {
        Ok(report) => {
            if args.json {
                println!("{}", report_json(&report, write));
            } else if report.packages.is_empty() {
                println!("no Teal packages found under {}", args.path.display());
            } else {
                if write {
                    println!("artifacts: {}", report.target_dir.display());
                }
                print!("{}", report.render());
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            if args.json {
                println!("{}", error_json(&error));
            } else {
                eprint!("{}", error.render());
                eprintln!(
                    "{} error(s)",
                    error.diagnostics.iter().filter(|d| d.is_error()).count()
                );
            }
            ExitCode::FAILURE
        }
    }
}

fn run_deps(args: Args) -> ExitCode {
    match loader::build(&args.path, options(false, &args)) {
        Ok(report) => {
            if args.json {
                let entries: Vec<String> = report
                    .packages
                    .iter()
                    .map(|p| {
                        format!(
                            "{{\"name\":\"{}\",\"deps\":[{}]}}",
                            escape(&p.name),
                            p.deps
                                .iter()
                                .map(|d| format!("\"{}\"", escape(d)))
                                .collect::<Vec<_>>()
                                .join(",")
                        )
                    })
                    .collect();
                println!("[{}]", entries.join(","));
            } else {
                for package in &report.packages {
                    if package.deps.is_empty() {
                        println!("{}", package.name);
                    } else {
                        println!("{} -> {}", package.name, package.deps.join(", "));
                    }
                }
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            if args.json {
                println!("{}", error_json(&error));
            } else {
                eprint!("{}", error.render());
            }
            ExitCode::FAILURE
        }
    }
}

fn run_clean(args: Args) -> ExitCode {
    let root = if args.path.is_file() {
        args.path.parent().unwrap_or(&args.path).to_path_buf()
    } else {
        args.path.clone()
    };
    let target = args
        .target
        .unwrap_or_else(|| root.join("target").join("teal"));
    match std::fs::remove_dir_all(&target) {
        Ok(()) => {
            println!("removed {}", target.display());
            ExitCode::SUCCESS
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            println!("nothing to remove at {}", target.display());
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error: cannot remove {}: {error}", target.display());
            ExitCode::FAILURE
        }
    }
}

fn run_inspect(args: Args) -> ExitCode {
    let path = if args.path.is_file() {
        args.path.clone()
    } else {
        eprintln!("error: `inspect` expects a .tli file");
        return ExitCode::FAILURE;
    };
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("error: cannot read {}: {error}", path.display());
            return ExitCode::FAILURE;
        }
    };
    match teal::tli::Tli::decode(&bytes) {
        Ok(tli) => {
            if args.json {
                println!(
                    "{{\"package\":\"{}\",\"types\":{},\"functions\":{},\"interfaces\":{},\"impls\":{}}}",
                    escape(&tli.package),
                    tli.types.len(),
                    tli.functions.len(),
                    tli.interfaces.len(),
                    tli.impls.len()
                );
            } else {
                println!("package {}", tli.package);
                println!("  format    {}", tli.format_version);
                println!("  abi       {}", tli.abi);
                println!("  compiler  {}", tli.compiler_version);
                println!("  hash      {}", teal::hash::to_hex(&tli.content_hash));
                println!(
                    "  deps      {}",
                    tli.deps
                        .iter()
                        .map(|(n, _)| n.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                println!("  types     {}", tli.types.len());
                println!("  interfaces {}", tli.interfaces.len());
                println!("  impls     {}", tli.impls.len());
                println!("  functions {}", tli.functions.len());
                println!("  macros    {}", tli.macros.len());
                for ty in &tli.types {
                    println!("  type {} ({})", ty.name, ty.kind);
                }
                for function in &tli.functions {
                    println!("  fn {}", function.name);
                }
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error: {}: {error}", path.display());
            ExitCode::FAILURE
        }
    }
}

fn report_json(report: &BuildReport, write: bool) -> String {
    let packages: Vec<String> = report
        .packages
        .iter()
        .map(|p| {
            format!(
                "{{\"name\":\"{}\",\"files\":{},\"lines\":{},\"functions\":{},\"cached\":{},\"hash\":\"{}\",\"deps\":[{}]}}",
                escape(&p.name),
                p.files,
                p.lines,
                p.functions,
                p.cached,
                p.content_hash,
                p.deps
                    .iter()
                    .map(|d| format!("\"{}\"", escape(d)))
                    .collect::<Vec<_>>()
                    .join(",")
            )
        })
        .collect();
    format!(
        "{{\"write\":{},\"total_ms\":{},\"packages\":{},\"lines\":{},\"functions\":{},\"cached\":{},\"target\":\"{}\",\"built\":[{}]}}",
        write,
        report.total.as_millis(),
        report.packages.len(),
        report.source_lines,
        report.functions,
        report.cached,
        escape(&report.target_dir.display().to_string()),
        packages.join(",")
    )
}

fn error_json(error: &teal::CompileError) -> String {
    let diagnostics: Vec<String> = error
        .diagnostics
        .iter()
        .map(|d| {
            let file = error
                .sources
                .file(d.span.file)
                .map(|f| f.display_name())
                .unwrap_or_default();
            let (line, col) = error
                .sources
                .file(d.span.file)
                .map(|f| f.line_col(d.span.start))
                .unwrap_or((0, 0));
            format!(
                "{{\"code\":\"{}\",\"message\":\"{}\",\"file\":\"{}\",\"line\":{},\"column\":{}}}",
                d.code,
                escape(&d.message),
                escape(&file),
                line,
                col
            )
        })
        .collect();
    format!("{{\"errors\":[{}]}}", diagnostics.join(","))
}

fn escape(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

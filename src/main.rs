use std::path::PathBuf;

fn main() {
    let mut args = std::env::args().skip(1);
    let command = args.next().unwrap_or_else(|| "help".into());
    let path = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let result = match command.as_str() {
        "check" => teal::loader::check_path(&path),
        "build" | "emit-tli" => teal::loader::build_path(&path),
        "help" | "-h" | "--help" => {
            println!("tlc 0.1.0 - Teal compiler prototype\n\nUsage:\n  tlc check [path]\n  tlc build [path]\n  tlc emit-tli [path]");
            return;
        }
        _ => {
            eprintln!("unknown command: {command}");
            std::process::exit(1);
        }
    };
    match result {
        Ok(output) => {
            println!("{output}");
        }
        Err(errors) => {
            for error in errors {
                eprintln!(
                    "error at {}..{}: {}",
                    error.span.start, error.span.end, error.message
                );
            }
            std::process::exit(1);
        }
    }
}

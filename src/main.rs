use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let config_path = match parse_args() {
        Ok(path) => path,
        Err(err) => {
            eprintln!("aiterm: {err}");
            eprintln!("try `aiterm --help`");
            return ExitCode::from(2);
        }
    };

    if let Err(err) = aiterm::run(config_path) {
        eprintln!("aiterm: {err}");
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}

fn parse_args() -> Result<Option<PathBuf>, String> {
    let mut args = std::env::args().skip(1);
    let mut config = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            "-c" | "--config" => {
                let path = args
                    .next()
                    .ok_or_else(|| "missing path after --config".to_string())?;
                config = Some(PathBuf::from(path));
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown argument {other}"));
            }
            other => return Err(format!("unexpected argument {other}")),
        }
    }
    Ok(config)
}

fn print_help() {
    println!(
        "\
aiterm — a single-window GPU terminal

Usage:
  aiterm [--config <path>]

Config is TOML. When --config and $AITERM_CONFIG are both unset, aiterm
looks in the platform config directory and uses built-in defaults if
that file is missing. See config.example.toml and the README."
    );
}

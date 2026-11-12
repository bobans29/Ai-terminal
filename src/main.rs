#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args = match parse_args_from(std::env::args_os().skip(1)) {
        Ok(args) => args,
        Err(err) => {
            eprintln!("aiterm: {err}");
            eprintln!("try `aiterm --help`");
            return ExitCode::from(2);
        }
    };

    if args.help {
        print_help();
        return ExitCode::SUCCESS;
    }

    if let Err(err) = aiterm::run_with_profile(args.config, args.profile) {
        eprintln!("aiterm: {err}");
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Args {
    config: Option<PathBuf>,
    profile: Option<String>,
    help: bool,
}

fn parse_args_from<I>(args: I) -> Result<Args, String>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = args.into_iter();
    let mut parsed = Args::default();
    while let Some(arg) = args.next() {
        if arg == OsStr::new("-h") || arg == OsStr::new("--help") {
            parsed.help = true;
        } else if arg == OsStr::new("-c") || arg == OsStr::new("--config") {
            let path = args
                .next()
                .ok_or_else(|| "missing path after --config".to_string())?;
            parsed.config = Some(PathBuf::from(path));
        } else if arg == OsStr::new("-p") || arg == OsStr::new("--profile") {
            let name = args
                .next()
                .ok_or_else(|| "missing name after --profile".to_string())?;
            parsed.profile = Some(
                name.into_string()
                    .map_err(|_| "profile name must be valid UTF-8".to_string())?,
            );
        } else if arg.to_string_lossy().starts_with('-') {
            return Err(format!("unknown argument {}", arg.to_string_lossy()));
        } else {
            return Err(format!("unexpected argument {}", arg.to_string_lossy()));
        }
    }
    Ok(parsed)
}

fn print_help() {
    println!(
        "\
aiterm — a single-window GPU terminal

Usage:
  aiterm [--config <path>] [--profile <name>]

Options:
  -c, --config <path>    Read configuration from this TOML file
  -p, --profile <name>   Use this named profile for this run
  -h, --help             Show this help

Profiles can also be selected with default_profile in the configuration.
When --config and $AITERM_CONFIG are both unset, aiterm looks in the
platform config directory and uses built-in defaults if that file is missing.
See config.example.toml and the README."
    );
}

#[cfg(test)]
mod tests {
    use super::{parse_args_from, Args};
    use std::ffi::OsString;
    use std::path::PathBuf;

    fn parse(items: &[&str]) -> Result<Args, String> {
        parse_args_from(items.iter().map(OsString::from))
    }

    #[test]
    fn parses_options_in_either_order() {
        assert_eq!(
            parse(&["--config", "cfg.toml", "-p", "work"]).unwrap(),
            Args {
                config: Some(PathBuf::from("cfg.toml")),
                profile: Some("work".into()),
                help: false,
            }
        );
        assert_eq!(
            parse(&["--profile", "work", "-c", "cfg.toml"]).unwrap(),
            Args {
                config: Some(PathBuf::from("cfg.toml")),
                profile: Some("work".into()),
                help: false,
            }
        );
    }

    #[test]
    fn parses_help_and_reports_missing_or_unknown_arguments() {
        assert!(parse(&["--help"]).unwrap().help);
        assert!(parse(&["--config"]).unwrap_err().contains("missing path"));
        assert!(parse(&["-p"]).unwrap_err().contains("missing name"));
        assert!(parse(&["--wat"]).unwrap_err().contains("unknown argument"));
    }

    #[cfg(unix)]
    #[test]
    fn preserves_non_utf8_config_path() {
        use std::os::unix::ffi::OsStringExt;
        let path = OsString::from_vec(vec![b'c', b'f', b'g', 0xff]);
        let args = parse_args_from([OsString::from("--config"), path.clone()]).unwrap();
        assert_eq!(args.config, Some(PathBuf::from(path)));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_non_utf8_profile_name() {
        use std::os::unix::ffi::OsStringExt;
        let name = OsString::from_vec(vec![0xff]);
        let err = parse_args_from([OsString::from("--profile"), name]).unwrap_err();
        assert!(err.contains("profile name must be valid UTF-8"));
    }
}

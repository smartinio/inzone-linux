use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use inzone_buds::{
    BatteryCell, BatteryReading, DEFAULT_TIMEOUT, Error, QueryResult, discover_device,
    query_battery,
};

const HELP: &str = "\
Native Sony INZONE Buds battery status for Linux

Usage: inzone-buds [OPTIONS]

Options:
      --device PATH  Use a specific verified hidraw device
      --json         Print machine-readable JSON
      --raw          Print the raw HID response to stderr
  -h, --help         Print help
  -V, --version      Print version
";

#[derive(Default)]
struct Options {
    device: Option<PathBuf>,
    json: bool,
    raw: bool,
}

enum Command {
    Query(Options),
    Help,
    Version,
}

fn main() -> ExitCode {
    let (exit_code, stdout, stderr) =
        application(env::args_os().skip(1).collect(), discover_device, query);
    print!("{stdout}");
    eprint!("{stderr}");
    exit_code
}

fn query(path: &Path) -> Result<QueryResult, Error> {
    query_battery(path, DEFAULT_TIMEOUT)
}

fn application(
    args: Vec<OsString>,
    discover: fn() -> Result<PathBuf, Error>,
    query: fn(&Path) -> Result<QueryResult, Error>,
) -> (ExitCode, String, String) {
    match parse_args_from(args) {
        Ok(Command::Help) => (ExitCode::SUCCESS, HELP.into(), String::new()),
        Ok(Command::Version) => (
            ExitCode::SUCCESS,
            format!("inzone-buds {}\n", env!("CARGO_PKG_VERSION")),
            String::new(),
        ),
        Ok(Command::Query(options)) => match execute(options, discover, query) {
            Ok((stdout, stderr)) => (ExitCode::SUCCESS, stdout, stderr),
            Err(message) => (
                ExitCode::FAILURE,
                String::new(),
                format!("error: {message}\n"),
            ),
        },
        Err(message) => (
            ExitCode::FAILURE,
            String::new(),
            format!("error: {message}\n"),
        ),
    }
}

fn execute(
    options: Options,
    discover: fn() -> Result<PathBuf, Error>,
    query: fn(&Path) -> Result<QueryResult, Error>,
) -> Result<(String, String), String> {
    let device = match options.device {
        Some(path) => path,
        None => discover().map_err(|error| error.to_string())?,
    };
    let result = query(&device).map_err(|error| error.to_string())?;

    let stderr = if options.raw {
        let bytes = result
            .raw_response
            .iter()
            .map(|byte| format!(" {byte:02x}"))
            .collect::<String>();
        format!("raw:{bytes}\n")
    } else {
        String::new()
    };

    let stdout = if options.json {
        format!(
            "{}\n",
            reading_json(&device.to_string_lossy(), result.reading)
        )
    } else {
        format!(
            "Sony INZONE Buds ({})\nLeft:  {}\nRight: {}\nCase:  {}\n",
            device.display(),
            result.reading.left,
            result.reading.right,
            result.reading.case
        )
    };
    Ok((stdout, stderr))
}

fn parse_args_from(arguments: Vec<OsString>) -> Result<Command, String> {
    let mut options = Options::default();
    let mut args = arguments.into_iter();

    while let Some(argument) = args.next() {
        match argument.to_str() {
            Some("--device") => {
                let path = args
                    .next()
                    .ok_or_else(|| "--device requires a path".to_string())?;
                options.device = Some(PathBuf::from(path));
            }
            Some("--json") => options.json = true,
            Some("--raw") => options.raw = true,
            Some("-h" | "--help") => return Ok(Command::Help),
            Some("-V" | "--version") => return Ok(Command::Version),
            Some(value) => return Err(format!("unknown option: {value}\n\n{HELP}")),
            None => return Err("arguments must be valid UTF-8".into()),
        }
    }
    Ok(Command::Query(options))
}

fn reading_json(device: &str, reading: BatteryReading) -> String {
    format!(
        "{{\"device\":\"{}\",\"left\":{},\"right\":{},\"case\":{}}}",
        json_escape(device),
        cell_json(reading.left),
        cell_json(reading.right),
        cell_json(reading.case)
    )
}

fn cell_json(cell: BatteryCell) -> String {
    let percent = cell
        .percent
        .map_or_else(|| "null".to_string(), |value| value.to_string());
    format!(
        "{{\"percent\":{percent},\"state\":\"{}\"}}",
        cell.state.as_str()
    )
}

fn json_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            character if character.is_control() => {
                use std::fmt::Write;
                write!(escaped, "\\u{:04x}", character as u32)
                    .expect("writing to String cannot fail");
            }
            character => escaped.push(character),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;
    use inzone_buds::BatteryState;
    use std::os::unix::ffi::OsStringExt;
    use std::time::Duration;

    fn reading() -> BatteryReading {
        BatteryReading {
            left: BatteryCell {
                percent: Some(54),
                state: BatteryState::Discharging,
            },
            right: BatteryCell {
                percent: Some(56),
                state: BatteryState::Charging,
            },
            case: BatteryCell {
                percent: None,
                state: BatteryState::Unavailable,
            },
        }
    }

    fn result() -> QueryResult {
        QueryResult {
            reading: reading(),
            raw_response: vec![0x02, 0x12, 0xab],
        }
    }

    fn discovered() -> Result<PathBuf, Error> {
        Ok(PathBuf::from("/dev/hidraw3"))
    }

    fn queried(_: &Path) -> Result<QueryResult, Error> {
        Ok(result())
    }

    fn timed_out(_: &Path) -> Result<QueryResult, Error> {
        Err(Error::Timeout(Duration::from_secs(3)))
    }

    #[test]
    fn formats_machine_and_human_output() {
        assert_eq!(
            reading_json("/dev/hidraw3", reading()),
            r#"{"device":"/dev/hidraw3","left":{"percent":54,"state":"discharging"},"right":{"percent":56,"state":"charging"},"case":{"percent":null,"state":"unavailable"}}"#
        );

        let options = Options {
            device: Some(PathBuf::from("/dev/hidraw3")),
            json: false,
            raw: false,
        };
        let (stdout, stderr) = execute(options, discovered, queried).unwrap();
        assert!(stdout.contains("Left:  54% (discharging)"));
        assert!(stdout.contains("Case:  unknown (unavailable)"));
        assert!(stderr.is_empty());

        let options = Options {
            device: None,
            json: true,
            raw: true,
        };
        let (stdout, stderr) = execute(options, discovered, queried).unwrap();
        assert!(stdout.starts_with("{\"device\""));
        assert_eq!(stderr, "raw: 02 12 ab\n");
    }

    #[test]
    fn reports_discovery_and_query_errors() {
        let error =
            execute(Options::default(), || Err(Error::DeviceNotFound), queried).unwrap_err();
        assert!(error.contains("was not found"));

        let error = execute(
            Options {
                device: Some(PathBuf::from("/dev/hidraw3")),
                ..Options::default()
            },
            discovered,
            timed_out,
        )
        .unwrap_err();
        assert!(error.contains("3 seconds"));
    }

    #[test]
    fn parses_every_argument_form() {
        assert!(matches!(parse_args_from(vec![]), Ok(Command::Query(_))));
        assert!(matches!(
            parse_args_from(vec!["-h".into()]),
            Ok(Command::Help)
        ));
        assert!(matches!(
            parse_args_from(vec!["--help".into()]),
            Ok(Command::Help)
        ));
        assert!(matches!(
            parse_args_from(vec!["-V".into()]),
            Ok(Command::Version)
        ));
        assert!(matches!(
            parse_args_from(vec!["--version".into()]),
            Ok(Command::Version)
        ));

        let command = parse_args_from(vec![
            "--device".into(),
            "/dev/hidraw3".into(),
            "--json".into(),
            "--raw".into(),
        ])
        .unwrap();
        assert!(matches!(
            command,
            Command::Query(Options {
                device: Some(_),
                json: true,
                raw: true
            })
        ));

        assert!(parse_args_from(vec!["--device".into()]).is_err());
        assert!(parse_args_from(vec!["--unknown".into()]).is_err());
        assert!(parse_args_from(vec![OsString::from_vec(vec![0xff])]).is_err());
    }

    #[test]
    fn application_returns_every_exit_shape() {
        let (code, stdout, stderr) = application(vec!["--help".into()], discovered, queried);
        assert_eq!(code, ExitCode::SUCCESS);
        assert_eq!(stdout, HELP);
        assert!(stderr.is_empty());

        let (code, stdout, stderr) = application(vec!["--version".into()], discovered, queried);
        assert_eq!(code, ExitCode::SUCCESS);
        assert!(stdout.contains(env!("CARGO_PKG_VERSION")));
        assert!(stderr.is_empty());

        let (code, stdout, stderr) = application(
            vec!["--device".into(), "/dev/hidraw3".into()],
            discovered,
            queried,
        );
        assert_eq!(code, ExitCode::SUCCESS);
        assert!(stdout.contains("Sony INZONE Buds"));
        assert!(stderr.is_empty());

        let (code, stdout, stderr) = application(vec![], || Err(Error::DeviceNotFound), queried);
        assert_eq!(code, ExitCode::FAILURE);
        assert!(stdout.is_empty());
        assert!(stderr.starts_with("error:"));

        let (code, stdout, stderr) = application(vec!["bad".into()], discovered, queried);
        assert_eq!(code, ExitCode::FAILURE);
        assert!(stdout.is_empty());
        assert!(stderr.contains("unknown option"));
    }

    #[test]
    fn escapes_every_json_character_class() {
        assert_eq!(
            json_escape("\"\\\n\r\t\u{0008}é"),
            "\\\"\\\\\\n\\r\\t\\u0008é"
        );
        assert!(query(Path::new("/tmp/hidraw3")).is_err());
    }
}

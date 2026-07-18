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
#[path = "../tests/unit/main_tests.rs"]
mod tests;

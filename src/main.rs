use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

use inzone_buds::{BatteryCell, BatteryReading, DEFAULT_TIMEOUT, discover_device, query_battery};

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

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}
fn run() -> Result<(), String> {
    let Some(options) = parse_args()? else {
        return Ok(());
    };
    let device = match options.device {
        Some(path) => path,
        None => discover_device().map_err(|error| error.to_string())?,
    };
    let result = query_battery(&device, DEFAULT_TIMEOUT).map_err(|error| error.to_string())?;

    if options.raw {
        eprint!("raw:");
        for byte in &result.raw_response {
            eprint!(" {byte:02x}");
        }
        eprintln!();
    }

    if options.json {
        println!(
            "{}",
            reading_json(&device.to_string_lossy(), result.reading)
        );
    } else {
        println!("Sony INZONE Buds ({})", device.display());
        println!("Left:  {}", result.reading.left);
        println!("Right: {}", result.reading.right);
        println!("Case:  {}", result.reading.case);
    }
    Ok(())
}

fn parse_args() -> Result<Option<Options>, String> {
    let mut options = Options::default();
    let mut args = env::args_os().skip(1);

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
            Some("-h" | "--help") => {
                print!("{HELP}");
                return Ok(None);
            }
            Some("-V" | "--version") => {
                println!("inzone-buds {}", env!("CARGO_PKG_VERSION"));
                return Ok(None);
            }
            Some(value) => return Err(format!("unknown option: {value}\n\n{HELP}")),
            None => return Err("arguments must be valid UTF-8".into()),
        }
    }
    Ok(Some(options))
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

    #[test]
    fn formats_machine_readable_output() {
        let reading = BatteryReading {
            left: BatteryCell {
                percent: Some(54),
                state: BatteryState::Discharging,
            },
            right: BatteryCell {
                percent: Some(56),
                state: BatteryState::Discharging,
            },
            case: BatteryCell {
                percent: Some(90),
                state: BatteryState::Unavailable,
            },
        };
        assert_eq!(
            reading_json("/dev/hidraw3", reading),
            r#"{"device":"/dev/hidraw3","left":{"percent":54,"state":"discharging"},"right":{"percent":56,"state":"discharging"},"case":{"percent":90,"state":"unavailable"}}"#
        );
    }
}

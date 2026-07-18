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
    let error = execute(Options::default(), || Err(Error::DeviceNotFound), queried).unwrap_err();
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

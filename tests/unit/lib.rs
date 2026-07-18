use super::*;
use std::collections::VecDeque;
use std::error::Error as _;
use std::os::unix::fs::symlink;
use std::sync::atomic::AtomicUsize;

const CAPTURED_RESPONSE: [u8; 64] = [
    0x02, 0x12, 0x04, 0xff, 0x0f, 0x00, 0x96, 0xc3, 0x14, 0x04, 0x10, 0x01, 0x00, 0x00, 0x36, 0x00,
    0x38, 0xff, 0x5a, 0x49, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

static TEMP_ID: AtomicUsize = AtomicUsize::new(0);

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let id = TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("inzone-buds-test-{}-{id}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn write_interface(node: &Path, uevent: &str, interface: &str, descriptor: &[u8]) {
    fs::create_dir_all(node.join("device")).unwrap();
    fs::write(node.join("device/uevent"), uevent).unwrap();
    fs::write(node.join("bInterfaceNumber"), interface).unwrap();
    fs::write(node.join("device/report_descriptor"), descriptor).unwrap();
}

fn valid_descriptor() -> Vec<u8> {
    let mut descriptor = vec![0xaa, 0xbb];
    descriptor.extend_from_slice(EXPECTED_REPORT_COLLECTION);
    descriptor.push(0xcc);
    descriptor
}

fn response_for(transaction: u16) -> [u8; 64] {
    let mut response = CAPTURED_RESPONSE;
    response[11..13].copy_from_slice(&transaction.to_le_bytes());
    fix_checksum(&mut response);
    response
}

fn fix_checksum(response: &mut [u8; 64]) {
    response[19] = response[5..19]
        .iter()
        .fold(0_u8, |sum, byte| sum.wrapping_add(*byte));
}

enum IoAction {
    Data(Vec<u8>),
    Length(usize),
    Error(io::ErrorKind),
    DelayedError(Duration, io::ErrorKind),
}

struct ScriptedDevice {
    reads: VecDeque<IoAction>,
    writes: VecDeque<IoAction>,
    written: Vec<u8>,
}

impl ScriptedDevice {
    fn new(reads: Vec<IoAction>, writes: Vec<IoAction>) -> Self {
        Self {
            reads: reads.into(),
            writes: writes.into(),
            written: Vec::new(),
        }
    }
}

impl Read for ScriptedDevice {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        match self.reads.pop_front().unwrap_or(IoAction::Length(0)) {
            IoAction::Data(data) => {
                let length = data.len().min(buffer.len());
                buffer[..length].copy_from_slice(&data[..length]);
                Ok(length)
            }
            IoAction::Length(length) => Ok(length),
            IoAction::Error(kind) => Err(io::Error::from(kind)),
            IoAction::DelayedError(delay, kind) => {
                thread::sleep(delay);
                Err(io::Error::from(kind))
            }
        }
    }
}

impl Write for ScriptedDevice {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        match self
            .writes
            .pop_front()
            .unwrap_or(IoAction::Length(buffer.len()))
        {
            IoAction::Length(length) => {
                self.written
                    .extend_from_slice(&buffer[..length.min(buffer.len())]);
                Ok(length)
            }
            IoAction::Error(kind) => Err(io::Error::from(kind)),
            IoAction::DelayedError(delay, kind) => {
                thread::sleep(delay);
                Err(io::Error::from(kind))
            }
            IoAction::Data(data) => {
                let length = data.len().min(buffer.len());
                self.written.extend_from_slice(&buffer[..length]);
                Ok(length)
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn creates_known_battery_request() {
    let request = battery_request(1);
    assert_eq!(
        &request[..14],
        &[
            0x02, 0x0c, 0x01, 0x00, 0xfc, 0x08, 0x96, 0xc3, 0x41, 0x04, 0x01, 0x01, 0x00, 0xa0
        ]
    );
    assert!(request[14..].iter().all(|byte| *byte == 0));
}

#[test]
fn parses_captured_battery_response() {
    let reading = parse_battery_response(&CAPTURED_RESPONSE, 1).unwrap();
    assert_eq!(reading.left.percent, Some(54));
    assert_eq!(reading.left.state, BatteryState::Discharging);
    assert_eq!(reading.right.percent, Some(56));
    assert_eq!(reading.right.state, BatteryState::Discharging);
    assert_eq!(reading.case.percent, Some(90));
    assert_eq!(reading.case.state, BatteryState::Unavailable);
    assert_eq!(reading.case.to_string(), "90%");
}

#[test]
fn rejects_bad_checksum() {
    let mut response = CAPTURED_RESPONSE;
    response[19] ^= 1;
    let error = parse_battery_response(&response, 1).unwrap_err();
    assert!(error.to_string().contains("checksum"));
}

#[test]
fn rejects_wrong_transaction() {
    let error = parse_battery_response(&CAPTURED_RESPONSE, 2).unwrap_err();
    assert!(error.to_string().contains("transaction"));
}

#[test]
fn rejects_undocumented_frame_shapes() {
    let mut response = CAPTURED_RESPONSE;
    response[1] = 19;
    let error = parse_battery_response(&response, 1).unwrap_err();
    assert!(error.to_string().contains("frame length"));

    let mut response = CAPTURED_RESPONSE;
    response[4] = 14;
    let error = parse_battery_response(&response, 1).unwrap_err();
    assert!(error.to_string().contains("event header"));
}

#[test]
fn rejects_paths_not_bound_to_dev_hidraw() {
    let root = Path::new("/dev");
    assert!(validate_device_path_in(Path::new("/dev/hidraw3"), root).is_ok());
    assert!(validate_device_path_in(Path::new("/"), root).is_err());
    assert!(validate_device_path_in(Path::new("/dev/nope"), root).is_err());
    assert!(validate_device_path_in(Path::new("/tmp/hidraw3"), root).is_err());
    assert!(validate_device_path_in(Path::new("/dev/hidrawx"), root).is_err());
    assert!(validate_device_path_in(Path::new("/dev/hidraw3/../hidraw3"), root).is_err());
}

#[test]
fn decodes_linux_device_numbers() {
    assert_eq!(linux_device_major(0xf303), 243);
    assert_eq!(linux_device_minor(0xf303), 3);
}

#[test]
fn formats_all_public_states_and_errors() {
    let states = [
        (0, BatteryState::Discharging, "discharging"),
        (1, BatteryState::Charging, "charging"),
        (2, BatteryState::Error, "error"),
        (0xff, BatteryState::Unavailable, "unavailable"),
    ];
    for (raw, state, text) in states {
        assert_eq!(BatteryState::from_byte(raw), state);
        assert_eq!(state.as_str(), text);
        assert_eq!(state.to_string(), text);
    }
    let unknown = BatteryState::from_byte(7);
    assert_eq!(unknown, BatteryState::Unknown(7));
    assert_eq!(unknown.as_str(), "unknown");
    assert_eq!(unknown.to_string(), "unknown (0x07)");

    assert_eq!(BatteryCell::from_bytes(1, 42).to_string(), "42% (charging)");
    assert_eq!(
        BatteryCell::from_bytes(2, 101).to_string(),
        "unknown (error)"
    );

    let paths = vec![PathBuf::from("/dev/hidraw2"), PathBuf::from("/dev/hidraw3")];
    let errors = [
        Error::DeviceNotFound.to_string(),
        Error::AmbiguousDevices(paths).to_string(),
        Error::DeviceMismatch(PathBuf::from("/tmp/nope")).to_string(),
        Error::Timeout(Duration::from_secs(2)).to_string(),
        Error::InvalidResponse("bad".into()).to_string(),
    ];
    assert!(errors[0].contains("054c:0ec2"));
    assert!(errors[1].contains("hidraw2, /dev/hidraw3"));
    assert!(errors[2].contains("/tmp/nope"));
    assert!(errors[3].contains("2 seconds"));
    assert!(errors[4].contains("bad"));

    let source = io::Error::new(io::ErrorKind::PermissionDenied, "denied");
    let error = io_error("open", source);
    assert_eq!(error.to_string(), "open: denied");
    assert_eq!(error.source().unwrap().to_string(), "denied");
    assert!(Error::DeviceNotFound.source().is_none());
}

#[test]
fn discovers_only_exact_interfaces_and_fails_closed() {
    let temp = TestDir::new();
    let class = temp.0.join("class");
    let devices = temp.0.join("dev");
    fs::create_dir_all(&class).unwrap();

    assert!(matches!(
        discover_device_in(&temp.0.join("missing"), &devices),
        Err(Error::Io { .. })
    ));
    assert!(matches!(
        discover_device_in(&class, &devices),
        Err(Error::DeviceNotFound)
    ));

    let node = class.join("hidraw9");
    write_interface(
        &node,
        "HID_ID=0003:0000054C:00000EC2\n",
        "05\n",
        &valid_descriptor(),
    );
    assert_eq!(
        discover_device_in(&class, &devices).unwrap(),
        devices.join("hidraw9")
    );

    let second = class.join("hidraw10");
    write_interface(
        &second,
        "hid_id=0003:0000054c:00000ec2\n",
        "05",
        &valid_descriptor(),
    );
    let error = discover_device_in(&class, &devices).unwrap_err();
    assert!(matches!(error, Error::AmbiguousDevices(_)));

    let mut broken = vec![Err(io::Error::from(io::ErrorKind::Other))].into_iter();
    assert!(matches!(
        discover_device_paths(&mut broken, &devices),
        Err(Error::Io { .. })
    ));
}

#[test]
fn rejects_incomplete_or_wrong_sysfs_interfaces() {
    let temp = TestDir::new();
    let node = temp.0.join("hidraw0");
    assert!(!matches_expected_interface(&node));

    fs::create_dir_all(node.join("device")).unwrap();
    fs::write(
        node.join("device/uevent"),
        "HID_ID=0003:0000054C:00000EC2\n",
    )
    .unwrap();
    assert!(!matches_expected_interface(&node));
    fs::write(node.join("bInterfaceNumber"), "05").unwrap();
    assert!(!matches_expected_interface(&node));
    fs::write(node.join("device/report_descriptor"), valid_descriptor()).unwrap();
    assert!(matches_expected_interface(&node));

    fs::write(
        node.join("device/uevent"),
        "HID_ID=0003:00000001:00000002\n",
    )
    .unwrap();
    assert!(!matches_expected_interface(&node));
    fs::write(
        node.join("device/uevent"),
        "HID_ID=0003:0000054C:00000EC2\n",
    )
    .unwrap();
    fs::write(node.join("bInterfaceNumber"), "xx").unwrap();
    assert!(!matches_expected_interface(&node));
    fs::write(node.join("bInterfaceNumber"), "04").unwrap();
    assert!(!matches_expected_interface(&node));
    fs::write(node.join("bInterfaceNumber"), "05").unwrap();
    fs::write(node.join("device/report_descriptor"), [1, 2, 3]).unwrap();
    assert!(!matches_expected_interface(&node));
    assert_eq!(matching_device_path(&node, Path::new("/dev")), None);
    assert!(!is_inzone_uevent(""));
}

#[test]
fn validates_the_exact_opened_character_device() {
    let temp = TestDir::new();
    let device_root = temp.0.join("dev");
    let sys_root = temp.0.join("sys");
    let path = device_root.join("hidraw7");
    fs::create_dir_all(&device_root).unwrap();

    let denied = open_verified_device_with(
        &path,
        &device_root,
        &sys_root,
        Box::new(|_| Err(io::Error::from(io::ErrorKind::PermissionDenied))),
    )
    .unwrap_err();
    assert!(matches!(denied, Error::Io { .. }));

    let regular = temp.0.join("regular");
    fs::write(&regular, b"data").unwrap();
    let mismatch = open_verified_device_with(
        &path,
        &device_root,
        &sys_root,
        Box::new(|_| File::open(&regular)),
    )
    .unwrap_err();
    assert!(matches!(mismatch, Error::DeviceMismatch(_)));

    let null = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/null")
        .unwrap();
    let rdev = null.metadata().unwrap().rdev();
    let node = sys_root.join(format!(
        "{}:{}",
        linux_device_major(rdev),
        linux_device_minor(rdev)
    ));
    write_interface(
        &node,
        "HID_ID=0003:0000054C:00000EC2\n",
        "05",
        &valid_descriptor(),
    );
    let opened = open_verified_device_with(
        &path,
        &device_root,
        &sys_root,
        Box::new(|_| OpenOptions::new().read(true).write(true).open("/dev/null")),
    )
    .unwrap();
    assert!(opened.metadata().unwrap().file_type().is_char_device());

    fs::write(node.join("bInterfaceNumber"), "04").unwrap();
    let mismatch = open_verified_device_with(
        &path,
        &device_root,
        &sys_root,
        Box::new(|_| OpenOptions::new().read(true).write(true).open("/dev/null")),
    )
    .unwrap_err();
    assert!(matches!(mismatch, Error::DeviceMismatch(_)));

    let regular = File::open(&regular).unwrap();
    assert!(matches!(
        inspect_opened_device(
            &path,
            &sys_root,
            regular,
            Err(io::Error::from(io::ErrorKind::Other))
        ),
        Err(Error::Io { .. })
    ));

    let target = temp.0.join("target");
    let link = temp.0.join("link");
    fs::write(&target, b"data").unwrap();
    symlink(&target, &link).unwrap();
    assert_eq!(
        open_hid_device(&link).unwrap_err().raw_os_error(),
        Some(libc::ELOOP)
    );
}

#[test]
fn parses_every_rejected_response_header() {
    let cases: Vec<Vec<u8>> = vec![
        vec![],
        vec![REPORT_ID],
        {
            let mut value = CAPTURED_RESPONSE;
            value[0] = 9;
            value.to_vec()
        },
        {
            let mut value = CAPTURED_RESPONSE;
            value[1] = 17;
            value.to_vec()
        },
        CAPTURED_RESPONSE[..19].to_vec(),
    ];
    for case in cases {
        assert!(parse_battery_response(&case, 1).is_err());
    }

    for (offset, value) in [(2, 3), (3, 0), (4, 14), (5, 1)] {
        let mut response = CAPTURED_RESPONSE;
        response[offset] = value;
        fix_checksum(&mut response);
        assert!(
            parse_battery_response(&response, 1)
                .unwrap_err()
                .to_string()
                .contains("event header")
        );
    }
    for (offset, value) in [(6, 0), (7, 0)] {
        let mut response = CAPTURED_RESPONSE;
        response[offset] = value;
        fix_checksum(&mut response);
        assert!(
            parse_battery_response(&response, 1)
                .unwrap_err()
                .to_string()
                .contains("Sony key")
        );
    }
    for (offset, value) in [(8, 0), (9, 0), (10, 0)] {
        let mut response = CAPTURED_RESPONSE;
        response[offset] = value;
        fix_checksum(&mut response);
        assert!(
            parse_battery_response(&response, 1)
                .unwrap_err()
                .to_string()
                .contains("battery return event")
        );
    }
}

#[test]
fn recognizes_only_the_matching_battery_event_shape() {
    assert!(looks_like_matching_battery_event(&CAPTURED_RESPONSE, 1));
    assert!(!looks_like_matching_battery_event(
        &CAPTURED_RESPONSE[..12],
        1
    ));
    for (offset, value) in [
        (0, 0),
        (2, 0),
        (3, 0),
        (6, 0),
        (7, 0),
        (8, 0),
        (9, 0),
        (10, 0),
        (11, 2),
    ] {
        let mut response = CAPTURED_RESPONSE;
        response[offset] = value;
        assert!(!looks_like_matching_battery_event(&response, 1));
    }
}

#[test]
fn queries_through_a_scripted_transport() {
    let response = response_for(77);
    let result = query_battery_with_transaction(
        Path::new("/ignored"),
        Duration::from_secs(1),
        77,
        Box::new(|_| {
            Ok(Box::new(ScriptedDevice::new(
                vec![IoAction::Data(response.to_vec())],
                vec![],
            )) as Box<dyn HidDevice>)
        }),
    )
    .unwrap();
    assert_eq!(result.reading.left.percent, Some(54));
    assert_eq!(result.raw_response, response[..20]);

    let write_error = query_battery_with_transaction(
        Path::new("/ignored"),
        Duration::from_secs(1),
        77,
        Box::new(|_| {
            Ok(Box::new(ScriptedDevice::new(
                vec![],
                vec![IoAction::Error(io::ErrorKind::BrokenPipe)],
            )) as Box<dyn HidDevice>)
        }),
    )
    .unwrap_err();
    assert!(matches!(
        write_error,
        Error::Io {
            operation: "send battery GET report",
            ..
        }
    ));

    let read_error = query_battery_with_transaction(
        Path::new("/ignored"),
        Duration::from_secs(1),
        77,
        Box::new(|_| {
            Ok(Box::new(ScriptedDevice::new(
                vec![IoAction::Error(io::ErrorKind::BrokenPipe)],
                vec![],
            )) as Box<dyn HidDevice>)
        }),
    )
    .unwrap_err();
    assert!(matches!(
        read_error,
        Error::Io {
            operation: "read battery response",
            ..
        }
    ));

    let error = query_battery_with(
        Path::new("/ignored"),
        Duration::from_secs(1),
        Box::new(|_| Err(Error::DeviceNotFound)),
    )
    .unwrap_err();
    assert!(matches!(error, Error::DeviceNotFound));
    assert!(query_battery(Path::new("/tmp/hidraw3"), Duration::ZERO).is_err());
}

#[test]
fn handles_every_write_outcome() {
    let request = battery_request(1);
    let mut interrupted = ScriptedDevice::new(
        vec![],
        vec![
            IoAction::Error(io::ErrorKind::Interrupted),
            IoAction::Length(64),
        ],
    );
    write_request(
        &mut interrupted,
        &request,
        Instant::now(),
        Duration::from_secs(1),
    )
    .unwrap();
    interrupted.flush().unwrap();
    assert_eq!(interrupted.written, request);

    let mut data_action = ScriptedDevice::new(vec![], vec![IoAction::Data(vec![0; 64])]);
    write_request(
        &mut data_action,
        &request,
        Instant::now(),
        Duration::from_secs(1),
    )
    .unwrap();

    let mut short = ScriptedDevice::new(vec![], vec![IoAction::Length(3)]);
    assert!(write_request(&mut short, &request, Instant::now(), Duration::from_secs(1)).is_err());

    let mut failed = ScriptedDevice::new(vec![], vec![IoAction::Error(io::ErrorKind::BrokenPipe)]);
    assert!(
        write_request(
            &mut failed,
            &request,
            Instant::now(),
            Duration::from_secs(1)
        )
        .is_err()
    );

    let mut blocked = ScriptedDevice::new(vec![], vec![IoAction::Error(io::ErrorKind::WouldBlock)]);
    assert!(
        write_request(
            &mut blocked,
            &request,
            Instant::now(),
            Duration::from_millis(1)
        )
        .is_err()
    );
    let mut expired_during_write = ScriptedDevice::new(
        vec![],
        vec![IoAction::DelayedError(
            Duration::from_millis(2),
            io::ErrorKind::WouldBlock,
        )],
    );
    let error = write_request(
        &mut expired_during_write,
        &request,
        Instant::now(),
        Duration::from_millis(1),
    );
    assert!(matches!(
        error,
        Err(Error::Timeout(duration)) if duration == Duration::from_millis(1)
    ));
    let mut unused = ScriptedDevice::new(vec![], vec![]);
    assert!(write_request(&mut unused, &request, Instant::now(), Duration::ZERO).is_err());
}

#[test]
fn handles_every_read_outcome() {
    let valid = response_for(9);
    let unrelated = vec![0; 64];
    let mut device = ScriptedDevice::new(
        vec![
            IoAction::Error(io::ErrorKind::Interrupted),
            IoAction::Data(unrelated),
            IoAction::Data(valid.to_vec()),
        ],
        vec![],
    );
    let (reading, raw) =
        read_response(&mut device, 9, Instant::now(), Duration::from_secs(1)).unwrap();
    assert_eq!(reading.right.percent, Some(56));
    assert_eq!(raw.len(), 20);

    let mut malformed = valid;
    malformed[19] ^= 1;
    let mut device = ScriptedDevice::new(vec![IoAction::Data(malformed.to_vec())], vec![]);
    assert!(read_response(&mut device, 9, Instant::now(), Duration::from_secs(1)).is_err());

    for action in [
        IoAction::Length(0),
        IoAction::Error(io::ErrorKind::BrokenPipe),
        IoAction::Error(io::ErrorKind::WouldBlock),
    ] {
        let timeout = if matches!(action, IoAction::Error(io::ErrorKind::WouldBlock)) {
            Duration::from_millis(1)
        } else {
            Duration::from_secs(1)
        };
        let mut device = ScriptedDevice::new(vec![action], vec![]);
        assert!(read_response(&mut device, 9, Instant::now(), timeout).is_err());
    }
    let mut expired_during_read = ScriptedDevice::new(
        vec![IoAction::DelayedError(
            Duration::from_millis(2),
            io::ErrorKind::WouldBlock,
        )],
        vec![],
    );
    let error = read_response(
        &mut expired_during_read,
        9,
        Instant::now(),
        Duration::from_millis(1),
    );
    assert!(matches!(
        error,
        Err(Error::Timeout(duration)) if duration == Duration::from_millis(1)
    ));
    let mut device = ScriptedDevice::new(vec![], vec![]);
    assert!(read_response(&mut device, 9, Instant::now(), Duration::ZERO).is_err());
}

#[test]
fn deadline_helpers_cover_wait_and_expiry() {
    let started = Instant::now();
    pause_until_retry(started, Duration::from_millis(20)).unwrap();
    assert!(ensure_before_deadline(started, Duration::from_secs(1)).is_ok());
    assert!(pause_until_retry(started, Duration::ZERO).is_err());
    assert!(ensure_before_deadline(started, Duration::ZERO).is_err());
}

#[test]
fn production_adapters_fail_safely_without_opening_a_device() {
    let _ = discover_device();
    assert!(open_verified_device(Path::new("/tmp/inzone-buds-coverage-no-device")).is_err());
    let mut null = box_file_device(
        OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/null")
            .unwrap(),
    );
    assert_eq!(null.write(&[0]).unwrap(), 1);
}

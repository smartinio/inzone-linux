//! Minimal access to Sony INZONE Buds battery status.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU16, Ordering};
use std::thread;
use std::time::{Duration, Instant};

pub const USB_VENDOR_ID: u16 = 0x054c;
pub const USB_PRODUCT_ID: u16 = 0x0ec2;
pub const USB_INTERFACE_NUMBER: u8 = 5;
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(3);

const REPORT_ID: u8 = 0x02;
const REPORT_SIZE: usize = 64;
const SONY_KEY_LOW: u8 = 0x96;
const SONY_KEY_HIGH: u8 = 0xc3;
const BATTERY_EVENT_ID: u8 = 0x04;
const EVENT_TYPE_GET: u8 = 0x01;
const EVENT_TYPE_RETURN: u8 = 0x10;
const COMMAND_PACKET_TYPE: u8 = 0x01;
const EVENT_PACKET_TYPE: u8 = 0x04;
const EXPECTED_FRAME_LENGTH: usize = 18;
const EXPECTED_EVENT_PARAMETER_LENGTH: u8 = 0x0f;
const RETRY_INTERVAL: Duration = Duration::from_millis(10);
const EXPECTED_REPORT_COLLECTION: &[u8] = &[
    0x06, 0x04, 0xff, 0x09, 0x01, 0xa1, 0x01, 0x15, 0x00, 0x26, 0xff, 0x00, 0x85, 0x02, 0x75, 0x08,
    0x95, 0x3f, 0x09, 0x02, 0x81, 0x02, 0x09, 0x03, 0x91, 0x02, 0xc0,
];
static NEXT_TRANSACTION: AtomicU16 = AtomicU16::new(1);

#[derive(Debug)]
pub enum Error {
    DeviceNotFound,
    AmbiguousDevices(Vec<PathBuf>),
    DeviceMismatch(PathBuf),
    Io {
        operation: &'static str,
        source: io::Error,
    },
    Timeout(Duration),
    InvalidResponse(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DeviceNotFound => write!(
                f,
                "Sony INZONE Buds receiver {:04x}:{:04x} was not found",
                USB_VENDOR_ID, USB_PRODUCT_ID
            ),
            Self::AmbiguousDevices(paths) => write!(
                f,
                "multiple matching INZONE battery interfaces found: {}",
                paths
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::DeviceMismatch(path) => write!(
                f,
                "refusing to send a Sony HID request to unverified device {}",
                path.display()
            ),
            Self::Io { operation, source } => write!(f, "{operation}: {source}"),
            Self::Timeout(duration) => {
                write!(
                    f,
                    "no battery response within {} seconds",
                    duration.as_secs()
                )
            }
            Self::InvalidResponse(reason) => write!(f, "invalid battery response: {reason}"),
        }
    }
}
impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

fn io_error(operation: &'static str, source: io::Error) -> Error {
    Error::Io { operation, source }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BatteryState {
    Discharging,
    Charging,
    Error,
    Unavailable,
    Unknown(u8),
}

impl BatteryState {
    pub fn from_byte(value: u8) -> Self {
        match value {
            0 => Self::Discharging,
            1 => Self::Charging,
            2 => Self::Error,
            0xff => Self::Unavailable,
            other => Self::Unknown(other),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Discharging => "discharging",
            Self::Charging => "charging",
            Self::Error => "error",
            Self::Unavailable => "unavailable",
            Self::Unknown(_) => "unknown",
        }
    }
}

impl fmt::Display for BatteryState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unknown(value) => write!(f, "unknown (0x{value:02x})"),
            state => f.write_str(state.as_str()),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BatteryCell {
    pub percent: Option<u8>,
    pub state: BatteryState,
}

impl BatteryCell {
    fn from_bytes(state: u8, percent: u8) -> Self {
        Self {
            percent: (percent <= 100).then_some(percent),
            state: BatteryState::from_byte(state),
        }
    }
}

impl fmt::Display for BatteryCell {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.percent, self.state) {
            // The receiver reports a useful case percentage while its state byte is 0xff.
            (Some(percent), BatteryState::Unavailable) => write!(f, "{percent}%"),
            (Some(percent), state) => write!(f, "{percent}% ({state})"),
            (None, state) => write!(f, "unknown ({state})"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BatteryReading {
    pub left: BatteryCell,
    pub right: BatteryCell,
    pub case: BatteryCell,
}

#[derive(Debug)]
pub struct QueryResult {
    pub reading: BatteryReading,
    pub raw_response: Vec<u8>,
}

pub fn discover_device() -> Result<PathBuf, Error> {
    discover_device_in(Path::new("/sys/class/hidraw"), Path::new("/dev"))
}

fn discover_device_in(class_root: &Path, device_root: &Path) -> Result<PathBuf, Error> {
    let entries = match fs::read_dir(class_root) {
        Ok(entries) => entries,
        Err(source) => return Err(io_error("read hidraw class", source)),
    };
    let mut paths = entries.map(|entry| entry.map(|entry| entry.path()));
    discover_device_paths(&mut paths, device_root)
}

fn discover_device_paths(
    paths: &mut dyn Iterator<Item = io::Result<PathBuf>>,
    device_root: &Path,
) -> Result<PathBuf, Error> {
    let mut matches = Vec::new();

    for path in paths {
        let path = match path {
            Ok(path) => path,
            Err(source) => return Err(io_error("inspect hidraw class", source)),
        };
        if matches_expected_interface(&path) {
            if let Some(name) = path.file_name() {
                matches.push(device_root.join(name));
            }
        }
    }

    matches.sort();
    match matches.len() {
        0 => Err(Error::DeviceNotFound),
        1 => Ok(matches.remove(0)),
        _ => Err(Error::AmbiguousDevices(matches)),
    }
}

fn is_inzone_uevent(uevent: &str) -> bool {
    uevent
        .lines()
        .any(|line| line.eq_ignore_ascii_case("HID_ID=0003:0000054C:00000EC2"))
}

fn matches_expected_interface(sysfs_node: &Path) -> bool {
    let Ok(uevent) = fs::read_to_string(sysfs_node.join("device/uevent")) else {
        return false;
    };
    let Ok(interface) = fs::read_to_string(sysfs_node.join("device/../bInterfaceNumber")) else {
        return false;
    };
    let Ok(descriptor) = fs::read(sysfs_node.join("device/report_descriptor")) else {
        return false;
    };

    is_inzone_uevent(&uevent)
        && u8::from_str_radix(interface.trim(), 16) == Ok(USB_INTERFACE_NUMBER)
        && descriptor
            .windows(EXPECTED_REPORT_COLLECTION.len())
            .any(|window| window == EXPECTED_REPORT_COLLECTION)
}

fn validate_device_path_in<'a>(path: &'a Path, device_root: &Path) -> Result<&'a str, Error> {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return Err(Error::DeviceMismatch(path.to_path_buf()));
    };
    let Some(suffix) = name.strip_prefix("hidraw") else {
        return Err(Error::DeviceMismatch(path.to_path_buf()));
    };
    if suffix.is_empty()
        || !suffix.bytes().all(|byte| byte.is_ascii_digit())
        || path != device_root.join(name)
    {
        return Err(Error::DeviceMismatch(path.to_path_buf()));
    }
    Ok(name)
}

fn linux_device_major(device: u64) -> u64 {
    ((device & 0x0000_0000_000f_ff00) >> 8) | ((device & 0xffff_f000_0000_0000) >> 32)
}

fn linux_device_minor(device: u64) -> u64 {
    (device & 0xff) | ((device & 0x0000_0fff_fff0_0000) >> 12)
}

fn open_verified_device(path: &Path) -> Result<File, Error> {
    open_verified_device_with(
        path,
        Path::new("/dev"),
        Path::new("/sys/dev/char"),
        open_hid_device,
    )
}

fn open_hid_device(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
}

fn open_verified_device_with<F>(
    path: &Path,
    device_root: &Path,
    sys_char_root: &Path,
    opener: F,
) -> Result<File, Error>
where
    F: FnOnce(&Path) -> io::Result<File>,
{
    validate_device_path_in(path, device_root)?;
    let device = match opener(path) {
        Ok(device) => device,
        Err(source) => {
            return Err(io_error(
                "open HID device for reading and battery queries",
                source,
            ));
        }
    };

    let metadata = device.metadata();
    inspect_opened_device(path, sys_char_root, device, metadata)
}

fn inspect_opened_device(
    path: &Path,
    sys_char_root: &Path,
    device: File,
    metadata: io::Result<fs::Metadata>,
) -> Result<File, Error> {
    let metadata = match metadata {
        Ok(metadata) => metadata,
        Err(source) => return Err(io_error("inspect opened HID device", source)),
    };
    if !metadata.file_type().is_char_device() {
        return Err(Error::DeviceMismatch(path.to_path_buf()));
    }

    let device_number = metadata.rdev();
    let sysfs_node = sys_char_root.join(format!(
        "{}:{}",
        linux_device_major(device_number),
        linux_device_minor(device_number)
    ));

    if matches_expected_interface(&sysfs_node) {
        Ok(device)
    } else {
        Err(Error::DeviceMismatch(path.to_path_buf()))
    }
}

pub fn battery_request(transaction_id: u16) -> [u8; REPORT_SIZE] {
    let mut report = [0_u8; REPORT_SIZE];
    report[..13].copy_from_slice(&[
        REPORT_ID,
        0x0c,
        COMMAND_PACKET_TYPE,
        0x00,
        0xfc,
        0x08,
        SONY_KEY_LOW,
        SONY_KEY_HIGH,
        0x41, // PC -> receiver
        BATTERY_EVENT_ID,
        EVENT_TYPE_GET,
        transaction_id as u8,
        (transaction_id >> 8) as u8,
    ]);
    report[13] = report[6..13]
        .iter()
        .fold(0_u8, |sum, value| sum.wrapping_add(*value));
    report
}

pub fn parse_battery_response(
    response: &[u8],
    expected_transaction: u16,
) -> Result<BatteryReading, Error> {
    if response.len() < 2 {
        return Err(Error::InvalidResponse(
            "report is shorter than its header".into(),
        ));
    }
    if response[0] != REPORT_ID {
        return Err(Error::InvalidResponse(format!(
            "unexpected report ID 0x{:02x}",
            response[0]
        )));
    }

    let frame_length = usize::from(response[1]);
    if frame_length != EXPECTED_FRAME_LENGTH || frame_length + 2 > response.len() {
        return Err(Error::InvalidResponse(format!(
            "invalid frame length {frame_length}"
        )));
    }
    let frame = &response[2..2 + frame_length];

    if frame[0] != EVENT_PACKET_TYPE
        || frame[1] != 0xff
        || frame[2] != EXPECTED_EVENT_PARAMETER_LENGTH
        || usize::from(frame[2]) != frame.len() - 3
        || frame[3] != 0
    {
        return Err(Error::InvalidResponse(
            "unexpected Sony HCI event header".into(),
        ));
    }
    if frame[4] != SONY_KEY_LOW || frame[5] != SONY_KEY_HIGH {
        return Err(Error::InvalidResponse("Sony key does not match".into()));
    }
    if frame[6] != 0x14 || frame[7] != BATTERY_EVENT_ID || frame[8] != EVENT_TYPE_RETURN {
        return Err(Error::InvalidResponse(
            "packet is not a battery return event".into(),
        ));
    }

    let transaction = u16::from_le_bytes([frame[9], frame[10]]);
    if transaction != expected_transaction {
        return Err(Error::InvalidResponse(format!(
            "transaction {transaction} does not match {expected_transaction}"
        )));
    }

    let calculated_checksum = frame[3..frame.len() - 1]
        .iter()
        .fold(0_u8, |sum, value| sum.wrapping_add(*value));
    if calculated_checksum != frame[frame.len() - 1] {
        return Err(Error::InvalidResponse(format!(
            "checksum 0x{:02x} does not match calculated 0x{calculated_checksum:02x}",
            frame[frame.len() - 1]
        )));
    }

    let params = &frame[11..frame.len() - 1];

    Ok(BatteryReading {
        left: BatteryCell::from_bytes(params[0], params[1]),
        right: BatteryCell::from_bytes(params[2], params[3]),
        case: BatteryCell::from_bytes(params[4], params[5]),
    })
}

pub fn query_battery(path: &Path, timeout: Duration) -> Result<QueryResult, Error> {
    query_battery_with(path, timeout, open_verified_device)
}

fn query_battery_with<D, F>(path: &Path, timeout: Duration, opener: F) -> Result<QueryResult, Error>
where
    D: Read + Write,
    F: FnOnce(&Path) -> Result<D, Error>,
{
    let transaction = NEXT_TRANSACTION.fetch_add(1, Ordering::Relaxed);
    query_battery_with_transaction(path, timeout, transaction, opener)
}

fn query_battery_with_transaction<D, F>(
    path: &Path,
    timeout: Duration,
    transaction: u16,
    opener: F,
) -> Result<QueryResult, Error>
where
    D: Read + Write,
    F: FnOnce(&Path) -> Result<D, Error>,
{
    let mut device = opener(path)?;
    let started = Instant::now();
    write_request(&mut device, &battery_request(transaction), started, timeout)?;
    let (reading, response) = read_response(&mut device, transaction, started, timeout)?;
    Ok(QueryResult {
        reading,
        raw_response: response,
    })
}

fn pause_until_retry(started: Instant, timeout: Duration) -> Result<(), Error> {
    let elapsed = started.elapsed();
    let Some(remaining) = timeout.checked_sub(elapsed) else {
        return Err(Error::Timeout(timeout));
    };
    thread::sleep(RETRY_INTERVAL.min(remaining));
    Ok(())
}

fn ensure_before_deadline(started: Instant, timeout: Duration) -> Result<(), Error> {
    if started.elapsed() >= timeout {
        Err(Error::Timeout(timeout))
    } else {
        Ok(())
    }
}

fn write_request(
    device: &mut dyn Write,
    request: &[u8; REPORT_SIZE],
    started: Instant,
    timeout: Duration,
) -> Result<(), Error> {
    loop {
        ensure_before_deadline(started, timeout)?;
        match device.write(request) {
            Ok(REPORT_SIZE) => return Ok(()),
            Ok(length) => {
                return Err(io_error(
                    "send complete battery GET report",
                    io::Error::new(
                        io::ErrorKind::WriteZero,
                        format!("wrote {length} of {REPORT_SIZE} bytes"),
                    ),
                ));
            }
            Err(source) if source.kind() == io::ErrorKind::Interrupted => {}
            Err(source) if source.kind() == io::ErrorKind::WouldBlock => {
                pause_until_retry(started, timeout)?;
            }
            Err(source) => return Err(io_error("send battery GET report", source)),
        }
    }
}

fn looks_like_matching_battery_event(response: &[u8], transaction: u16) -> bool {
    response.len() >= 13
        && response[0] == REPORT_ID
        && response[2] == EVENT_PACKET_TYPE
        && response[3] == 0xff
        && response[6] == SONY_KEY_LOW
        && response[7] == SONY_KEY_HIGH
        && response[8] == 0x14
        && response[9] == BATTERY_EVENT_ID
        && response[10] == EVENT_TYPE_RETURN
        && u16::from_le_bytes([response[11], response[12]]) == transaction
}

fn read_response(
    device: &mut dyn Read,
    transaction: u16,
    started: Instant,
    timeout: Duration,
) -> Result<(BatteryReading, Vec<u8>), Error> {
    loop {
        ensure_before_deadline(started, timeout)?;
        let mut response = [0_u8; REPORT_SIZE];
        match device.read(&mut response) {
            Ok(0) => {
                return Err(io_error(
                    "read battery response",
                    io::Error::new(io::ErrorKind::UnexpectedEof, "HID device returned no data"),
                ));
            }
            Ok(length) => match parse_battery_response(&response[..length], transaction) {
                Ok(reading) => {
                    let meaningful_length = 2 + usize::from(response[1]);
                    return Ok((reading, response[..meaningful_length].to_vec()));
                }
                Err(error)
                    if looks_like_matching_battery_event(&response[..length], transaction) =>
                {
                    return Err(error);
                }
                Err(_) => continue,
            },
            Err(source) if source.kind() == io::ErrorKind::Interrupted => continue,
            Err(source) if source.kind() == io::ErrorKind::WouldBlock => {
                pause_until_retry(started, timeout)?;
            }
            Err(source) => return Err(io_error("read battery response", source)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::error::Error as _;
    use std::os::unix::fs::symlink;
    use std::sync::atomic::AtomicUsize;

    const CAPTURED_RESPONSE: [u8; 64] = [
        0x02, 0x12, 0x04, 0xff, 0x0f, 0x00, 0x96, 0xc3, 0x14, 0x04, 0x10, 0x01, 0x00, 0x00, 0x36,
        0x00, 0x38, 0xff, 0x5a, 0x49, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00,
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
        assert!(!is_inzone_uevent(""));
    }

    #[test]
    fn validates_the_exact_opened_character_device() {
        let temp = TestDir::new();
        let device_root = temp.0.join("dev");
        let sys_root = temp.0.join("sys");
        let path = device_root.join("hidraw7");
        fs::create_dir_all(&device_root).unwrap();

        let denied = open_verified_device_with(&path, &device_root, &sys_root, |_| {
            Err(io::Error::from(io::ErrorKind::PermissionDenied))
        })
        .unwrap_err();
        assert!(matches!(denied, Error::Io { .. }));

        let regular = temp.0.join("regular");
        fs::write(&regular, b"data").unwrap();
        let mismatch =
            open_verified_device_with(&path, &device_root, &sys_root, |_| File::open(&regular))
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
        let opened = open_verified_device_with(&path, &device_root, &sys_root, |_| {
            OpenOptions::new().read(true).write(true).open("/dev/null")
        })
        .unwrap();
        assert!(opened.metadata().unwrap().file_type().is_char_device());

        fs::write(node.join("bInterfaceNumber"), "04").unwrap();
        let mismatch = open_verified_device_with(&path, &device_root, &sys_root, |_| {
            OpenOptions::new().read(true).write(true).open("/dev/null")
        })
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
            |_| {
                Ok(ScriptedDevice::new(
                    vec![IoAction::Data(response.to_vec())],
                    vec![],
                ))
            },
        )
        .unwrap();
        assert_eq!(result.reading.left.percent, Some(54));
        assert_eq!(result.raw_response, response[..20]);

        let error = query_battery_with::<ScriptedDevice, _>(
            Path::new("/ignored"),
            Duration::from_secs(1),
            |_| Err(Error::DeviceNotFound),
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
        assert!(
            write_request(&mut short, &request, Instant::now(), Duration::from_secs(1)).is_err()
        );

        let mut failed =
            ScriptedDevice::new(vec![], vec![IoAction::Error(io::ErrorKind::BrokenPipe)]);
        assert!(
            write_request(
                &mut failed,
                &request,
                Instant::now(),
                Duration::from_secs(1)
            )
            .is_err()
        );

        let mut blocked =
            ScriptedDevice::new(vec![], vec![IoAction::Error(io::ErrorKind::WouldBlock)]);
        assert!(
            write_request(
                &mut blocked,
                &request,
                Instant::now(),
                Duration::from_millis(1)
            )
            .is_err()
        );
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
    }
}

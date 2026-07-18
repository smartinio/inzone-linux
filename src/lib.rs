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
    let entries = fs::read_dir("/sys/class/hidraw")
        .map_err(|source| io_error("read /sys/class/hidraw", source))?;
    let mut matches = Vec::new();

    for entry in entries {
        let entry = entry.map_err(|source| io_error("inspect /sys/class/hidraw", source))?;
        if matches_expected_interface(&entry.path()) {
            matches.push(PathBuf::from("/dev").join(entry.file_name()));
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

fn validate_device_path(path: &Path) -> Result<&str, Error> {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return Err(Error::DeviceMismatch(path.to_path_buf()));
    };
    let Some(suffix) = name.strip_prefix("hidraw") else {
        return Err(Error::DeviceMismatch(path.to_path_buf()));
    };
    if suffix.is_empty()
        || !suffix.bytes().all(|byte| byte.is_ascii_digit())
        || path != Path::new("/dev").join(name)
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
    validate_device_path(path)?;

    let device = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|source| io_error("open HID device for reading and battery queries", source))?;

    let metadata = device
        .metadata()
        .map_err(|source| io_error("inspect opened HID device", source))?;
    if !metadata.file_type().is_char_device() {
        return Err(Error::DeviceMismatch(path.to_path_buf()));
    }

    let device_number = metadata.rdev();
    let sysfs_node = PathBuf::from(format!(
        "/sys/dev/char/{}:{}",
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
    if params.len() != 6 {
        return Err(Error::InvalidResponse(
            "battery payload is not exactly six bytes".into(),
        ));
    }

    Ok(BatteryReading {
        left: BatteryCell::from_bytes(params[0], params[1]),
        right: BatteryCell::from_bytes(params[2], params[3]),
        case: BatteryCell::from_bytes(params[4], params[5]),
    })
}

pub fn query_battery(path: &Path, timeout: Duration) -> Result<QueryResult, Error> {
    let mut device = open_verified_device(path)?;
    let started = Instant::now();
    let transaction = NEXT_TRANSACTION.fetch_add(1, Ordering::Relaxed);
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
    if remaining.is_zero() {
        return Err(Error::Timeout(timeout));
    }
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
    device: &mut File,
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
    device: &mut File,
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

    const CAPTURED_RESPONSE: [u8; 64] = [
        0x02, 0x12, 0x04, 0xff, 0x0f, 0x00, 0x96, 0xc3, 0x14, 0x04, 0x10, 0x01, 0x00, 0x00, 0x36,
        0x00, 0x38, 0xff, 0x5a, 0x49, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00,
    ];

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
        assert!(validate_device_path(Path::new("/dev/hidraw3")).is_ok());
        assert!(validate_device_path(Path::new("/tmp/hidraw3")).is_err());
        assert!(validate_device_path(Path::new("/dev/hidrawx")).is_err());
        assert!(validate_device_path(Path::new("/dev/hidraw3/../hidraw3")).is_err());
    }

    #[test]
    fn decodes_linux_device_numbers() {
        assert_eq!(linux_device_major(0xf303), 243);
        assert_eq!(linux_device_minor(0xf303), 3);
    }
}

use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};
use std::{env, ffi::OsString, path::PathBuf};

use inzone_buds::{BatteryCell, BatteryReading, DEFAULT_TIMEOUT, discover_device, query_battery};
use ksni::blocking::TrayMethods;
use ksni::menu::StandardItem;
use ksni::{Category, Icon, MenuItem, Status, ToolTip, Tray};

const AUTO_REFRESH_INTERVAL: Duration = Duration::from_secs(60);
const HOVER_REFRESH_COOLDOWN: Duration = Duration::from_secs(1);
const DIMMED_ICON_SIZE: i32 = 22;
const DIMMED_ICON_OPACITY: u8 = 102;
const HEADPHONES_MASK: &[u8; (DIMMED_ICON_SIZE * DIMMED_ICON_SIZE) as usize] = b"\
......................\
......................\
........######........\
......##########......\
.....###......###.....\
....##..........##....\
...##............##...\
...##............##...\
..##..............##..\
..##..............##..\
.####............####.\
.####............####.\
.####............####.\
.####............####.\
.####............####.\
..###............###..\
...##............##...\
......................\
......................\
......................\
......................\
......................";

#[derive(Debug)]
enum ReadingStatus {
    Loading,
    Ready(BatteryReading),
    Error(String),
}

#[derive(Debug)]
struct InzoneTray {
    status: ReadingStatus,
    refresh_sender: mpsc::SyncSender<()>,
    last_refresh_activity: Arc<Mutex<Instant>>,
    quit_sender: mpsc::Sender<()>,
}

impl InzoneTray {
    fn summary(&self) -> String {
        match &self.status {
            ReadingStatus::Loading => "Reading battery status…".into(),
            ReadingStatus::Ready(reading) => format!(
                "Left {} · Right {}",
                short_cell(reading.left),
                short_cell(reading.right)
            ),
            ReadingStatus::Error(error) => error.clone(),
        }
    }

    fn request_refresh(&self, minimum_age: Duration) -> bool {
        if matches!(&self.status, ReadingStatus::Loading) {
            return false;
        }

        let mut last_activity = self
            .last_refresh_activity
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if last_activity.elapsed() < minimum_age {
            return false;
        }

        if self.refresh_sender.try_send(()).is_ok() {
            *last_activity = Instant::now();
            true
        } else {
            false
        }
    }

    fn begin_refresh(&mut self) {
        if self.request_refresh(Duration::ZERO) {
            self.status = ReadingStatus::Loading;
        }
    }
}

impl Tray for InzoneTray {
    const MENU_ON_ACTIVATE: bool = true;

    fn id(&self) -> String {
        "inzone-buds-linux".into()
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        self.begin_refresh();
    }

    fn secondary_activate(&mut self, _x: i32, _y: i32) {
        self.begin_refresh();
    }

    fn category(&self) -> Category {
        Category::Hardware
    }

    fn title(&self) -> String {
        "Sony INZONE Buds".into()
    }

    fn status(&self) -> Status {
        match self.status {
            // The receiver cannot reach the buds while they are in the case.
            // Keep that expected offline state subdued instead of asking the
            // tray host to show an animated warning.
            ReadingStatus::Error(_) => Status::Passive,
            _ => Status::Active,
        }
    }

    fn icon_name(&self) -> String {
        match self.status {
            ReadingStatus::Error(_) => String::new(),
            _ => "audio-headphones-symbolic".into(),
        }
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        match self.status {
            ReadingStatus::Error(_) => vec![dimmed_icon()],
            _ => Vec::new(),
        }
    }

    fn attention_icon_name(&self) -> String {
        self.icon_name()
    }

    fn tool_tip(&self) -> ToolTip {
        // StatusNotifierItem has no hover event. Tray hosts fetch this property
        // to display a tooltip, so use that access as the hover refresh signal.
        // The cooldown also prevents ksni's own property checks from creating
        // a self-sustaining refresh loop.
        let _ = self.request_refresh(HOVER_REFRESH_COOLDOWN);
        ToolTip {
            icon_name: self.icon_name(),
            title: self.title(),
            description: self.summary(),
            ..ToolTip::default()
        }
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let refresh_enabled = !matches!(&self.status, ReadingStatus::Loading);
        let mut menu = match &self.status {
            ReadingStatus::Loading => vec![information_item("Reading battery status…")],
            ReadingStatus::Ready(reading) => vec![
                information_item(format!("Left:  {}", reading.left)),
                information_item(format!("Right: {}", reading.right)),
                information_item(format!("Case:  {}", reading.case)),
            ],
            ReadingStatus::Error(error) => vec![information_item(format!("Unavailable: {error}"))],
        };

        menu.extend([
            MenuItem::Separator,
            StandardItem {
                label: "Refresh".into(),
                icon_name: "view-refresh-symbolic".into(),
                enabled: refresh_enabled,
                activate: Box::new(|tray: &mut Self| {
                    tray.begin_refresh();
                }),
                ..StandardItem::default()
            }
            .into(),
            StandardItem {
                label: "Quit".into(),
                icon_name: "application-exit-symbolic".into(),
                activate: Box::new(|tray: &mut Self| {
                    let _ = tray.quit_sender.send(());
                }),
                ..StandardItem::default()
            }
            .into(),
        ]);
        menu
    }

    fn menu_about_to_show(&mut self) {
        // With MENU_ON_ACTIVATE, the tray host opens the menu instead of
        // invoking activate(), making this the reliable click notification.
        self.begin_refresh();
    }
}

fn information_item<T: Send + 'static>(label: impl Into<String>) -> MenuItem<T> {
    StandardItem {
        label: label.into(),
        enabled: false,
        ..StandardItem::default()
    }
    .into()
}

fn short_cell(cell: BatteryCell) -> String {
    cell.percent
        .map_or_else(|| "unknown".into(), |percent| format!("{percent}%"))
}

fn dimmed_icon() -> Icon {
    let data = HEADPHONES_MASK
        .iter()
        .flat_map(|pixel| {
            if *pixel == b'#' {
                [DIMMED_ICON_OPACITY, u8::MAX, u8::MAX, u8::MAX]
            } else {
                [0, 0, 0, 0]
            }
        })
        .collect();

    Icon {
        width: DIMMED_ICON_SIZE,
        height: DIMMED_ICON_SIZE,
        data,
    }
}

fn read_batteries_with<Q>(
    discovered: Result<std::path::PathBuf, inzone_buds::Error>,
    query: Q,
) -> Result<BatteryReading, String>
where
    Q: FnOnce(&std::path::Path) -> Result<inzone_buds::QueryResult, inzone_buds::Error>,
{
    let device = discovered.map_err(|error| error.to_string())?;
    query(&device)
        .map(|result| result.reading)
        .map_err(|error| error.to_string())
}

fn configured_device() -> Result<PathBuf, inzone_buds::Error> {
    let configured = cfg!(debug_assertions)
        .then(|| env::var_os("INZONE_BUDS_TRAY_TEST_DEVICE"))
        .flatten();
    configured_device_with(configured, discover_device)
}

fn configured_device_with(
    configured: Option<OsString>,
    discover: fn() -> Result<PathBuf, inzone_buds::Error>,
) -> Result<PathBuf, inzone_buds::Error> {
    match configured {
        Some(path) => Ok(path.into()),
        None => discover(),
    }
}

fn wait_for_refresh(receiver: &mpsc::Receiver<()>, interval: Duration) -> bool {
    matches!(
        receiver.recv_timeout(interval),
        Ok(()) | Err(mpsc::RecvTimeoutError::Timeout)
    )
}

fn run_refresh_loop(
    receiver: &mpsc::Receiver<()>,
    interval: Duration,
    read: &mut dyn FnMut() -> Result<BatteryReading, String>,
    update: &mut dyn FnMut(ReadingStatus) -> bool,
) {
    loop {
        let status = match read() {
            Ok(reading) => ReadingStatus::Ready(reading),
            Err(error) => ReadingStatus::Error(error),
        };
        if !update(status) {
            break;
        }
        if !wait_for_refresh(receiver, interval) {
            break;
        }
    }
}

fn finish_refresh(updated: bool, exit_after_refresh: bool, exit_sender: &mpsc::Sender<()>) -> bool {
    if updated && exit_after_refresh {
        let _ = exit_sender.send(());
    }
    updated
}

fn wait_until_quit(
    receiver: &mpsc::Receiver<()>,
    interval: Duration,
    is_closed: &mut dyn FnMut() -> bool,
) {
    while !is_closed() {
        match receiver.recv_timeout(interval) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (refresh_sender, refresh_receiver) = mpsc::sync_channel(1);
    let (quit_sender, quit_receiver) = mpsc::channel();
    let refresh_exit_sender = quit_sender.clone();
    let last_refresh_activity = Arc::new(Mutex::new(Instant::now()));
    let exit_after_refresh = cfg!(debug_assertions)
        && env::var("INZONE_BUDS_TRAY_TEST_EXIT_AFTER_REFRESH").as_deref() == Ok("1");
    let tray = InzoneTray {
        status: ReadingStatus::Loading,
        refresh_sender: refresh_sender.clone(),
        last_refresh_activity: last_refresh_activity.clone(),
        quit_sender,
    };
    let handle = tray.assume_sni_available(true).spawn()?;

    let update_handle = handle.clone();
    thread::spawn(move || {
        let mut read = || {
            read_batteries_with(configured_device(), |device| {
                query_battery(device, DEFAULT_TIMEOUT)
            })
        };
        let mut update = |status| {
            *last_refresh_activity
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Instant::now();
            let updated = update_handle
                .update(move |tray| tray.status = status)
                .is_some();
            finish_refresh(updated, exit_after_refresh, &refresh_exit_sender)
        };
        run_refresh_loop(
            &refresh_receiver,
            AUTO_REFRESH_INTERVAL,
            &mut read,
            &mut update,
        );
    });

    let mut is_closed = || handle.is_closed();
    wait_until_quit(&quit_receiver, Duration::from_secs(1), &mut is_closed);
    handle.shutdown().wait();
    Ok(())
}

#[cfg(test)]
#[path = "../../tests/unit/inzone-buds-tray.rs"]
mod tests;

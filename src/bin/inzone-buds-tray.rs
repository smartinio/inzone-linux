use std::sync::mpsc;
use std::thread;
use std::time::Duration;
use std::{env, ffi::OsString, path::PathBuf};

use inzone_buds::{BatteryCell, BatteryReading, DEFAULT_TIMEOUT, discover_device, query_battery};
use ksni::blocking::TrayMethods;
use ksni::menu::StandardItem;
use ksni::{Category, MenuItem, Status, ToolTip, Tray};

const AUTO_REFRESH_INTERVAL: Duration = Duration::from_secs(60);

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
    quit_sender: mpsc::Sender<()>,
}

impl InzoneTray {
    fn summary(&self) -> String {
        match &self.status {
            ReadingStatus::Loading => "Reading battery status…".into(),
            ReadingStatus::Ready(reading) => format!(
                "Left {} · Right {} · Case {}",
                short_cell(reading.left),
                short_cell(reading.right),
                short_cell(reading.case)
            ),
            ReadingStatus::Error(error) => error.clone(),
        }
    }
}

impl Tray for InzoneTray {
    const MENU_ON_ACTIVATE: bool = true;

    fn id(&self) -> String {
        "inzone-buds-linux".into()
    }

    fn category(&self) -> Category {
        Category::Hardware
    }

    fn title(&self) -> String {
        "Sony INZONE Buds".into()
    }

    fn status(&self) -> Status {
        match self.status {
            ReadingStatus::Error(_) => Status::NeedsAttention,
            _ => Status::Active,
        }
    }

    fn icon_name(&self) -> String {
        "audio-headphones-symbolic".into()
    }

    fn attention_icon_name(&self) -> String {
        "dialog-warning-symbolic".into()
    }

    fn tool_tip(&self) -> ToolTip {
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
                    if !matches!(&tray.status, ReadingStatus::Loading)
                        && tray.refresh_sender.try_send(()).is_ok()
                    {
                        tray.status = ReadingStatus::Loading;
                    }
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

fn run_refresh_loop<R, U>(
    receiver: &mpsc::Receiver<()>,
    interval: Duration,
    mut read: R,
    mut update: U,
) where
    R: FnMut() -> Result<BatteryReading, String>,
    U: FnMut(ReadingStatus) -> bool,
{
    while wait_for_refresh(receiver, interval) {
        let status = match read() {
            Ok(reading) => ReadingStatus::Ready(reading),
            Err(error) => ReadingStatus::Error(error),
        };
        if !update(status) {
            break;
        }
    }
}

fn wait_until_quit<F>(receiver: &mpsc::Receiver<()>, interval: Duration, mut is_closed: F)
where
    F: FnMut() -> bool,
{
    loop {
        match receiver.recv_timeout(interval) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) if is_closed() => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (refresh_sender, refresh_receiver) = mpsc::sync_channel(1);
    let (quit_sender, quit_receiver) = mpsc::channel();
    let refresh_exit_sender = quit_sender.clone();
    let exit_after_refresh = cfg!(debug_assertions)
        && env::var("INZONE_BUDS_TRAY_TEST_EXIT_AFTER_REFRESH").as_deref() == Ok("1");
    let tray = InzoneTray {
        status: ReadingStatus::Loading,
        refresh_sender: refresh_sender.clone(),
        quit_sender,
    };
    let handle = tray.assume_sni_available(true).spawn()?;

    let update_handle = handle.clone();
    thread::spawn(move || {
        run_refresh_loop(
            &refresh_receiver,
            AUTO_REFRESH_INTERVAL,
            || {
                read_batteries_with(configured_device(), |device| {
                    query_battery(device, DEFAULT_TIMEOUT)
                })
            },
            |status| {
                let updated = update_handle
                    .update(move |tray| tray.status = status)
                    .is_some();
                if updated && exit_after_refresh {
                    let _ = refresh_exit_sender.send(());
                }
                updated
            },
        );
    });

    refresh_sender.try_send(())?;
    let is_closed = || handle.is_closed();
    if exit_after_refresh {
        let _ = is_closed();
    }
    wait_until_quit(&quit_receiver, Duration::from_secs(1), is_closed);
    handle.shutdown().wait();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use inzone_buds::{BatteryState, QueryResult};

    fn reading() -> BatteryReading {
        BatteryReading {
            left: BatteryCell {
                percent: Some(40),
                state: BatteryState::Discharging,
            },
            right: BatteryCell {
                percent: Some(50),
                state: BatteryState::Charging,
            },
            case: BatteryCell {
                percent: None,
                state: BatteryState::Unavailable,
            },
        }
    }

    fn tray(status: ReadingStatus) -> (InzoneTray, mpsc::Receiver<()>, mpsc::Receiver<()>) {
        let (refresh_sender, refresh_receiver) = mpsc::sync_channel(1);
        let (quit_sender, quit_receiver) = mpsc::channel();
        (
            InzoneTray {
                status,
                refresh_sender,
                quit_sender,
            },
            refresh_receiver,
            quit_receiver,
        )
    }

    fn standard(item: MenuItem<InzoneTray>) -> Option<StandardItem<InzoneTray>> {
        if let MenuItem::Standard(item) = item {
            Some(item)
        } else {
            None
        }
    }

    fn query_error(_: &std::path::Path) -> Result<QueryResult, inzone_buds::Error> {
        Err(inzone_buds::Error::Timeout(Duration::from_secs(1)))
    }

    fn discovered() -> Result<PathBuf, inzone_buds::Error> {
        Ok("/dev/hidraw3".into())
    }

    #[test]
    fn refreshes_for_manual_requests_and_timer_expiry() {
        let (sender, receiver) = mpsc::sync_channel(1);
        sender.try_send(()).unwrap();
        assert!(wait_for_refresh(&receiver, Duration::from_secs(1)));
        assert!(wait_for_refresh(&receiver, Duration::ZERO));
    }

    #[test]
    fn stops_refreshing_when_all_senders_are_gone() {
        let (sender, receiver) = mpsc::sync_channel(1);
        drop(sender);
        assert!(!wait_for_refresh(&receiver, Duration::ZERO));
    }

    #[test]
    fn exposes_every_tray_state_and_property() {
        let (loading, _, _) = tray(ReadingStatus::Loading);
        assert_eq!(loading.summary(), "Reading battery status…");
        assert_eq!(loading.id(), "inzone-buds-linux");
        assert!(matches!(loading.category(), Category::Hardware));
        assert_eq!(loading.title(), "Sony INZONE Buds");
        assert!(matches!(loading.status(), Status::Active));
        assert_eq!(loading.icon_name(), "audio-headphones-symbolic");
        assert_eq!(loading.attention_icon_name(), "dialog-warning-symbolic");
        let tooltip = loading.tool_tip();
        assert_eq!(tooltip.title, "Sony INZONE Buds");
        assert_eq!(tooltip.description, "Reading battery status…");
        assert_eq!(loading.menu().len(), 4);

        let (ready, _, _) = tray(ReadingStatus::Ready(reading()));
        assert_eq!(ready.summary(), "Left 40% · Right 50% · Case unknown");
        assert!(matches!(ready.status(), Status::Active));
        let menu = ready.menu();
        assert_eq!(menu.len(), 6);
        assert_eq!(
            standard(menu.into_iter().next().unwrap()).unwrap().label,
            "Left:  40% (discharging)"
        );

        let (failed, _, _) = tray(ReadingStatus::Error("offline".into()));
        assert_eq!(failed.summary(), "offline");
        assert!(matches!(failed.status(), Status::NeedsAttention));
        assert_eq!(
            standard(failed.menu().into_iter().next().unwrap())
                .unwrap()
                .label,
            "Unavailable: offline"
        );
        assert!(standard(MenuItem::Separator).is_none());
        assert_eq!(
            short_cell(BatteryCell {
                percent: None,
                state: BatteryState::Unavailable
            }),
            "unknown"
        );
    }

    #[test]
    fn menu_callbacks_coalesce_refresh_and_quit() {
        let (mut ready, refresh_receiver, quit_receiver) = tray(ReadingStatus::Ready(reading()));
        let mut menu = ready.menu();
        let refresh = standard(menu.remove(4)).unwrap();
        assert!(refresh.enabled);
        (refresh.activate)(&mut ready);
        assert!(matches!(ready.status, ReadingStatus::Loading));
        refresh_receiver.try_recv().unwrap();

        let mut loading_menu = ready.menu();
        let disabled = standard(loading_menu.remove(2)).unwrap();
        assert!(!disabled.enabled);
        (disabled.activate)(&mut ready);
        assert!(refresh_receiver.try_recv().is_err());

        ready.status = ReadingStatus::Ready(reading());
        ready.refresh_sender.try_send(()).unwrap();
        let mut full_menu = ready.menu();
        let full = standard(full_menu.remove(4)).unwrap();
        (full.activate)(&mut ready);
        assert!(matches!(ready.status, ReadingStatus::Ready(_)));
        refresh_receiver.try_recv().unwrap();

        let mut quit_menu = ready.menu();
        let quit = standard(quit_menu.remove(5)).unwrap();
        (quit.activate)(&mut ready);
        quit_receiver.try_recv().unwrap();
        drop(quit_receiver);
        let mut quit_menu = ready.menu();
        let quit = standard(quit_menu.remove(5)).unwrap();
        (quit.activate)(&mut ready);
    }

    #[test]
    fn reads_batteries_through_both_error_boundaries() {
        let expected = reading();
        let actual = read_batteries_with(Ok("/dev/hidraw3".into()), |_| {
            Ok(QueryResult {
                reading: expected,
                raw_response: vec![],
            })
        })
        .unwrap();
        assert_eq!(actual, expected);
        assert!(read_batteries_with(Err(inzone_buds::Error::DeviceNotFound), query_error).is_err());
        assert!(read_batteries_with(Ok("/dev/hidraw3".into()), query_error).is_err());
    }

    #[test]
    fn refresh_worker_maps_success_error_timer_and_shutdown() {
        let (sender, receiver) = mpsc::channel();
        sender.send(()).unwrap();
        sender.send(()).unwrap();
        drop(sender);
        let mut reads = 0;
        let mut states = Vec::new();
        run_refresh_loop(
            &receiver,
            Duration::from_secs(1),
            || {
                reads += 1;
                if reads == 1 {
                    Ok(reading())
                } else {
                    Err("offline".into())
                }
            },
            |status| {
                states.push(status);
                true
            },
        );
        assert_eq!(states.len(), 2);
        assert!(matches!(states[0], ReadingStatus::Ready(_)));
        assert!(matches!(states[1], ReadingStatus::Error(_)));

        let (_sender, receiver) = mpsc::channel();
        run_refresh_loop(&receiver, Duration::ZERO, || Ok(reading()), |_| false);
    }

    #[test]
    fn quit_wait_handles_message_disconnect_and_both_timeout_states() {
        let (sender, receiver) = mpsc::channel();
        sender.send(()).unwrap();
        wait_until_quit(&receiver, Duration::from_secs(1), || false);

        let (sender, receiver) = mpsc::channel::<()>();
        drop(sender);
        wait_until_quit(&receiver, Duration::from_secs(1), || false);

        let (_sender, receiver) = mpsc::channel::<()>();
        let mut checks = 0;
        wait_until_quit(&receiver, Duration::ZERO, || {
            checks += 1;
            checks == 2
        });
        assert_eq!(checks, 2);
    }

    #[test]
    fn configured_device_covers_override_and_discovery() {
        assert_eq!(
            configured_device_with(Some("/dev/hidraw4".into()), discovered).unwrap(),
            PathBuf::from("/dev/hidraw4")
        );
        assert_eq!(
            configured_device_with(None, discovered).unwrap(),
            PathBuf::from("/dev/hidraw3")
        );
    }
}

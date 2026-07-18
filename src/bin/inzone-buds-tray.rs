use std::sync::mpsc;
use std::thread;
use std::time::Duration;

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

fn read_batteries() -> Result<BatteryReading, String> {
    let device = discover_device().map_err(|error| error.to_string())?;
    query_battery(&device, DEFAULT_TIMEOUT)
        .map(|result| result.reading)
        .map_err(|error| error.to_string())
}

fn wait_for_refresh(receiver: &mpsc::Receiver<()>, interval: Duration) -> bool {
    matches!(
        receiver.recv_timeout(interval),
        Ok(()) | Err(mpsc::RecvTimeoutError::Timeout)
    )
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (refresh_sender, refresh_receiver) = mpsc::sync_channel(1);
    let (quit_sender, quit_receiver) = mpsc::channel();
    let tray = InzoneTray {
        status: ReadingStatus::Loading,
        refresh_sender: refresh_sender.clone(),
        quit_sender,
    };
    let handle = tray.assume_sni_available(true).spawn()?;

    let update_handle = handle.clone();
    thread::spawn(move || {
        while wait_for_refresh(&refresh_receiver, AUTO_REFRESH_INTERVAL) {
            let result = read_batteries();
            if update_handle
                .update(move |tray| {
                    tray.status = match result {
                        Ok(reading) => ReadingStatus::Ready(reading),
                        Err(error) => ReadingStatus::Error(error),
                    };
                })
                .is_none()
            {
                break;
            }
        }
    });

    refresh_sender.try_send(())?;
    loop {
        match quit_receiver.recv_timeout(Duration::from_secs(1)) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) if handle.is_closed() => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
    handle.shutdown().wait();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
}

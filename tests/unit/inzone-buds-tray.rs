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
    drop(sender);
    let mut reads = 0;
    let mut states = Vec::new();
    let mut read = || {
        reads += 1;
        if reads == 1 {
            Ok(reading())
        } else {
            Err("offline".into())
        }
    };
    let mut update = |status| {
        states.push(status);
        true
    };
    run_refresh_loop(&receiver, Duration::from_secs(1), &mut read, &mut update);
    assert_eq!(states.len(), 2);
    assert!(matches!(states[0], ReadingStatus::Ready(_)));
    assert!(matches!(states[1], ReadingStatus::Error(_)));

    let (_sender, receiver) = mpsc::channel();
    run_refresh_loop(
        &receiver,
        Duration::ZERO,
        &mut || Ok(reading()),
        &mut |_| false,
    );
}

#[test]
fn refresh_completion_signals_only_after_a_successful_update() {
    let (sender, receiver) = mpsc::channel();
    assert!(finish_refresh(true, true, &sender));
    receiver.try_recv().unwrap();

    assert!(finish_refresh(true, false, &sender));
    assert!(receiver.try_recv().is_err());

    assert!(!finish_refresh(false, true, &sender));
    assert!(receiver.try_recv().is_err());
}

#[test]
fn quit_wait_handles_message_disconnect_and_both_timeout_states() {
    let (sender, receiver) = mpsc::channel();
    sender.send(()).unwrap();
    wait_until_quit(&receiver, Duration::from_secs(1), &mut || false);

    let (sender, receiver) = mpsc::channel::<()>();
    drop(sender);
    wait_until_quit(&receiver, Duration::from_secs(1), &mut || false);

    let (_sender, receiver) = mpsc::channel::<()>();
    let mut checks = 0;
    wait_until_quit(&receiver, Duration::ZERO, &mut || {
        checks += 1;
        checks == 2
    });
    assert_eq!(checks, 2);

    let (_sender, receiver) = mpsc::channel::<()>();
    wait_until_quit(&receiver, Duration::ZERO, &mut || true);
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

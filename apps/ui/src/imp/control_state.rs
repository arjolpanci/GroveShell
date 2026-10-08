//! Slow radio operations run on one worker; painting only reads snapshots.
use super::{radios, theme, wifi};
use std::cell::RefCell;
use std::sync::mpsc::{self, Receiver, Sender};

#[derive(Clone)]
pub(crate) enum Action {
    Refresh,
    Wifi(bool),
    Bluetooth(bool),
    Theme,
    Airplane(bool),
    Scan,
    Connect(String),
    Disconnect,
}

#[derive(Clone, Default)]
pub(crate) struct Snapshot {
    pub wifi: Option<bool>,
    pub bluetooth: Option<bool>,
    pub airplane: Option<bool>,
    pub light: Option<bool>,
    pub message: String,
    pub networks: Vec<wifi::Network>,
    pub network_error: Option<u32>,
}

struct Worker {
    tx: Sender<Action>,
    rx: Receiver<Snapshot>,
    snapshot: Snapshot,
    pending: usize,
}

impl Worker {
    fn new() -> Self {
        let (tx, requests) = mpsc::channel();
        let (results, rx) = mpsc::channel();
        std::thread::spawn(move || {
            use windows::Win32::System::Com::{
                CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED,
            };
            // SAFETY: initialize only this new worker thread, and balance
            // successful initialization below on that same thread.
            let initialized = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
            let mut connecting: Option<(String, std::time::Instant)> = None;
            while let Ok(action) = requests.recv() {
                let mut network_result = None;
                match &action {
                    Action::Refresh => {}
                    Action::Wifi(on) => wifi::set_wifi_radio_on(*on),
                    Action::Bluetooth(on) => radios::set_bluetooth_on(*on),
                    Action::Airplane(on) => radios::set_airplane_mode_on(*on),
                    Action::Theme => theme::toggle_theme(),
                    Action::Scan => wifi::scan(),
                    Action::Connect(profile) => network_result = Some(wifi::connect_saved(profile)),
                    Action::Disconnect => network_result = Some(wifi::disconnect()),
                }
                let networks = wifi::available_networks();
                let mut snapshot = Snapshot {
                    wifi: wifi::wifi_radio_on(),
                    bluetooth: radios::bluetooth_on(),
                    airplane: radios::airplane_mode_on(),
                    light: theme::apps_use_light_theme(),
                    message: String::new(),
                    network_error: networks.as_ref().err().copied(),
                    networks: networks.unwrap_or_default(),
                };
                let failed = match action {
                    Action::Wifi(on) => snapshot.wifi != Some(on),
                    Action::Bluetooth(on) => snapshot.bluetooth != Some(on),
                    Action::Airplane(on) => snapshot.airplane != Some(on),
                    _ => false,
                };
                if failed {
                    snapshot.message =
                        "Couldn't change this setting. Open its options to check Windows settings."
                            .into();
                }
                if let Some(code) = network_result {
                    snapshot.message = if code == 0 {
                        "Connection request sent. Waiting for Windows…".into()
                    } else {
                        format!("Couldn't update the connection (Windows error {code}).")
                    };
                    if code == 0 {
                        if let Action::Connect(profile) = &action {
                            connecting = Some((profile.clone(), std::time::Instant::now()));
                        } else {
                            connecting = None;
                            snapshot.message.clear();
                        }
                    }
                }
                if let Some((profile, started)) = &connecting {
                    if snapshot
                        .networks
                        .iter()
                        .any(|n| n.connected && n.profile == *profile)
                    {
                        snapshot.message = "Connected".into();
                        connecting = None;
                    } else if started.elapsed().as_secs() >= 20 {
                        snapshot.message =
                            "Couldn't connect. Open Windows networks to check the connection."
                                .into();
                        connecting = None;
                    } else {
                        snapshot.message = "Connecting…".into();
                    }
                }
                if results.send(snapshot).is_err() {
                    break;
                }
            }
            if initialized {
                // SAFETY: balances this thread's successful CoInitializeEx.
                unsafe {
                    CoUninitialize();
                }
            }
        });
        Self {
            tx,
            rx,
            snapshot: Snapshot::default(),
            pending: 0,
        }
    }
}

thread_local! { static WORKER: RefCell<Worker> = RefCell::new(Worker::new()); }

pub(crate) fn snapshot() -> Snapshot {
    WORKER.with(|w| w.borrow().snapshot.clone())
}

#[cfg(test)]
pub(crate) fn set_test_snapshot(snapshot: Snapshot) {
    WORKER.with(|w| w.borrow_mut().snapshot = snapshot);
}

pub(crate) fn request(action: Action) {
    WORKER.with(|w| {
        let mut w = w.borrow_mut();
        let refresh = matches!(action, Action::Refresh);
        if (refresh && w.pending > 0) || w.pending >= 3 {
            return;
        }
        if w.tx.send(action).is_ok() {
            w.pending += 1;
            if !refresh {
                w.snapshot.message = "Applying change…".into();
            }
        }
    });
}

pub(crate) fn poll() -> bool {
    WORKER.with(|w| {
        let mut w = w.borrow_mut();
        if let Ok(snapshot) = w.rx.try_recv() {
            let previous_message = w.snapshot.message.clone();
            w.snapshot = snapshot;
            if w.snapshot.message.is_empty() && previous_message.starts_with("Couldn't") {
                w.snapshot.message = previous_message;
            }
            w.pending = w.pending.saturating_sub(1);
            true
        } else {
            false
        }
    })
}

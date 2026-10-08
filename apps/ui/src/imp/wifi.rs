//! Best-effort Wi-Fi radio on/off via `wlanapi.dll`, for the Quick
//! Settings panel's Wi-Fi toggle chip. A machine with no wireless
//! adapter, or with the WLAN AutoConfig service stopped, just reports
//! "unavailable" — this never fails the rest of the panel.

use windows::core::GUID;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::NetworkManagement::WiFi::{
    wlan_intf_opcode_radio_state, WlanCloseHandle, WlanEnumInterfaces, WlanFreeMemory,
    WlanOpenHandle, WlanQueryInterface, WlanSetInterface, WLAN_INTERFACE_INFO_LIST,
    WLAN_OPCODE_VALUE_TYPE, WLAN_PHY_RADIO_STATE, WLAN_RADIO_STATE,
};

const DOT11_RADIO_STATE_ON: u32 = 1;
const DOT11_RADIO_STATE_OFF: u32 = 2;

#[derive(Clone, Debug)]
pub(crate) struct Network {
    pub name: String,
    pub profile: String,
    pub signal: u32,
    pub secured: bool,
    pub connected: bool,
    pub connectable: bool,
}

/// Copy the variable-length WLAN allocation while its handle is live.
pub(crate) fn available_networks() -> Result<Vec<Network>, u32> {
    with_first_interface(|handle, guid| unsafe {
        use windows::Win32::NetworkManagement::WiFi::*;
        let mut list = std::ptr::null_mut();
        let status = WlanGetAvailableNetworkList(handle, guid, 0, None, &mut list);
        if status != 0 || list.is_null() {
            return Some(Err(if status == 0 { 1 } else { status }));
        }
        // SAFETY: WLAN owns a contiguous array with dwNumberOfItems entries;
        // all strings/flags are copied before WlanFreeMemory below.
        let entries =
            std::slice::from_raw_parts((*list).Network.as_ptr(), (*list).dwNumberOfItems as usize);
        let mut networks: Vec<Network> = entries
            .iter()
            .map(|n| Network {
                name: String::from_utf8_lossy(
                    &n.dot11Ssid.ucSSID[..(n.dot11Ssid.uSSIDLength as usize).min(32)],
                )
                .into_owned(),
                profile: String::from_utf16_lossy(
                    &n.strProfileName
                        [..n.strProfileName.iter().position(|c| *c == 0).unwrap_or(256)],
                ),
                signal: n.wlanSignalQuality.min(100),
                secured: n.bSecurityEnabled.as_bool(),
                connected: n.dwFlags & WLAN_AVAILABLE_NETWORK_CONNECTED != 0,
                connectable: n.bNetworkConnectable.as_bool(),
            })
            .collect();
        WlanFreeMemory(list as *const _);
        networks.sort_by(|a, b| {
            b.connected
                .cmp(&a.connected)
                .then(a.name.cmp(&b.name))
                .then(b.profile.cmp(&a.profile))
        });
        networks.dedup_by(|a, b| a.name == b.name && a.secured == b.secured);
        Some(Ok(networks))
    })
    .unwrap_or(Err(1062))
}

pub(crate) fn scan() {
    with_first_interface(|handle, guid| {
        // SAFETY: live interface and client handle; optional parameters are null.
        unsafe {
            windows::Win32::NetworkManagement::WiFi::WlanScan(handle, guid, None, None, None);
        }
        Some(())
    });
}

pub(crate) fn connect_saved(profile: &str) -> u32 {
    with_first_interface(|handle, guid| {
        use windows::Win32::NetworkManagement::WiFi::*;
        let profile = windows::core::HSTRING::from(profile);
        let params = WLAN_CONNECTION_PARAMETERS {
            wlanConnectionMode: wlan_connection_mode_profile,
            strProfile: windows::core::PCWSTR(profile.as_ptr()),
            dot11BssType: dot11_BSS_type_infrastructure,
            ..Default::default()
        };
        // SAFETY: profile, parameters and handle remain live for the call.
        Some(unsafe { WlanConnect(handle, guid, &params, None) })
    })
    .unwrap_or(1062)
}

pub(crate) fn disconnect() -> u32 {
    with_first_interface(|handle, guid| {
        // SAFETY: the helper owns the live WLAN handle and interface GUID.
        Some(unsafe { windows::Win32::NetworkManagement::WiFi::WlanDisconnect(handle, guid, None) })
    })
    .unwrap_or(1062)
}

/// Opens a fresh WLAN handle and hands the first available interface's
/// GUID to `f`, same "reacquire every call" tradeoff as the volume
/// control in `quick_settings.rs` — this only ever runs in response to
/// a user opening/clicking the panel.
fn with_first_interface<R>(f: impl FnOnce(HANDLE, &GUID) -> Option<R>) -> Option<R> {
    // SAFETY: every call here is synchronous; `handle` is closed before
    // returning, and `list` is freed via `WlanFreeMemory` before
    // returning, both on every path including early `?` returns via the
    // enclosing `Option`-returning helper below.
    unsafe {
        let mut negotiated = 0u32;
        let mut handle = HANDLE::default();
        if WlanOpenHandle(2, None, &mut negotiated, &mut handle) != 0 {
            return None;
        }
        let result = (|| {
            let mut list_ptr: *mut WLAN_INTERFACE_INFO_LIST = std::ptr::null_mut();
            if WlanEnumInterfaces(handle, None, &mut list_ptr) != 0 || list_ptr.is_null() {
                return None;
            }
            let list = &*list_ptr;
            let out = if list.dwNumberOfItems == 0 {
                None
            } else {
                let guid = list.InterfaceInfo[0].InterfaceGuid;
                f(handle, &guid)
            };
            WlanFreeMemory(list_ptr as *const _);
            out
        })();
        let _ = WlanCloseHandle(handle, None);
        result
    }
}

/// `None` when there's no Wi-Fi adapter to report (or the WLAN service
/// isn't running) — the toggle chip hides itself in that case.
pub(crate) fn wifi_radio_on() -> Option<bool> {
    with_first_interface(|handle, guid| {
        // SAFETY: `handle`/`guid` both came from a live `WlanOpenHandle`/
        // `WlanEnumInterfaces` pair still in scope; `data` is freed via
        // `WlanFreeMemory` before returning on every path.
        unsafe {
            let mut data_ptr: *mut core::ffi::c_void = std::ptr::null_mut();
            let mut data_size = 0u32;
            let mut opcode_type = WLAN_OPCODE_VALUE_TYPE::default();
            let ok = WlanQueryInterface(
                handle,
                guid,
                wlan_intf_opcode_radio_state,
                None,
                &mut data_size,
                &mut data_ptr,
                Some(&mut opcode_type),
            ) == 0
                && !data_ptr.is_null();
            // A *query* for `wlan_intf_opcode_radio_state` returns a
            // `WLAN_RADIO_STATE` (a phy count, then an array of
            // per-phy `WLAN_PHY_RADIO_STATE`) — not a bare
            // `WLAN_PHY_RADIO_STATE` on its own, which is only what
            // `WlanSetInterface` takes for a *set*. Reading it as the
            // wrong shape silently misaligned every field by one
            // `DWORD`, which is why this reported "off" no matter what
            // the adapter's actual state was. Hardware state also
            // isn't required to read exactly "on" — most adapters have
            // no physical kill switch and report it as "unknown"
            // instead, so only treat an explicit hardware-off as
            // meaningful.
            let on = ok.then(|| {
                let state = &*(data_ptr as *const WLAN_RADIO_STATE);
                let phy = &state.PhyRadioState[0];
                phy.dot11SoftwareRadioState.0 as u32 == DOT11_RADIO_STATE_ON
                    && phy.dot11HardwareRadioState.0 as u32 != DOT11_RADIO_STATE_OFF
            });
            if !data_ptr.is_null() {
                WlanFreeMemory(data_ptr);
            }
            on
        }
    })
}

/// Sets the software radio state (the only side a program can control —
/// a hardware kill switch, if the laptop has one, always wins). No-op,
/// silently, if there's no adapter.
pub(crate) fn set_wifi_radio_on(on: bool) {
    with_first_interface(|handle, guid| {
        let mut state = WLAN_PHY_RADIO_STATE {
            dot11SoftwareRadioState: windows::Win32::NetworkManagement::WiFi::DOT11_RADIO_STATE(
                if on {
                    DOT11_RADIO_STATE_ON
                } else {
                    DOT11_RADIO_STATE_OFF
                } as i32,
            ),
            ..Default::default()
        };
        // SAFETY: `handle`/`guid` are live for the duration of this call;
        // `state` is a plain, fully-initialized local passed by pointer
        // only for the duration of the call.
        unsafe {
            let _ = WlanSetInterface(
                handle,
                guid,
                wlan_intf_opcode_radio_state,
                std::mem::size_of::<WLAN_PHY_RADIO_STATE>() as u32,
                &mut state as *mut _ as *const core::ffi::c_void,
                None,
            );
        }
        Some(())
    });
}

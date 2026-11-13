//! The notification area: GroveShell hosts it, rather than copying
//! Explorer's.
//!
//! An application never talks to "the taskbar". `Shell_NotifyIcon` looks
//! up the first top-level window of class `Shell_TrayWnd` and sends it a
//! `WM_COPYDATA` carrying the icon, its tooltip, and the window and
//! message the application wants its clicks delivered to. Whoever owns
//! that window is the notification area.
//!
//! So this module creates one. While the shell runs it sits above
//! Explorer's in the z-order, which is what the lookup goes by, and
//! receives every add, change and removal as the application makes it.
//! Each message is also passed on to Explorer's real tray, so Explorer's
//! own record stays complete and nothing is lost when the shell exits.
//!
//! This replaces a scheme that screenshotted Explorer's taskbar before
//! hiding it and cropped the icons out of the picture. Windows 11 stops
//! rendering a hidden taskbar, so those pictures could never update: a
//! new icon, or an unread badge, needed a restart to appear. Here the
//! icons are the real ones, they change when the application changes
//! them, and a click goes straight to the application instead of
//! through UI Automation on a window that is not being drawn.
//!
//! What this does not cover is the icons Explorer draws for itself
//! (network, volume, battery). Those are not notify icons at all on
//! Windows 11, and the bar has its own controls for them.
//!
//! Which icons go in the bar is the user's existing choice: Settings >
//! Personalisation > Taskbar > Other system tray icons. Windows keeps
//! that list in the registry, and the bar shows the ones switched on
//! there. The rest are where Windows puts them, in the overflow the
//! chevron opens.

use std::cell::RefCell;
use std::ffi::c_void;

use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    ClientToScreen, CreateCompatibleDC, CreateDIBSection, CreateSolidBrush, DeleteDC, DeleteObject,
    FillRect, GdiAlphaBlend, GdiFlush, GetObjectW, InvalidateRect, SelectObject, AC_SRC_ALPHA,
    AC_SRC_OVER, BITMAP, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, DIB_RGB_COLORS, HDC,
};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemInformation::GetTickCount64;
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime;
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, CopyIcon, CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyWindow,
    DrawIconEx, FindWindowExW, FindWindowW, GetCursorPos, GetIconInfo, GetWindowThreadProcessId,
    IsWindow, KillTimer, RegisterClassW, RegisterWindowMessageW, SendMessageTimeoutW,
    SendNotifyMessageW, SetTimer, SetWindowPos, DI_NORMAL, HICON, HWND_BROADCAST, HWND_TOPMOST,
    ICONINFO, SMTO_ABORTIFHUNG, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, WM_CONTEXTMENU,
    WM_COPYDATA, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_TIMER, WM_USER,
    WNDCLASSW, WS_CHILD, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

// ---------------------------------------------------------------------
// The wire format, and the icon list it maintains. Pure: no handles are
// dereferenced here, so all of it runs under test.
// ---------------------------------------------------------------------

/// First field of every tray `WM_COPYDATA` payload.
const TRAY_SIGNATURE: u32 = 0x3475_3423;

/// `COPYDATASTRUCT::dwData` values `Shell_TrayWnd` is sent.
const COPYDATA_NOTIFY_ICON: usize = 1;
const COPYDATA_ICON_IDENTIFIER: usize = 3;

const NIF_MESSAGE: u32 = 0x01;
const NIF_ICON: u32 = 0x02;
const NIF_TIP: u32 = 0x04;
const NIF_STATE: u32 = 0x08;
const NIF_GUID: u32 = 0x20;
const NIS_HIDDEN: u32 = 0x01;

/// `WM_USER + 0`: "the icon was activated", sent after a left click to
/// icons that asked for version 3 behaviour or later.
const NIN_SELECT: u32 = WM_USER;

/// Icons shown in the bar. The rest stay reachable through the chevron,
/// which opens Explorer's own overflow.
const MAX_SHOWN: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TrayOp {
    Add,
    Modify,
    Delete,
    SetFocus,
    SetVersion,
}

/// One `Shell_NotifyIcon` call, as it arrives.
///
/// The payload is a signature, the operation, then a `NOTIFYICONDATAW`
/// laid out for 32-bit handles whatever the bitness of either process:
/// the window and icon handles are four bytes each.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TrayMessage {
    pub(crate) op: TrayOp,
    pub(crate) hwnd: u32,
    pub(crate) id: u32,
    pub(crate) flags: u32,
    pub(crate) callback: u32,
    pub(crate) icon: u32,
    pub(crate) tip: String,
    pub(crate) state: u32,
    pub(crate) state_mask: u32,
    pub(crate) version: u32,
    pub(crate) guid: [u8; 16],
}

/// Offsets into the payload. `NID` is where the icon data starts.
mod layout {
    pub(super) const SIGNATURE: usize = 0;
    pub(super) const OP: usize = 4;
    pub(super) const NID: usize = 8;
    pub(super) const HWND: usize = NID + 4;
    pub(super) const ID: usize = NID + 8;
    pub(super) const FLAGS: usize = NID + 12;
    pub(super) const CALLBACK: usize = NID + 16;
    pub(super) const ICON: usize = NID + 20;
    pub(super) const TIP: usize = NID + 24;
    pub(super) const TIP_CHARS: usize = 128;
    pub(super) const STATE: usize = NID + 280;
    pub(super) const STATE_MASK: usize = NID + 284;
    pub(super) const VERSION: usize = NID + 800;
    pub(super) const GUID: usize = NID + 936;
    /// Everything up to and including the icon handle. A payload
    /// shorter than this cannot name an icon.
    pub(super) const MINIMUM: usize = ICON + 4;
}

fn u32_at(data: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(data.get(offset..offset + 4)?.try_into().ok()?))
}

/// Reads a tray payload. `None` for anything that is not one.
///
/// Fields past the icon handle are optional: whatever the payload is too
/// short to hold reads as zero or empty, which is what an application
/// that never set them would have sent.
pub(crate) fn parse_tray_message(data: &[u8]) -> Option<TrayMessage> {
    if data.len() < layout::MINIMUM || u32_at(data, layout::SIGNATURE)? != TRAY_SIGNATURE {
        return None;
    }
    let op = match u32_at(data, layout::OP)? {
        0 => TrayOp::Add,
        1 => TrayOp::Modify,
        2 => TrayOp::Delete,
        3 => TrayOp::SetFocus,
        4 => TrayOp::SetVersion,
        _ => return None,
    };

    let tip_bytes = data.get(layout::TIP..).unwrap_or(&[]);
    let tip_units: Vec<u16> = tip_bytes
        .chunks_exact(2)
        .take(layout::TIP_CHARS)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|&u| u != 0)
        .collect();

    let mut guid = [0u8; 16];
    if let Some(bytes) = data.get(layout::GUID..layout::GUID + 16) {
        guid.copy_from_slice(bytes);
    }

    Some(TrayMessage {
        op,
        hwnd: u32_at(data, layout::HWND)?,
        id: u32_at(data, layout::ID)?,
        flags: u32_at(data, layout::FLAGS)?,
        callback: u32_at(data, layout::CALLBACK)?,
        icon: u32_at(data, layout::ICON)?,
        tip: String::from_utf16_lossy(&tip_units),
        state: u32_at(data, layout::STATE).unwrap_or(0),
        state_mask: u32_at(data, layout::STATE_MASK).unwrap_or(0),
        version: u32_at(data, layout::VERSION).unwrap_or(0),
        guid,
    })
}

/// What identifies an icon. An application names it by GUID if it gave
/// one, and by its window and a number of its own choosing otherwise.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IconKey {
    Guid([u8; 16]),
    Window(u32, u32),
}

fn key_of(flags: u32, hwnd: u32, id: u32, guid: [u8; 16]) -> IconKey {
    if flags & NIF_GUID != 0 && guid != [0u8; 16] {
        IconKey::Guid(guid)
    } else {
        IconKey::Window(hwnd, id)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Icon {
    pub(crate) key: IconKey,
    pub(crate) hwnd: u32,
    pub(crate) id: u32,
    pub(crate) callback: u32,
    pub(crate) version: u32,
    pub(crate) tip: String,
    pub(crate) hidden: bool,
    /// The application's icon handle, as sent. It is the application's
    /// to destroy, so it is drawn into `pixels` as soon as it arrives
    /// and never kept.
    pub(crate) source_icon: u32,
    /// Set when `source_icon` changed and `pixels` is out of date.
    pub(crate) needs_render: bool,
    /// Premultiplied BGRA, `size` by `size`. Empty until rendered.
    pub(crate) pixels: Vec<u8>,
    pub(crate) size: i32,
    /// Where the bar last drew it, in the bar's client coordinates.
    pub(crate) rect: Option<RECT>,
    /// Full path of the program that owns the icon, lowercased. Empty
    /// until looked up, or if the process could not be opened.
    pub(crate) exe: String,
    /// Whether the user has this icon switched on for the taskbar.
    pub(crate) promoted: bool,
}

impl Icon {
    fn is_shown(&self) -> bool {
        !self.hidden && self.promoted && !self.pixels.is_empty()
    }
}

/// One entry of Windows' own list of notification icons: the program,
/// the icon's identity within it, and whether the user has it switched
/// on for the taskbar.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Preference {
    /// Lowercased full path, known-folder prefix already expanded.
    pub(crate) exe: String,
    pub(crate) uid: Option<u32>,
    pub(crate) guid: Option<[u8; 16]>,
    pub(crate) promoted: bool,
}

/// The user's "Other system tray icons" choices.
///
/// `None` when Windows keeps no such list (older builds). Everything is
/// shown then: hiding every icon because a list is missing would be
/// worse than showing a few the user might have preferred tucked away.
#[derive(Default)]
pub(crate) struct Preferences {
    pub(crate) entries: Option<Vec<Preference>>,
}

impl Preferences {
    /// Whether this icon belongs in the bar.
    ///
    /// An icon Windows has no entry for is new, and Windows starts new
    /// icons off in the overflow, so that is where it stays here too.
    pub(crate) fn promotes(&self, exe: &str, id: u32, key: IconKey) -> bool {
        let Some(entries) = &self.entries else { return true };
        entries.iter().any(|e| {
            e.promoted
                && e.exe == exe
                && match key {
                    IconKey::Guid(guid) => e.guid == Some(guid),
                    IconKey::Window(..) => e.guid.is_none() && e.uid == Some(id),
                }
        })
    }
}

/// Reads `{XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX}` into the byte order a
/// `GUID` has in memory, which is the order it arrives in on the wire.
pub(crate) fn parse_guid(text: &str) -> Option<[u8; 16]> {
    let hex: String = text.trim().trim_start_matches('{').trim_end_matches('}').replace('-', "");
    if hex.len() != 32 {
        return None;
    }
    let value = u128::from_str_radix(&hex, 16).ok()?.to_be_bytes();
    // The first three groups are stored little-endian; the rest as read.
    let mut out = [0u8; 16];
    out[0..4].copy_from_slice(&[value[3], value[2], value[1], value[0]]);
    out[4..6].copy_from_slice(&[value[5], value[4]]);
    out[6..8].copy_from_slice(&[value[7], value[6]]);
    out[8..16].copy_from_slice(&value[8..16]);
    Some(out)
}

/// Windows records a program under Program Files or the Windows
/// directory as a known-folder id and the path below it:
/// `{6D809377-...}\PowerToys\PowerToys.exe`. `folder` turns such an id
/// into its directory. A path with no id is returned as it is.
pub(crate) fn expand_known_folder(path: &str, mut folder: impl FnMut(&str) -> Option<String>) -> String {
    let expanded = match (path.starts_with('{'), path.find('}')) {
        (true, Some(end)) => match folder(&path[..=end]) {
            Some(directory) => format!("{}{}", directory.trim_end_matches('\\'), &path[end + 1..]),
            None => path.to_string(),
        },
        _ => path.to_string(),
    };
    expanded.to_lowercase()
}

/// The two places an icon can be drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Place {
    Bar,
    Overflow,
}

/// The icons applications have registered, oldest first.
#[derive(Default)]
pub(crate) struct IconStore {
    icons: Vec<Icon>,
}

impl IconStore {
    fn position(&self, key: IconKey) -> Option<usize> {
        self.icons.iter().position(|i| i.key == key)
    }

    /// Applies one message. Returns whether the bar needs repainting.
    pub(crate) fn apply(&mut self, msg: &TrayMessage) -> bool {
        let key = key_of(msg.flags, msg.hwnd, msg.id, msg.guid);
        match msg.op {
            TrayOp::Delete => match self.position(key) {
                Some(index) => {
                    self.icons.remove(index);
                    true
                }
                None => false,
            },
            TrayOp::SetVersion => {
                if let Some(index) = self.position(key) {
                    self.icons[index].version = msg.version;
                }
                false
            }
            TrayOp::SetFocus => false,
            // Add and Modify are one operation here. An application
            // re-adds every icon when told the taskbar was recreated,
            // and one that was running before the shell started may
            // only ever send Modify for an icon this host never saw
            // added. Either way the right answer is the icon, current.
            TrayOp::Add | TrayOp::Modify => {
                let index = self.position(key).unwrap_or_else(|| {
                    self.icons.push(Icon {
                        key,
                        hwnd: msg.hwnd,
                        id: msg.id,
                        callback: 0,
                        version: 0,
                        tip: String::new(),
                        hidden: false,
                        source_icon: 0,
                        needs_render: false,
                        pixels: Vec::new(),
                        size: 0,
                        rect: None,
                        exe: String::new(),
                        // Shown until the first check says otherwise,
                        // which is on the same message.
                        promoted: true,
                    });
                    self.icons.len() - 1
                });
                let icon = &mut self.icons[index];
                icon.hwnd = msg.hwnd;
                icon.id = msg.id;
                if msg.flags & NIF_MESSAGE != 0 {
                    icon.callback = msg.callback;
                }
                if msg.flags & NIF_ICON != 0 {
                    icon.source_icon = msg.icon;
                    icon.needs_render = true;
                }
                if msg.flags & NIF_TIP != 0 {
                    icon.tip = msg.tip.clone();
                }
                if msg.flags & NIF_STATE != 0 && msg.state_mask & NIS_HIDDEN != 0 {
                    icon.hidden = msg.state & NIS_HIDDEN != 0;
                }
                true
            }
        }
    }

    /// The icons the bar shows, in bar order: not hidden, and with a
    /// picture to draw.
    fn shown(&self) -> impl Iterator<Item = &Icon> {
        self.icons.iter().filter(|i| i.is_shown()).take(MAX_SHOWN)
    }

    /// Everything else that has a picture: the icons the user has not
    /// switched on for the bar, and any that did not fit in it. These
    /// are what the chevron's panel shows.
    fn overflow(&self) -> Vec<&Icon> {
        let in_bar: Vec<IconKey> = self.shown().map(|i| i.key).collect();
        self.icons
            .iter()
            .filter(|i| !i.hidden && !i.pixels.is_empty() && !in_bar.contains(&i.key))
            .collect()
    }

    /// Icon `index` of the bar, or of the chevron's panel.
    fn pick(&self, place: Place, index: usize) -> Option<&Icon> {
        match place {
            Place::Bar => self.shown().nth(index),
            Place::Overflow => self.overflow().get(index).copied(),
        }
    }

    fn shown_mut(&mut self) -> impl Iterator<Item = &mut Icon> {
        self.icons.iter_mut().filter(|i| i.is_shown()).take(MAX_SHOWN)
    }

    /// Re-reads each icon's "show in taskbar" setting. Returns whether
    /// any changed.
    fn apply_preferences(&mut self, preferences: &Preferences) -> bool {
        let mut changed = false;
        for icon in &mut self.icons {
            let promoted = preferences.promotes(&icon.exe, icon.id, icon.key);
            changed |= promoted != icon.promoted;
            icon.promoted = promoted;
        }
        changed
    }

    /// Drops icons for which `alive` says the window is gone. An
    /// application that crashes never sends its Delete.
    fn prune(&mut self, alive: impl Fn(u32) -> bool) -> bool {
        let before = self.icons.len();
        self.icons.retain(|i| alive(i.hwnd));
        self.icons.len() != before
    }
}

/// `wParam`/`lParam` for delivering `event` to an icon's window.
///
/// Version 4 icons get the pointer position in `wParam` and the icon's
/// id packed above the event; older ones get the id in `wParam` and the
/// bare event. An application picks which with `NIM_SETVERSION`, and
/// reads its messages accordingly, so sending the wrong shape is a click
/// that silently does nothing.
pub(crate) fn callback_params(version: u32, id: u32, event: u32, x: i32, y: i32) -> (usize, isize) {
    if version > 3 {
        let wparam = (x as u16 as usize) | ((y as u16 as usize) << 16);
        let lparam = ((event & 0xFFFF) | ((id & 0xFFFF) << 16)) as isize;
        (wparam, lparam)
    } else {
        (id as usize, event as isize)
    }
}

// ---------------------------------------------------------------------
// The host window
// ---------------------------------------------------------------------

const KEEP_ON_TOP_TIMER: usize = 1;
const KEEP_ON_TOP_MS: u32 = 250;

struct Host {
    hwnd: isize,
    /// The primary bar: where the icons are drawn, and what to repaint.
    bar: isize,
    store: IconStore,
    preferences: Preferences,
    /// Seconds until the preferences are read again.
    preferences_age: u32,
}

/// How often the user's choices are re-read, in `refresh` calls (one a
/// second). Soon enough that flipping a switch in Settings shows up
/// while the Settings window is still open.
const PREFERENCES_EVERY: u32 = 3;

thread_local! {
    static HOST: RefCell<Host> = const {
        RefCell::new(Host {
            hwnd: 0,
            bar: 0,
            store: IconStore { icons: Vec::new() },
            preferences: Preferences { entries: None },
            preferences_age: 0,
        })
    };
}

/// Full path of the program that owns `hwnd`, lowercased.
fn exe_of_window(hwnd: u32) -> String {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    // SAFETY: a stale window gives pid 0 and an empty answer; the process
    // handle is closed before returning.
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd_from(hwnd), Some(&mut pid));
        if pid == 0 {
            return String::new();
        }
        let Ok(process) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return String::new();
        };
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(process, PROCESS_NAME_FORMAT(0), windows::core::PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
        let _ = CloseHandle(process);
        if ok { String::from_utf16_lossy(&buf[..len as usize]).to_lowercase() } else { String::new() }
    }
}

/// A known folder's directory, from its `{id}`.
fn known_folder(id: &str) -> Option<String> {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{SHGetKnownFolderPath, KNOWN_FOLDER_FLAG};
    let bytes = parse_guid(id)?;
    // SAFETY: `bytes` is a GUID in memory order; the returned string is
    // freed once copied.
    unsafe {
        let guid = std::mem::transmute::<[u8; 16], windows::core::GUID>(bytes);
        let path = SHGetKnownFolderPath(&guid, KNOWN_FOLDER_FLAG(0), None).ok()?;
        let text = path.to_string().ok();
        CoTaskMemFree(Some(path.0 as *const c_void));
        text
    }
}

/// Reads Windows' list of notification icons and the user's choice for
/// each.
fn read_preferences() -> Preferences {
    use windows::core::{HSTRING, PCWSTR, PWSTR};
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegEnumKeyExW, RegGetValueW, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, KEY_READ,
        RRF_RT_REG_DWORD, RRF_RT_REG_SZ,
    };

    // SAFETY: every buffer passed is a live local of the size given; the
    // keys opened are closed before returning.
    unsafe {
        let mut root = HKEY::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, w!(r"Control Panel\NotifyIconSettings"), 0, KEY_READ, &mut root) != ERROR_SUCCESS {
            return Preferences { entries: None };
        }

        let dword = |sub: &HSTRING, name: PCWSTR| -> Option<u32> {
            let mut data = 0u32;
            let mut len = 4u32;
            (RegGetValueW(root, sub, name, RRF_RT_REG_DWORD, None, Some(&mut data as *mut _ as *mut c_void), Some(&mut len))
                == ERROR_SUCCESS)
                .then_some(data)
        };
        let string = |sub: &HSTRING, name: PCWSTR| -> Option<String> {
            let mut buf = [0u16; 1024];
            let mut len = (buf.len() * 2) as u32;
            (RegGetValueW(root, sub, name, RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr() as *mut c_void), Some(&mut len))
                == ERROR_SUCCESS)
                .then(|| {
                    let units = (len as usize / 2).min(buf.len());
                    String::from_utf16_lossy(&buf[..units]).trim_end_matches('\0').to_string()
                })
        };

        let mut entries = Vec::new();
        let mut folders: std::collections::HashMap<String, Option<String>> = std::collections::HashMap::new();
        for index in 0.. {
            let mut name = [0u16; 64];
            let mut name_len = name.len() as u32;
            if RegEnumKeyExW(root, index, PWSTR(name.as_mut_ptr()), &mut name_len, None, PWSTR::null(), None, None) != ERROR_SUCCESS {
                break;
            }
            let sub = HSTRING::from_wide(&name[..name_len as usize]).unwrap_or_default();
            let Some(path) = string(&sub, w!("ExecutablePath")) else { continue };
            let exe = expand_known_folder(&path, |id| folders.entry(id.to_string()).or_insert_with(|| known_folder(id)).clone());
            entries.push(Preference {
                exe,
                uid: dword(&sub, w!("UID")),
                guid: string(&sub, w!("IconGuid")).and_then(|g| parse_guid(&g)),
                promoted: dword(&sub, w!("IsPromoted")) == Some(1),
            });
        }
        let _ = RegCloseKey(root);
        Preferences { entries: Some(entries) }
    }
}

fn hwnd_from(value: u32) -> HWND {
    HWND(value as usize as *mut c_void)
}

/// Explorer's own `Shell_TrayWnd`: the one that is not ours.
///
/// Everything that used to say `FindWindow("Shell_TrayWnd")` meaning
/// "the taskbar" has to come through here now, because that lookup
/// finds this module's window first. That is the point of it.
pub(crate) fn explorer_tray() -> Option<HWND> {
    // SAFETY: plain enumeration of top-level windows by class.
    unsafe {
        let ours = GetCurrentProcessId();
        let mut previous = HWND(std::ptr::null_mut());
        loop {
            let next = FindWindowExW(HWND(std::ptr::null_mut()), previous, w!("Shell_TrayWnd"), None).ok()?;
            if next.0.is_null() {
                return None;
            }
            let mut pid = 0u32;
            GetWindowThreadProcessId(next, Some(&mut pid));
            if pid != ours {
                return Some(next);
            }
            previous = next;
        }
    }
}

/// Creates the host window and announces it. Called once at startup.
///
/// The announcement is the `TaskbarCreated` broadcast Explorer itself
/// sends when it restarts: every well-behaved application answers by
/// adding its icons again, and this time the adds arrive here.
pub(crate) fn prepare() {
    // SAFETY: registers a class and creates windows on the calling (UI)
    // thread, which owns them for the life of the process.
    unsafe {
        let Ok(module) = GetModuleHandleW(None) else { return };
        let class = WNDCLASSW {
            lpfnWndProc: Some(host_proc),
            hInstance: module.into(),
            lpszClassName: w!("Shell_TrayWnd"),
            ..Default::default()
        };
        RegisterClassW(&class);
        // Never shown: the lookup that matters finds hidden windows too.
        // Tool window, so it stays out of Alt+Tab and out of the shell's
        // own window list.
        let Ok(hwnd) = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            w!("Shell_TrayWnd"),
            w!(""),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            module,
            None,
        ) else {
            tracing::warn!("tray: could not create the notification-area host");
            return;
        };

        // Some applications walk to the notification area by class to
        // decide where to put a flyout. Give them one to find.
        let child = WNDCLASSW {
            lpfnWndProc: Some(child_proc),
            hInstance: module.into(),
            lpszClassName: w!("TrayNotifyWnd"),
            ..Default::default()
        };
        RegisterClassW(&child);
        let _ = CreateWindowExW(Default::default(), w!("TrayNotifyWnd"), w!(""), WS_CHILD, 0, 0, 1, 1, hwnd, None, module, None);

        let preferences = read_preferences();
        HOST.with(|h| {
            let mut h = h.borrow_mut();
            h.hwnd = hwnd.0 as isize;
            h.preferences = preferences;
        });
        keep_on_top(hwnd);
        SetTimer(hwnd, KEEP_ON_TOP_TIMER, KEEP_ON_TOP_MS, None);

        let taskbar_created = RegisterWindowMessageW(w!("TaskbarCreated"));
        if taskbar_created != 0 {
            let _ = SendNotifyMessageW(HWND_BROADCAST, taskbar_created, WPARAM(0), LPARAM(0));
        }
        tracing::info!(found_first = FindWindowW(w!("Shell_TrayWnd"), None).ok() == Some(hwnd), "tray: hosting the notification area");
    }
}

/// Tells the host which bar draws the icons, and places the host window
/// over it so an application that asks where the taskbar is gets the
/// bar's rectangle.
pub(crate) fn attach_bar(bar: HWND, bar_rect: RECT) {
    let host = HOST.with(|h| {
        let mut h = h.borrow_mut();
        h.bar = bar.0 as isize;
        h.hwnd
    });
    if host == 0 {
        return;
    }
    // SAFETY: `host` is this module's own live window.
    unsafe {
        let _ = SetWindowPos(
            HWND(host as *mut c_void),
            HWND_TOPMOST,
            bar_rect.left,
            bar_rect.top,
            bar_rect.right - bar_rect.left,
            bar_rect.bottom - bar_rect.top,
            SWP_NOACTIVATE,
        );
    }
}

/// Gives the notification area back to Explorer. Explorer has been sent
/// every message this host received, so it has nothing to catch up on.
pub(crate) fn shutdown() {
    let host = HOST.with(|h| {
        let mut h = h.borrow_mut();
        h.store.icons.clear();
        std::mem::take(&mut h.hwnd)
    });
    if host != 0 {
        // SAFETY: this module's own window, destroyed on its own thread.
        unsafe {
            let hwnd = HWND(host as *mut c_void);
            let _ = KillTimer(hwnd, KEEP_ON_TOP_TIMER);
            let _ = DestroyWindow(hwnd);
        }
    }
}

/// Puts the host back above Explorer's tray if Explorer has moved in
/// front. `FindWindow` returns the first match in z-order, so "above" is
/// the whole mechanism.
unsafe fn keep_on_top(hwnd: HWND) {
    if FindWindowW(w!("Shell_TrayWnd"), None).ok() != Some(hwnd) {
        let _ = SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
    }
}

/// Passes a message on to Explorer's tray and returns its answer.
///
/// With a timeout: this runs on the UI thread, and a hung Explorer must
/// cost a pause, not the shell.
unsafe fn forward(msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let Some(explorer) = explorer_tray() else { return LRESULT(0) };
    let mut result = 0usize;
    let _ = SendMessageTimeoutW(explorer, msg, wparam, lparam, SMTO_ABORTIFHUNG, 1000, Some(&mut result));
    LRESULT(result as isize)
}

unsafe extern "system" fn child_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

unsafe extern "system" fn host_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_COPYDATA => {
            let copy = &*(lparam.0 as *const COPYDATASTRUCT);
            let data: &[u8] = if copy.lpData.is_null() {
                &[]
            } else {
                std::slice::from_raw_parts(copy.lpData as *const u8, copy.cbData as usize)
            };
            match copy.dwData {
                COPYDATA_NOTIFY_ICON => {
                    let handled = on_icon_message(data);
                    let explorer = forward(msg, wparam, lparam);
                    // Explorer refuses an Add for an icon it already
                    // has, which is every icon re-added after the
                    // announcement. The application asked this host,
                    // and this host took it.
                    if handled { LRESULT(1) } else { explorer }
                }
                COPYDATA_ICON_IDENTIFIER => match icon_rect_answer(data) {
                    Some(answer) => LRESULT(answer),
                    None => forward(msg, wparam, lparam),
                },
                // AppBar messages and anything else addressed to the
                // taskbar proper: Explorer's business.
                _ => forward(msg, wparam, lparam),
            }
        }
        WM_TIMER if wparam.0 == KEEP_ON_TOP_TIMER => {
            keep_on_top(hwnd);
            LRESULT(0)
        }
        // Private taskbar messages sent by window class. Registered
        // messages (0xC000 up) are broadcasts Explorer receives anyway.
        m if (WM_USER..0xC000).contains(&m) => forward(msg, wparam, lparam),
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// Applies one icon message. Returns whether it was one.
fn on_icon_message(data: &[u8]) -> bool {
    let Some(message) = parse_tray_message(data) else { return false };
    let bar = HOST.with(|h| {
        let mut h = h.borrow_mut();
        let changed = h.store.apply(&message);
        let Host { store, preferences, .. } = &mut *h;
        for icon in store.icons.iter_mut().filter(|i| i.exe.is_empty()) {
            icon.exe = exe_of_window(icon.hwnd);
            icon.promoted = preferences.promotes(&icon.exe, icon.id, icon.key);
        }
        for icon in h.store.icons.iter_mut().filter(|i| i.needs_render) {
            icon.needs_render = false;
            // SAFETY: the handle came from the application in this very
            // message, so it is valid now; it is copied before use and
            // not kept.
            match unsafe { render_icon(icon.source_icon) } {
                Some((pixels, size)) => {
                    icon.pixels = pixels;
                    icon.size = size;
                }
                // Keep the previous picture: a failed read of a new
                // handle should not blank an icon that was fine.
                None => {}
            }
        }
        changed.then_some(h.bar)
    });
    if let Some(bar) = bar.filter(|b| *b != 0) {
        // SAFETY: the primary bar is a process-lifetime window.
        unsafe {
            let _ = InvalidateRect(HWND(bar as *mut c_void), None, false);
        }
    }
    true
}

/// Answers `Shell_NotifyIconGetRect` for an icon the bar is showing:
/// one corner of its screen rectangle per request, packed as a point.
/// `None` for an icon this host is not drawing.
fn icon_rect_answer(data: &[u8]) -> Option<isize> {
    if u32_at(data, 0)? != TRAY_SIGNATURE {
        return None;
    }
    let which = u32_at(data, 4)?;
    let hwnd = u32_at(data, 16)?;
    let id = u32_at(data, 20)?;
    let mut guid = [0u8; 16];
    guid.copy_from_slice(data.get(24..40)?);

    let (bar, rect) = HOST.with(|h| {
        let h = h.borrow();
        let icon = h.store.shown().find(|i| match i.key {
            IconKey::Guid(g) => g == guid,
            IconKey::Window(w, n) => w == hwnd && n == id,
        })?;
        Some((h.bar, icon.rect?))
    })?;
    let (mut top_left, mut bottom_right) =
        (POINT { x: rect.left, y: rect.top }, POINT { x: rect.right, y: rect.bottom });
    // SAFETY: the primary bar is a process-lifetime window.
    unsafe {
        let bar = HWND(bar as *mut c_void);
        let _ = ClientToScreen(bar, &mut top_left);
        let _ = ClientToScreen(bar, &mut bottom_right);
    }
    let point = match which {
        1 => top_left,
        2 => bottom_right,
        _ => return None,
    };
    Some(((point.x as u16 as u32) | ((point.y as u16 as u32) << 16)) as isize)
}

/// A zeroed top-down 32-bit bitmap selected into its own DC.
unsafe fn dib(size: i32) -> Option<(HDC, windows::Win32::Graphics::Gdi::HBITMAP, *mut u8)> {
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: size,
            biHeight: -size,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let dc = CreateCompatibleDC(None);
    if dc.0.is_null() {
        return None;
    }
    let mut bits = std::ptr::null_mut();
    match CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, None, 0) {
        Ok(bitmap) if !bits.is_null() => {
            SelectObject(dc, bitmap);
            Some((dc, bitmap, bits as *mut u8))
        }
        Ok(bitmap) => {
            let _ = DeleteObject(bitmap);
            let _ = DeleteDC(dc);
            None
        }
        Err(_) => {
            let _ = DeleteDC(dc);
            None
        }
    }
}

/// Draws an application's icon into premultiplied BGRA.
///
/// Drawn twice, on black and on white. An icon may carry real alpha or
/// only a one-bit mask, and a single draw onto a cleared bitmap gets
/// alpha for the first kind and none at all for the second, which then
/// vanishes. The difference between the two draws is the coverage in
/// both cases: a pixel that looks the same on black and on white is
/// opaque, one that takes the background's colour is clear.
///
/// # Safety
/// `source` must be an icon handle that is valid right now.
unsafe fn render_icon(source: u32) -> Option<(Vec<u8>, i32)> {
    // The handle is the application's and may be destroyed at any time
    // after this message returns. Work on a copy.
    let icon = CopyIcon(HICON(source as usize as *mut c_void)).ok()?;

    let mut size = 32;
    let mut info = ICONINFO::default();
    if GetIconInfo(icon, &mut info).is_ok() {
        let mut bm = BITMAP::default();
        let measured = if info.hbmColor.is_invalid() { info.hbmMask } else { info.hbmColor };
        if GetObjectW(measured, std::mem::size_of::<BITMAP>() as i32, Some(&mut bm as *mut _ as *mut c_void)) != 0 {
            size = bm.bmWidth;
        }
        let _ = DeleteObject(info.hbmColor);
        let _ = DeleteObject(info.hbmMask);
    }
    let size = size.clamp(16, 64);
    let len = (size * size * 4) as usize;

    let result = (|| {
        let (black_dc, black_bmp, black_bits) = dib(size)?;
        let Some((white_dc, white_bmp, white_bits)) = dib(size) else {
            let _ = DeleteDC(black_dc);
            let _ = DeleteObject(black_bmp);
            return None;
        };
        let white = CreateSolidBrush(COLORREF(0x00FF_FFFF));
        FillRect(white_dc, &RECT { left: 0, top: 0, right: size, bottom: size }, white);
        let _ = DeleteObject(white);

        let drawn = DrawIconEx(black_dc, 0, 0, icon, size, size, 0, None, DI_NORMAL).is_ok()
            && DrawIconEx(white_dc, 0, 0, icon, size, size, 0, None, DI_NORMAL).is_ok();
        let _ = GdiFlush();

        let pixels = drawn.then(|| {
            let on_black = std::slice::from_raw_parts(black_bits, len);
            let on_white = std::slice::from_raw_parts(white_bits, len);
            coverage_from_two_draws(on_black, on_white)
        });
        let _ = DeleteDC(black_dc);
        let _ = DeleteDC(white_dc);
        let _ = DeleteObject(black_bmp);
        let _ = DeleteObject(white_bmp);
        pixels
    })();
    let _ = DestroyIcon(icon);
    result.map(|pixels| (pixels, size))
}

/// Recovers premultiplied BGRA from the same image composited on black
/// and on white.
///
/// On black, a pixel is `colour * alpha`: already the premultiplied
/// colour. On white it is that plus `255 * (1 - alpha)`. So the
/// difference between the two is `255 - alpha` in every channel.
fn coverage_from_two_draws(on_black: &[u8], on_white: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(on_black.len());
    for (b, w) in on_black.chunks_exact(4).zip(on_white.chunks_exact(4)) {
        // The channels agree up to rounding; green has the most signal.
        let alpha = 255u8.saturating_sub(w[1].saturating_sub(b[1]));
        out.extend_from_slice(&[b[0].min(alpha), b[1].min(alpha), b[2].min(alpha), alpha]);
    }
    out
}

// ---------------------------------------------------------------------
// What the bar uses
// ---------------------------------------------------------------------

/// Once a second: forget icons whose application has gone without
/// saying so, and every few seconds pick up what the user has changed
/// in Settings.
pub(crate) fn refresh() {
    let due = HOST.with(|h| {
        let mut h = h.borrow_mut();
        h.preferences_age += 1;
        let due = h.preferences_age >= PREFERENCES_EVERY;
        if due {
            h.preferences_age = 0;
        }
        due
    });
    // Read outside the borrow: the registry and shell calls are slow
    // enough to be worth keeping out of it.
    let fresh = due.then(read_preferences);
    let bar = HOST.with(|h| {
        let mut h = h.borrow_mut();
        // SAFETY: a plain query; a stale handle reports false.
        let mut changed = h.store.prune(|hwnd| unsafe { IsWindow(hwnd_from(hwnd)).as_bool() });
        if let Some(fresh) = fresh {
            h.preferences = fresh;
            let Host { store, preferences, .. } = &mut *h;
            changed |= store.apply_preferences(preferences);
        }
        let pruned = changed;
        pruned.then_some(h.bar)
    });
    if let Some(bar) = bar.filter(|b| *b != 0) {
        // SAFETY: the primary bar is a process-lifetime window.
        unsafe {
            let _ = InvalidateRect(HWND(bar as *mut c_void), None, false);
        }
    }
}

pub(crate) fn count() -> usize {
    HOST.with(|h| h.borrow().store.shown().count())
}

/// The icon's tooltip.
pub(crate) fn name(index: usize) -> String {
    HOST.with(|h| h.borrow().store.shown().nth(index).map(|i| i.tip.clone()).unwrap_or_default())
}

/// Records where the bar drew icon `index`, so an application asking
/// where its icon is can be told.
pub(crate) fn note_rect(index: usize, rect: RECT) {
    HOST.with(|h| {
        if let Some(icon) = h.borrow_mut().store.shown_mut().nth(index) {
            icon.rect = Some(rect);
        }
    });
}

thread_local! {
    /// The last left click delivered: which icon, and when.
    static LAST_LEFT_CLICK: std::cell::Cell<Option<(u32, u32, u64)>> = const { std::cell::Cell::new(None) };
}

/// Whether a left click on `icon` at `now` completes a double click.
pub(crate) fn is_double_click(previous: Option<(u32, u32, u64)>, icon: (u32, u32), now: u64, window_ms: u64) -> bool {
    previous.is_some_and(|(hwnd, id, at)| (hwnd, id) == icon && now.saturating_sub(at) <= window_ms)
}

/// Delivers a click to the icon's application: the button going down
/// and up, then the activation or context-menu notification that newer
/// icons act on.
pub(crate) fn invoke(index: usize, context: bool) {
    invoke_in(Place::Bar, index, context);
}

/// How many icons the chevron's panel holds.
pub(crate) fn overflow_count() -> usize {
    HOST.with(|h| h.borrow().store.overflow().len())
}

/// The tooltip of icon `index` in the chevron's panel.
pub(crate) fn overflow_name(index: usize) -> String {
    HOST.with(|h| h.borrow().store.pick(Place::Overflow, index).map(|i| i.tip.clone()).unwrap_or_default())
}

/// [`invoke`] for an icon in the chevron's panel.
pub(crate) fn overflow_invoke(index: usize, context: bool) {
    invoke_in(Place::Overflow, index, context);
}

/// [`paint`] for an icon in the chevron's panel.
///
/// # Safety
/// `hdc` must be a valid device context.
pub(crate) unsafe fn overflow_paint(hdc: HDC, index: usize, rect: RECT) {
    paint_in(Place::Overflow, hdc, index, rect);
}

fn invoke_in(place: Place, index: usize, context: bool) {
    let Some((hwnd, id, callback, version)) = HOST.with(|h| {
        h.borrow().store.pick(place, index).map(|i| (i.hwnd, i.id, i.callback, i.version))
    }) else {
        return;
    };
    if callback == 0 {
        return;
    }
    // A second left click soon after the first is a double click, and
    // has to be sent as one: plenty of applications open their window
    // only on that, and would otherwise never open it from here.
    // SAFETY: plain queries.
    let (now, window) = unsafe { (GetTickCount64(), GetDoubleClickTime() as u64) };
    let double = !context
        && LAST_LEFT_CLICK.with(|last| {
            let previous = last.replace(Some((hwnd, id, now)));
            is_double_click(previous, (hwnd, id), now, window)
        });
    if double {
        // A third click starts over rather than doubling again.
        LAST_LEFT_CLICK.with(|last| last.set(None));
    }
    let events: &[u32] = if context {
        &[WM_RBUTTONDOWN, WM_RBUTTONUP, WM_CONTEXTMENU]
    } else if double {
        &[WM_LBUTTONDBLCLK, WM_LBUTTONUP]
    } else {
        &[WM_LBUTTONDOWN, WM_LBUTTONUP, NIN_SELECT]
    };
    // SAFETY: `target` may have died since the last refresh; every call
    // below fails harmlessly on a stale handle.
    unsafe {
        let target = hwnd_from(hwnd);
        // The application is about to open a menu or a window in answer
        // to a click it did not receive itself. Without this it cannot
        // take the foreground, and a menu that is not foreground never
        // closes when the user clicks away.
        let mut pid = 0u32;
        GetWindowThreadProcessId(target, Some(&mut pid));
        if pid != 0 {
            let _ = AllowSetForegroundWindow(pid);
        }
        let mut cursor = POINT::default();
        let _ = GetCursorPos(&mut cursor);
        for (n, &event) in events.iter().enumerate() {
            // The third message is the notification, which only icons
            // that opted in to version 3 or later understand.
            if n == 2 && version < 3 {
                continue;
            }
            let (wparam, lparam) = callback_params(version, id, event, cursor.x, cursor.y);
            let _ = SendNotifyMessageW(target, callback, WPARAM(wparam), LPARAM(lparam));
        }
    }
}

/// Hands icon `index`'s premultiplied BGRA pixels to `f`, for the
/// Direct2D bar.
pub(crate) fn with_pixels<R>(index: usize, f: impl FnOnce(&[u8], i32) -> R) -> Option<R> {
    HOST.with(|h| {
        let h = h.borrow();
        let icon = h.store.shown().nth(index)?;
        Some(f(&icon.pixels, icon.size))
    })
}

/// Draws icon `index` for the GDI bar.
///
/// # Safety
/// `hdc` must be a valid device context.
pub(crate) unsafe fn paint(hdc: HDC, index: usize, rect: RECT) {
    paint_in(Place::Bar, hdc, index, rect);
}

unsafe fn paint_in(place: Place, hdc: HDC, index: usize, rect: RECT) {
    HOST.with(|h| {
        let h = h.borrow();
        let Some(icon) = h.store.pick(place, index) else { return };
        let Some((dc, bitmap, bits)) = dib(icon.size) else { return };
        std::ptr::copy_nonoverlapping(icon.pixels.as_ptr(), bits, icon.pixels.len());
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        let _ = GdiAlphaBlend(
            hdc,
            rect.left,
            rect.top,
            rect.right - rect.left,
            rect.bottom - rect.top,
            dc,
            0,
            0,
            icon.size,
            icon.size,
            blend,
        );
        let _ = DeleteDC(dc);
        let _ = DeleteObject(bitmap);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a payload the way `Shell_NotifyIcon` lays one out.
    fn payload(op: u32, hwnd: u32, id: u32, flags: u32, icon: u32, tip: &str) -> Vec<u8> {
        let mut data = vec![0u8; layout::NID + 956];
        let mut put = |offset: usize, value: u32| data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        put(layout::SIGNATURE, TRAY_SIGNATURE);
        put(layout::OP, op);
        put(layout::NID, 956);
        put(layout::HWND, hwnd);
        put(layout::ID, id);
        put(layout::FLAGS, flags);
        put(layout::CALLBACK, 0x8001);
        put(layout::ICON, icon);
        for (i, unit) in tip.encode_utf16().enumerate() {
            data[layout::TIP + i * 2..layout::TIP + i * 2 + 2].copy_from_slice(&unit.to_le_bytes());
        }
        data
    }

    fn message(op: u32, hwnd: u32, id: u32, flags: u32, icon: u32, tip: &str) -> TrayMessage {
        parse_tray_message(&payload(op, hwnd, id, flags, icon, tip)).expect("a well-formed payload")
    }

    const ALL: u32 = NIF_MESSAGE | NIF_ICON | NIF_TIP;

    #[test]
    fn a_payload_reads_back_field_by_field() {
        let m = message(0, 0x000A_0BCC, 7, ALL, 0x0001_0203, "Sync is up to date");
        assert_eq!(m.op, TrayOp::Add);
        assert_eq!((m.hwnd, m.id, m.callback, m.icon), (0x000A_0BCC, 7, 0x8001, 0x0001_0203));
        assert_eq!(m.tip, "Sync is up to date");
    }

    #[test]
    fn anything_else_arriving_as_copydata_is_not_an_icon_message() {
        assert_eq!(parse_tray_message(&[]), None);
        assert_eq!(parse_tray_message(&[0u8; 400]), None, "no signature");
        let mut wrong_op = payload(0, 1, 1, ALL, 1, "");
        wrong_op[layout::OP] = 9;
        assert_eq!(parse_tray_message(&wrong_op), None);
        let truncated = &payload(0, 1, 1, ALL, 1, "")[..20];
        assert_eq!(parse_tray_message(truncated), None);
    }

    #[test]
    fn a_short_payload_still_names_its_icon() {
        // An old application's smaller struct: nothing past the tooltip.
        let short = &payload(0, 5, 2, ALL, 9, "Old app")[..layout::TIP + 128];
        let m = parse_tray_message(short).expect("enough to identify the icon");
        assert_eq!((m.hwnd, m.id, m.tip.as_str()), (5, 2, "Old app"));
        assert_eq!((m.state, m.version), (0, 0));
    }

    #[test]
    fn modify_changes_only_what_its_flags_name() {
        let mut store = IconStore::default();
        store.apply(&message(0, 10, 1, ALL, 100, "First"));
        // A tooltip-only update must not disturb the icon or callback.
        store.apply(&message(1, 10, 1, NIF_TIP, 0, "Second"));
        let icon = &store.icons[0];
        assert_eq!((icon.tip.as_str(), icon.source_icon, icon.callback), ("Second", 100, 0x8001));
    }

    #[test]
    fn an_icon_only_ever_modified_is_adopted() {
        // Running before the shell started, and never told to re-add.
        let mut store = IconStore::default();
        assert!(store.apply(&message(1, 10, 1, ALL, 100, "Late")));
        assert_eq!(store.icons.len(), 1);
    }

    #[test]
    fn adding_twice_is_one_icon() {
        // What every application does on `TaskbarCreated`.
        let mut store = IconStore::default();
        store.apply(&message(0, 10, 1, ALL, 100, "Once"));
        store.apply(&message(0, 10, 1, ALL, 101, "Twice"));
        assert_eq!(store.icons.len(), 1);
        assert_eq!(store.icons[0].source_icon, 101);
    }

    #[test]
    fn delete_removes_the_icon_and_only_that_one() {
        let mut store = IconStore::default();
        store.apply(&message(0, 10, 1, ALL, 100, "A"));
        store.apply(&message(0, 10, 2, ALL, 100, "B"));
        assert!(store.apply(&message(2, 10, 1, 0, 0, "")));
        assert_eq!(store.icons.iter().map(|i| i.tip.as_str()).collect::<Vec<_>>(), vec!["B"]);
        assert!(!store.apply(&message(2, 10, 1, 0, 0, "")), "already gone");
    }

    fn preference(exe: &str, uid: Option<u32>, guid: Option<[u8; 16]>, promoted: bool) -> Preference {
        Preference { exe: exe.to_string(), uid, guid, promoted }
    }

    #[test]
    fn only_icons_the_user_switched_on_are_promoted() {
        let prefs = Preferences {
            entries: Some(vec![
                preference(r"c:\apps\sync.exe", Some(501), None, true),
                preference(r"c:\apps\chat.exe", Some(1), None, false),
            ]),
        };
        assert!(prefs.promotes(r"c:\apps\sync.exe", 501, IconKey::Window(9, 501)));
        assert!(!prefs.promotes(r"c:\apps\chat.exe", 1, IconKey::Window(9, 1)), "switched off");
        // The same program's other icon is a separate choice.
        assert!(!prefs.promotes(r"c:\apps\sync.exe", 2, IconKey::Window(9, 2)));
        // New to Windows: it starts in the overflow.
        assert!(!prefs.promotes(r"c:\apps\new.exe", 1, IconKey::Window(9, 1)));
    }

    #[test]
    fn a_guid_icon_is_matched_by_its_guid_not_its_number() {
        let guid = parse_guid("{699E423D-73DA-42BA-B94E-881FF8672467}").unwrap();
        let prefs = Preferences { entries: Some(vec![preference(r"c:\apps\chat.exe", None, Some(guid), true)]) };
        assert!(prefs.promotes(r"c:\apps\chat.exe", 0, IconKey::Guid(guid)));
        assert!(!prefs.promotes(r"c:\apps\chat.exe", 0, IconKey::Window(9, 0)));
    }

    #[test]
    fn with_no_list_from_windows_everything_is_shown() {
        let prefs = Preferences { entries: None };
        assert!(prefs.promotes(r"c:\apps\any.exe", 1, IconKey::Window(9, 1)));
    }

    #[test]
    fn a_guid_reads_into_memory_order() {
        let bytes = parse_guid("{00112233-4455-6677-8899-AABBCCDDEEFF}").unwrap();
        assert_eq!(bytes, [0x33, 0x22, 0x11, 0x00, 0x55, 0x44, 0x77, 0x66, 0x88, 0x99, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);
        assert_eq!(parse_guid("not a guid"), None);
    }

    #[test]
    fn a_known_folder_prefix_becomes_its_directory() {
        let folder = |id: &str| (id == "{6D809377-6AF0-444B-8957-A3773F02200E}").then(|| r"C:\Program Files".to_string());
        assert_eq!(
            expand_known_folder(r"{6D809377-6AF0-444B-8957-A3773F02200E}\PowerToys\PowerToys.exe", folder),
            r"c:\program files\powertoys\powertoys.exe"
        );
        // An ordinary path only changes case.
        assert_eq!(expand_known_folder(r"C:\Users\A\App.exe", folder), r"c:\users\a\app.exe");
        // An id nobody can resolve is left in place, and so never matches.
        assert_eq!(expand_known_folder(r"{DEAD}\x.exe", folder), r"{dead}\x.exe");
    }

    #[test]
    fn a_hidden_or_pictureless_icon_is_not_shown() {
        let mut store = IconStore::default();
        store.apply(&message(0, 10, 1, ALL, 100, "Visible"));
        store.apply(&message(0, 10, 2, ALL, 100, "Hidden"));
        store.apply(&message(0, 10, 3, NIF_TIP, 0, "No picture"));
        for icon in store.icons.iter_mut().take(2) {
            icon.pixels = vec![0; 4];
        }
        let mut hide = message(1, 10, 2, NIF_STATE, 0, "");
        hide.state = NIS_HIDDEN;
        hide.state_mask = NIS_HIDDEN;
        store.apply(&hide);
        assert_eq!(store.shown().map(|i| i.tip.as_str()).collect::<Vec<_>>(), vec!["Visible"]);
    }

    #[test]
    fn a_guid_names_the_icon_whatever_window_sends_it() {
        let mut store = IconStore::default();
        let mut first = message(0, 10, 1, ALL | NIF_GUID, 100, "By guid");
        first.guid = [7; 16];
        store.apply(&first);
        // The application restarted: new window, same GUID.
        let mut again = message(0, 99, 1, ALL | NIF_GUID, 100, "By guid");
        again.guid = [7; 16];
        store.apply(&again);
        assert_eq!(store.icons.len(), 1);
        assert_eq!(store.icons[0].hwnd, 99, "clicks go to the new window");
    }

    #[test]
    fn what_is_not_in_the_bar_is_in_the_panel_and_nothing_is_in_both() {
        let mut store = IconStore::default();
        for id in 1..=3 {
            store.apply(&message(0, 100, id, NIF_ICON, 7, ""));
        }
        for icon in &mut store.icons {
            icon.pixels = vec![0; 4];
            icon.size = 1;
        }
        store.icons[1].promoted = false;
        store.icons[2].hidden = true;

        let bar: Vec<u32> = store.shown().map(|i| i.id).collect();
        let panel: Vec<u32> = store.overflow().iter().map(|i| i.id).collect();
        assert_eq!(bar, vec![1]);
        assert_eq!(panel, vec![2], "not promoted goes to the panel; hidden goes nowhere");
        assert_eq!(store.pick(Place::Overflow, 0).map(|i| i.id), Some(2));
        assert_eq!(store.pick(Place::Overflow, 1).map(|i| i.id), None);
    }

    #[test]
    fn icons_that_do_not_fit_in_the_bar_spill_into_the_panel() {
        let mut store = IconStore::default();
        for id in 0..(MAX_SHOWN as u32 + 2) {
            store.apply(&message(0, 100, id, NIF_ICON, 7, ""));
        }
        for icon in &mut store.icons {
            icon.pixels = vec![0; 4];
        }
        assert_eq!(store.shown().count(), MAX_SHOWN);
        assert_eq!(store.overflow().len(), 2);
    }

    #[test]
    fn icons_whose_window_died_are_pruned() {
        let mut store = IconStore::default();
        store.apply(&message(0, 10, 1, ALL, 100, "Alive"));
        store.apply(&message(0, 20, 1, ALL, 100, "Crashed"));
        assert!(store.prune(|hwnd| hwnd == 10));
        assert_eq!(store.icons.len(), 1);
        assert!(!store.prune(|_| true));
    }

    #[test]
    fn a_version_four_icon_gets_the_point_and_a_packed_id() {
        let (wparam, lparam) = callback_params(4, 7, WM_LBUTTONUP, 300, 20);
        assert_eq!(wparam, 300 | (20 << 16));
        assert_eq!(lparam as u32 & 0xFFFF, WM_LBUTTONUP);
        assert_eq!(lparam as u32 >> 16, 7);
    }

    #[test]
    fn an_older_icon_gets_its_id_and_the_bare_event() {
        assert_eq!(callback_params(0, 7, WM_LBUTTONUP, 300, 20), (7, WM_LBUTTONUP as isize));
        assert_eq!(callback_params(3, 7, WM_CONTEXTMENU, 300, 20), (7, WM_CONTEXTMENU as isize));
    }

    #[test]
    fn a_second_click_on_the_same_icon_in_time_is_a_double_click() {
        assert!(is_double_click(Some((10, 1, 1000)), (10, 1), 1300, 500));
        assert!(!is_double_click(Some((10, 1, 1000)), (10, 1), 1600, 500), "too slow");
        assert!(!is_double_click(Some((10, 1, 1000)), (10, 2), 1100, 500), "a different icon");
        assert!(!is_double_click(None, (10, 1), 1100, 500));
    }

    #[test]
    fn two_draws_recover_opaque_clear_and_half_covered_pixels() {
        // Opaque red; fully clear; half-covered green.
        let on_black = [0, 0, 255, 0, 0, 0, 0, 0, 0, 128, 0, 0];
        let on_white = [0, 0, 255, 0, 255, 255, 255, 0, 127, 255, 127, 0];
        let out = coverage_from_two_draws(&on_black, &on_white);
        assert_eq!(&out[0..4], &[0, 0, 255, 255]);
        assert_eq!(&out[4..8], &[0, 0, 0, 0]);
        assert_eq!(&out[8..12], &[0, 128, 0, 128]);
    }
}

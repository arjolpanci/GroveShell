//! Mirror Explorer's notification buttons without reparenting its windows.
//! UI Automation and window capture stay on a worker so an unresponsive
//! notification provider cannot block GroveShell's window procedure.
use std::{
    cell::RefCell,
    sync::mpsc::{self, Receiver, Sender},
};
use windows::core::{Interface, VARIANT};
use windows::Win32::{
    Foundation::{HWND, RECT},
    Graphics::Gdi::*,
    System::Com::*,
    UI::{Accessibility::*, WindowsAndMessaging::*},
};

#[derive(Clone)]
pub(crate) struct TrayIcon {
    pub name: String,
    pixels: Vec<u8>,
    size: i32,
}
enum Request {
    Refresh,
    Invoke(String, bool),
    Overflow,
}
struct Worker {
    tx: Sender<Request>,
    rx: Receiver<Option<Vec<TrayIcon>>>,
    icons: Vec<TrayIcon>,
    pending: bool,
}
thread_local! { static WORKER: RefCell<Worker> = RefCell::new(Worker::new()); }

impl Worker {
    fn new() -> Self {
        let (tx, requests) = mpsc::channel();
        let (results, rx) = mpsc::channel();
        // SAFETY: COM and every automation element are confined to this
        // MTA worker. Handles are queried afresh; no Explorer memory is read.
        std::thread::spawn(move || unsafe {
            if CoInitializeEx(None, COINIT_MULTITHREADED).is_err() {
                return;
            }
            let automation: windows::core::Result<IUIAutomation> =
                CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER);
            if let Ok(automation) = automation {
                let mut cached = Vec::<(String, IUIAutomationElement)>::new();
                let mut overflow: Option<IUIAutomationElement> = None;
                while let Ok(request) = requests.recv() {
                    let icons = match request {
                        Request::Refresh => {
                            // Hidden XAML taskbars stop exposing their tree and
                            // paint black. Retain the last real images and COM
                            // elements acquired before GroveShell hid Explorer.
                            let visible = FindWindowW(windows::core::w!("Shell_TrayWnd"), None)
                                .map(|h| IsWindowVisible(h).as_bool())
                                .unwrap_or(false);
                            if !visible {
                                let _ = results.send(None);
                                continue;
                            }
                            if let Some((_, elements)) = buttons(&automation, "NotifyItemIcon") {
                                cached = elements
                                    .into_iter()
                                    .filter_map(|e| {
                                        e.CurrentName().ok().map(|n| (n.to_string(), e))
                                    })
                                    .collect();
                            }
                            if let Some((_, elements)) = buttons(&automation, "SystemTrayIcon") {
                                overflow = elements.into_iter().find(|e| {
                                    e.CurrentName()
                                        .map(|n| {
                                            matches!(
                                                n.to_string().to_lowercase().as_str(),
                                                "show hidden icons"
                                                    | "ausgeblendete symbole einblenden"
                                            )
                                        })
                                        .unwrap_or(false)
                                });
                            }
                            capture(&automation).unwrap_or_default()
                        }
                        Request::Invoke(name, context) => {
                            {
                                for (key, element) in &cached {
                                    if *key == name {
                                        if context {
                                            if let Ok(element) =
                                                element.cast::<IUIAutomationElement3>()
                                            {
                                                let _ = element.ShowContextMenu();
                                            }
                                        } else if let Ok(pattern) = element
                                            .GetCurrentPatternAs::<IUIAutomationInvokePattern>(
                                            UIA_InvokePatternId,
                                        ) {
                                            let _ = pattern.Invoke();
                                        }
                                        break;
                                    }
                                }
                            }
                            continue;
                        }
                        Request::Overflow => {
                            if let Some(element) = &overflow {
                                if let Ok(pattern) = element
                                    .GetCurrentPatternAs::<IUIAutomationInvokePattern>(
                                        UIA_InvokePatternId,
                                    )
                                {
                                    let _ = pattern.Invoke();
                                }
                            }
                            continue;
                        }
                    };
                    if results.send(Some(icons)).is_err() {
                        break;
                    }
                }
            }
            CoUninitialize();
        });
        Self {
            tx,
            rx,
            icons: Vec::new(),
            pending: false,
        }
    }
}

unsafe fn buttons(
    automation: &IUIAutomation,
    id: &str,
) -> Option<(HWND, Vec<IUIAutomationElement>)> {
    let hwnd = FindWindowW(windows::core::w!("Shell_TrayWnd"), None).ok()?;
    let root = automation.ElementFromHandle(hwnd).ok()?;
    let condition = automation
        .CreatePropertyCondition(UIA_AutomationIdPropertyId, &VARIANT::from(id))
        .ok()?;
    let list = root.FindAll(TreeScope_Descendants, &condition).ok()?;
    let mut elements = Vec::new();
    for index in 0..list.Length().ok()?.min(128) {
        if let Ok(element) = list.GetElement(index) {
            elements.push(element);
        }
    }
    Some((hwnd, elements))
}

fn bitmap_info(width: i32, height: i32) -> BITMAPINFO {
    BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    }
}

unsafe fn capture(automation: &IUIAutomation) -> Option<Vec<TrayIcon>> {
    let (hwnd, elements) = buttons(automation, "NotifyItemIcon")?;
    let mut rect = RECT::default();
    GetWindowRect(hwnd, &mut rect).ok()?;
    let (width, height) = (rect.right - rect.left, rect.bottom - rect.top);
    // Bound allocations even when Explorer is relaying a transient/bad rect.
    if width <= 0 || height <= 0 || width > 16384 || height > 512 {
        return None;
    }
    let dc = CreateCompatibleDC(None);
    if dc.0.is_null() {
        return None;
    }
    let mut bits = std::ptr::null_mut();
    let bitmap = match CreateDIBSection(
        dc,
        &bitmap_info(width, height),
        DIB_RGB_COLORS,
        &mut bits,
        None,
        0,
    ) {
        Ok(b) => b,
        Err(_) => {
            let _ = DeleteDC(dc);
            return None;
        }
    };
    let old = SelectObject(dc, bitmap);
    let length = (width * height * 4) as usize;
    std::ptr::write_bytes(bits, 0, length);
    let painted = windows::Win32::Storage::Xps::PrintWindow(
        hwnd,
        dc,
        windows::Win32::Storage::Xps::PRINT_WINDOW_FLAGS(2),
    )
    .as_bool();
    let _ = GdiFlush();
    let mut icons = Vec::new();
    if painted {
        let pixels = std::slice::from_raw_parts(bits as *const u8, length);
        for element in elements {
            let (Ok(bounds), Ok(name)) =
                (element.CurrentBoundingRectangle(), element.CurrentName())
            else {
                continue;
            };
            let size = (bounds.right - bounds.left)
                .min(bounds.bottom - bounds.top)
                .min(64);
            let x = (bounds.left + bounds.right - size) / 2 - rect.left;
            let y = (bounds.top + bounds.bottom - size) / 2 - rect.top;
            if size < 8 || x < 0 || y < 0 || x + size > width || y + size > height {
                continue;
            }
            let mut crop = Vec::with_capacity((size * size * 4) as usize);
            for row in y..y + size {
                let offset = ((row * width + x) * 4) as usize;
                crop.extend_from_slice(&pixels[offset..offset + (size * 4) as usize]);
            }
            // A hidden XAML host may refuse to render. Don't show blank icons.
            if crop.chunks_exact(4).any(|p| p[..3] != crop[..3]) {
                icons.push(TrayIcon {
                    name: name.to_string(),
                    pixels: crop,
                    size,
                });
            }
        }
    }
    SelectObject(dc, old);
    let _ = DeleteObject(bitmap);
    let _ = DeleteDC(dc);
    Some(icons)
}

pub(crate) fn refresh() {
    WORKER.with(|w| {
        let mut w = w.borrow_mut();
        if let Ok(icons) = w.rx.try_recv() {
            if let Some(icons) = icons {
                w.icons = icons;
            }
            w.pending = false;
        }
        if !w.pending && w.tx.send(Request::Refresh).is_ok() {
            w.pending = true;
        }
    });
}

/// Startup only: a bounded wait before Explorer is hidden. Never called
/// from painting or input dispatch, and a hung provider cannot hold startup.
pub(crate) fn prepare() {
    refresh();
    WORKER.with(|w| {
        let mut w = w.borrow_mut();
        if let Ok(icons) = w.rx.recv_timeout(std::time::Duration::from_millis(1000)) {
            if let Some(icons) = icons {
                w.icons = icons;
            }
            w.pending = false;
        }
    });
}

pub(crate) fn count() -> usize {
    WORKER.with(|w| w.borrow().icons.len().min(6))
}

pub(crate) fn name(index: usize) -> String {
    WORKER.with(|w| {
        w.borrow()
            .icons
            .get(index)
            .map(|i| i.name.clone())
            .unwrap_or_default()
    })
}

pub(crate) fn invoke(index: usize, context: bool) {
    WORKER.with(|w| {
        let w = w.borrow();
        if let Some(icon) = w.icons.get(index) {
            let _ = w.tx.send(Request::Invoke(icon.name.clone(), context));
        }
    });
}

pub(crate) fn invoke_overflow() {
    WORKER.with(|w| {
        let _ = w.borrow().tx.send(Request::Overflow);
    });
}

pub(crate) unsafe fn paint(hdc: HDC, index: usize, rect: RECT) {
    WORKER.with(|w| {
        let w = w.borrow();
        if let Some(icon) = w.icons.get(index) {
            StretchDIBits(
                hdc,
                rect.left,
                rect.top,
                rect.right - rect.left,
                rect.bottom - rect.top,
                0,
                0,
                icon.size,
                icon.size,
                Some(icon.pixels.as_ptr() as *const _),
                &bitmap_info(icon.size, icon.size),
                DIB_RGB_COLORS,
                SRCCOPY,
            );
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "Requires a visible Windows taskbar with at least one promoted notification icon"]
    fn capture_promoted_icons_on_desktop() {
        // SAFETY: this dedicated diagnostic process sets DPI awareness before
        // creating any windows; UIA bounds and captures then use physical pixels.
        unsafe {
            let _ = windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(
                windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
            );
        }
        prepare();
        WORKER.with(|w| {
            let w = w.borrow();
            assert!(!w.icons.is_empty(), "No promoted icon images available");
            for icon in &w.icons {
                assert_eq!(icon.pixels.len(), (icon.size * icon.size * 4) as usize);
                assert!(!icon.name.is_empty());
            }
        });
    }
}

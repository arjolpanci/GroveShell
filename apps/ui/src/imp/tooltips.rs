//! Native tooltips supply delayed, system-styled labels for painted controls.
use std::{cell::RefCell, collections::HashMap};
use windows::core::PCWSTR;
use windows::Win32::{
    Foundation::{HWND, LPARAM, WPARAM},
    UI::{Controls::*, WindowsAndMessaging::*},
};
struct Tip {
    hwnd: HWND,
    text: Vec<u16>,
}
thread_local! { static TIPS: RefCell<HashMap<isize, Tip>> = RefCell::new(HashMap::new()); }

pub(crate) fn destroy(owner: HWND) {
    let tip = TIPS.with(|tips| tips.borrow_mut().remove(&(owner.0 as isize)));
    if let Some(tip) = tip {
        // SAFETY: this tooltip was created by this module. Destroy it before
        // dropping the text allocation that its TOOLINFO references.
        unsafe {
            let _ = DestroyWindow(tip.hwnd);
        }
    }
}

pub(crate) fn update(owner: HWND, text: &str) {
    // SAFETY: the caller supplies a live bar HWND. Each tooltip is owned
    // by that bar, and its text allocation stays in TIPS until replaced.
    unsafe {
        TIPS.with(|tips| {
            let mut tips = tips.borrow_mut();
            let entry = tips.entry(owner.0 as isize).or_insert_with(|| {
                let controls = INITCOMMONCONTROLSEX {
                    dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
                    dwICC: ICC_WIN95_CLASSES,
                };
                let _ = InitCommonControlsEx(&controls);
                let hwnd = CreateWindowExW(
                    WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
                    TOOLTIPS_CLASSW,
                    PCWSTR::null(),
                    WS_POPUP | WINDOW_STYLE(TTS_ALWAYSTIP | TTS_NOPREFIX),
                    0,
                    0,
                    0,
                    0,
                    owner,
                    None,
                    None,
                    None,
                )
                .unwrap_or_default();
                let mut info = TTTOOLINFOW {
                    cbSize: std::mem::size_of::<TTTOOLINFOW>() as u32,
                    uFlags: TTF_IDISHWND | TTF_SUBCLASS,
                    hwnd: owner,
                    uId: owner.0 as usize,
                    ..Default::default()
                };
                SendMessageW(
                    hwnd,
                    TTM_ADDTOOLW,
                    WPARAM(0),
                    LPARAM(&mut info as *mut _ as isize),
                );
                SendMessageW(hwnd, TTM_SETMAXTIPWIDTH, WPARAM(0), LPARAM(360));
                Tip {
                    hwnd,
                    text: Vec::new(),
                }
            });
            let next: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
            if entry.text == next {
                return;
            }
            // Keep the previous allocation alive until the control has
            // received its replacement text pointer.
            let old = std::mem::replace(&mut entry.text, next);
            let mut info = TTTOOLINFOW {
                cbSize: std::mem::size_of::<TTTOOLINFOW>() as u32,
                hwnd: owner,
                uId: owner.0 as usize,
                lpszText: windows::core::PWSTR(entry.text.as_mut_ptr()),
                ..Default::default()
            };
            SendMessageW(entry.hwnd, TTM_POP, WPARAM(0), LPARAM(0));
            SendMessageW(
                entry.hwnd,
                TTM_UPDATETIPTEXTW,
                WPARAM(0),
                LPARAM(&mut info as *mut _ as isize),
            );
            drop(old);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_tooltip_keeps_text_and_cleans_up_with_its_owner() {
        // SAFETY: a hidden fixture window is created and destroyed on this
        // test thread. No taskbar, focus, or application settings are changed.
        unsafe {
            let owner = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                windows::core::w!("STATIC"),
                windows::core::w!("Tooltip fixture"),
                WS_POPUP,
                0,
                0,
                100,
                30,
                None,
                None,
                None,
                None,
            )
            .unwrap();
            update(owner, "Wi-Fi & volume");
            let tip = TIPS.with(|tips| tips.borrow().get(&(owner.0 as isize)).unwrap().hwnd);
            assert!(IsWindow(tip).as_bool());
            update(owner, "Updated label");
            destroy(owner);
            assert!(!IsWindow(tip).as_bool());
            let _ = DestroyWindow(owner);
        }
    }
}

//! Three- and four-finger touchpad swipes, read straight from the
//! Precision Touchpad's HID reports.
//!
//! Windows turns a two-finger drag into wheel messages, which is what
//! `gestures::on_scroll` rides on, but it keeps three and four fingers
//! for itself: Task View and its own virtual desktops. There is no
//! message for "three fingers moved 4 mm". Raw Input is the one place
//! the contacts are visible, so this module subscribes to the touchpad
//! collection (`0x0D/0x05`), rebuilds each frame of contacts, and turns
//! the motion of their centroid into begin/update/end events. Those feed
//! the same `SwipeTracker` the scroll path uses.
//!
//! Raw Input observes; it cannot consume. Windows would run its own
//! gesture alongside ours, so `native_gestures` switches those off while
//! the shell runs.
//!
//! Three layers, the first two pure so they can be tested without a
//! touchpad:
//!
//! - [`Recognizer`]: frames of contacts in, swipe events out.
//! - [`FrameAssembler`]: reports in, frames out. A pad that tracks more
//!   fingers than fit in one report spreads a frame over several.
//! - The HID glue at the bottom, which is the only part that talks to
//!   Windows.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;

use windows::Win32::Devices::HumanInterfaceDevice::{
    HidP_GetCaps, HidP_GetUsageValue, HidP_GetUsages, HidP_GetValueCaps, HidP_Input, HIDP_CAPS,
    HIDP_STATUS_SUCCESS, HIDP_VALUE_CAPS, PHIDP_PREPARSED_DATA,
};
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM};
use windows::Win32::UI::Input::{
    GetRawInputData, GetRawInputDeviceInfoW, GetRawInputDeviceList, RegisterRawInputDevices,
    HRAWINPUT, RAWINPUTDEVICE, RAWINPUTDEVICELIST, RAWINPUTHEADER, RIDEV_INPUTSINK, RIDEV_REMOVE,
    RIDI_DEVICEINFO, RIDI_PREPARSEDDATA, RID_DEVICE_INFO, RID_INPUT, RIM_TYPEHID,
};

/// One finger on the pad, in millimetres from the pad's top-left corner.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Contact {
    pub(crate) id: u32,
    pub(crate) x: f64,
    pub(crate) y: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Axis {
    Vertical,
    Horizontal,
}

/// What the fingers did. Deltas are millimetres along the gesture's
/// axis, in pad coordinates: positive is down, or right.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum SwipeEvent {
    /// The fingers have moved far enough to show which way they are
    /// going. `delta_mm` is the travel that got them there.
    Begin { axis: Axis, delta_mm: f64 },
    Update { delta_mm: f64 },
    /// A finger lifted, or one was added.
    End,
}

/// Travel before a resting hand counts as a swipe. Fingers landing on
/// the pad never arrive at exactly the same instant or stay exactly
/// still, so the first millimetre says nothing about intent.
const BEGIN_THRESHOLD_MM: f64 = 1.5;

/// A swipe is three or four fingers. Windows uses both, for different
/// things; here they do the same thing, so neither habit is punished.
fn is_swipe_count(n: usize) -> bool {
    n == 3 || n == 4
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Phase {
    Idle,
    /// The right number of fingers are down but have not moved yet.
    Pending { fingers: usize, origin: (f64, f64) },
    Active { fingers: usize, axis: Axis, last: (f64, f64) },
    /// The gesture ended while fingers were still down. Nothing starts
    /// until they have left, or lifting one finger at a time would fire
    /// a second gesture on the way out.
    Spent,
}

/// Turns frames of contacts into swipe events.
pub(crate) struct Recognizer {
    phase: Phase,
}

impl Default for Recognizer {
    fn default() -> Self {
        Self::new()
    }
}

impl Recognizer {
    pub(crate) fn new() -> Self {
        Self { phase: Phase::Idle }
    }

    /// Feeds one complete frame: every finger currently touching.
    pub(crate) fn feed(&mut self, contacts: &[Contact]) -> Option<SwipeEvent> {
        let n = contacts.len();
        let at = centroid(contacts);
        match self.phase {
            Phase::Idle => {
                if is_swipe_count(n) {
                    self.phase = Phase::Pending { fingers: n, origin: at };
                }
                None
            }
            Phase::Pending { fingers, origin } => {
                if !is_swipe_count(n) {
                    self.phase = Phase::Idle;
                    return None;
                }
                if n != fingers {
                    // A fourth finger landing late moves the centroid by
                    // itself. Measure from where it is now.
                    self.phase = Phase::Pending { fingers: n, origin: at };
                    return None;
                }
                let (dx, dy) = (at.0 - origin.0, at.1 - origin.1);
                if dx.abs().max(dy.abs()) < BEGIN_THRESHOLD_MM {
                    return None;
                }
                let (axis, delta_mm) =
                    if dy.abs() >= dx.abs() { (Axis::Vertical, dy) } else { (Axis::Horizontal, dx) };
                self.phase = Phase::Active { fingers: n, axis, last: at };
                Some(SwipeEvent::Begin { axis, delta_mm })
            }
            Phase::Active { fingers, axis, last } => {
                if n != fingers {
                    self.phase = if n < 3 { Phase::Idle } else { Phase::Spent };
                    return Some(SwipeEvent::End);
                }
                let delta_mm = match axis {
                    Axis::Vertical => at.1 - last.1,
                    Axis::Horizontal => at.0 - last.0,
                };
                self.phase = Phase::Active { fingers, axis, last: at };
                (delta_mm != 0.0).then_some(SwipeEvent::Update { delta_mm })
            }
            Phase::Spent => {
                if n < 3 {
                    self.phase = Phase::Idle;
                }
                None
            }
        }
    }
}

fn centroid(contacts: &[Contact]) -> (f64, f64) {
    if contacts.is_empty() {
        return (0.0, 0.0);
    }
    let n = contacts.len() as f64;
    let (sx, sy) = contacts.iter().fold((0.0, 0.0), |(sx, sy), c| (sx + c.x, sy + c.y));
    (sx / n, sy / n)
}

/// One contact slot as a report carries it, lifted fingers included.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ReportContact {
    pub(crate) contact: Contact,
    pub(crate) tip: bool,
}

/// Rebuilds frames from reports.
///
/// A Precision Touchpad in hybrid mode puts the frame's contact count in
/// its first report and zero in the ones that follow, each carrying the
/// next few contacts. A report also always has all its slots present,
/// used or not, so the count is the only thing that says which are real.
#[derive(Default)]
pub(crate) struct FrameAssembler {
    expected: usize,
    got: Vec<ReportContact>,
}

impl FrameAssembler {
    /// Adds one report. Returns the fingers touching once the frame is
    /// complete.
    pub(crate) fn push(&mut self, contact_count: usize, slots: &[ReportContact]) -> Option<Vec<Contact>> {
        if contact_count > 0 {
            self.expected = contact_count;
            self.got.clear();
        }
        if self.expected == 0 {
            return None;
        }
        let wanted = self.expected - self.got.len();
        self.got.extend(slots.iter().take(wanted));
        if self.got.len() < self.expected {
            return None;
        }
        self.expected = 0;
        // A lifted finger is reported once more with its tip switch off;
        // it is in the frame but no longer on the pad.
        Some(self.got.drain(..).filter(|c| c.tip).map(|c| c.contact).collect())
    }
}

/// Millimetres per logical unit along one axis.
///
/// HID gives a physical extent, a unit system and a base-ten exponent.
/// A pad that leaves them out gets `fallback_mm` spread over its logical
/// range: the scale is then a guess, but a swipe still has the right
/// shape.
pub(crate) fn mm_per_unit(
    logical: (i32, i32),
    physical: (i32, i32),
    units: u32,
    units_exp: u32,
    fallback_mm: f64,
) -> f64 {
    let logical_extent = (logical.1 as f64 - logical.0 as f64).abs();
    if logical_extent <= 0.0 {
        return 0.0;
    }
    // The low nibble is the unit system: 1 is SI linear (centimetres),
    // 3 is English linear (inches). The next nibble is the length
    // exponent, which is 1 for a plain length.
    let unit_mm = match units & 0xFF {
        0x11 => 10.0,
        0x13 => 25.4,
        _ => 0.0,
    };
    // The exponent is a four-bit two's-complement number.
    let exp = if units_exp & 0xF > 7 { (units_exp & 0xF) as i32 - 16 } else { (units_exp & 0xF) as i32 };
    let physical_extent = (physical.1 as f64 - physical.0 as f64).abs();
    let extent_mm = physical_extent * 10f64.powi(exp) * unit_mm;
    if extent_mm > 0.0 {
        extent_mm / logical_extent
    } else {
        fallback_mm / logical_extent
    }
}

// ---------------------------------------------------------------------
// HID glue
// ---------------------------------------------------------------------

const USAGE_PAGE_GENERIC: u16 = 0x01;
const USAGE_PAGE_DIGITIZER: u16 = 0x0D;
const USAGE_TOUCHPAD: u16 = 0x05;
const USAGE_X: u16 = 0x30;
const USAGE_Y: u16 = 0x31;
const USAGE_TIP_SWITCH: u16 = 0x42;
const USAGE_CONTACT_ID: u16 = 0x51;
const USAGE_CONTACT_COUNT: u16 = 0x54;

/// Typical laptop pad size, for pads that do not report one.
const FALLBACK_WIDTH_MM: f64 = 105.0;
const FALLBACK_HEIGHT_MM: f64 = 65.0;

#[derive(Clone, Copy)]
struct AxisScale {
    logical_min: i32,
    mm_per_unit: f64,
}

struct Device {
    /// The device's report descriptor as HID parsed it. Every `HidP_*`
    /// call needs it.
    preparsed: Vec<u8>,
    /// Link collection of each contact slot, in report order.
    slots: Vec<u16>,
    x: AxisScale,
    y: AxisScale,
    assembler: FrameAssembler,
    recognizer: Recognizer,
}

thread_local! {
    /// Touchpads seen so far, by raw-input device handle. `None` marks a
    /// handle whose descriptor could not be used, so it is parsed once
    /// rather than on every report.
    static DEVICES: RefCell<HashMap<isize, Option<Device>>> = RefCell::new(HashMap::new());
}

/// Finds the touchpads present and reads their descriptors, so the log
/// says at startup whether swipes can work on this machine. Otherwise
/// the first sign of a pad that cannot be read would be a swipe that
/// silently does nothing.
fn probe() -> usize {
    let entry = std::mem::size_of::<RAWINPUTDEVICELIST>() as u32;
    // SAFETY: a count query, then a fill of that many entries.
    let list = unsafe {
        let mut count = 0u32;
        GetRawInputDeviceList(None, &mut count, entry);
        let mut list = vec![RAWINPUTDEVICELIST::default(); count as usize];
        let got = GetRawInputDeviceList(Some(list.as_mut_ptr()), &mut count, entry);
        if got == u32::MAX {
            return 0;
        }
        list.truncate(got as usize);
        list
    };

    let mut pads = 0;
    for item in list.iter().filter(|d| d.dwType == RIM_TYPEHID) {
        // `cbSize` must be the struct's real size. It has a union, and a
        // hand-computed size makes every call fail in a way that reads
        // as "no such device".
        let mut info = RID_DEVICE_INFO { cbSize: std::mem::size_of::<RID_DEVICE_INFO>() as u32, ..Default::default() };
        let mut size = info.cbSize;
        // SAFETY: `info` is a live buffer of the size passed.
        let got = unsafe {
            GetRawInputDeviceInfoW(item.hDevice, RIDI_DEVICEINFO, Some(&mut info as *mut _ as *mut c_void), &mut size)
        };
        if got == u32::MAX || got == 0 {
            continue;
        }
        // SAFETY: `dwType` was checked to be HID above, so `hid` is the
        // live arm.
        let hid = unsafe { info.Anonymous.hid };
        if hid.usUsagePage != USAGE_PAGE_DIGITIZER || hid.usUsage != USAGE_TOUCHPAD {
            continue;
        }
        match describe(item.hDevice) {
            Some(dev) => {
                pads += 1;
                tracing::info!(
                    vendor = format_args!("{:04X}", hid.dwVendorId),
                    product = format_args!("{:04X}", hid.dwProductId),
                    slots = dev.slots.len(),
                    x_mm_per_unit = dev.x.mm_per_unit,
                    y_mm_per_unit = dev.y.mm_per_unit,
                    "touchpad: found"
                );
            }
            None => tracing::warn!(
                vendor = format_args!("{:04X}", hid.dwVendorId),
                product = format_args!("{:04X}", hid.dwProductId),
                "touchpad: found, but its descriptor could not be read"
            ),
        }
    }
    pads
}

/// Subscribes `hwnd` to touchpad reports, foreground or not. Returns
/// whether there is a pad whose swipes can be read.
pub(crate) fn register(hwnd: HWND) -> bool {
    if probe() == 0 {
        tracing::info!("touchpad: no readable precision touchpad; finger swipes are off");
        return false;
    }
    let device = RAWINPUTDEVICE {
        usUsagePage: USAGE_PAGE_DIGITIZER,
        usUsage: USAGE_TOUCHPAD,
        // Without `INPUTSINK` reports stop the moment another window has
        // focus, which for a shell is always.
        dwFlags: RIDEV_INPUTSINK,
        hwndTarget: hwnd,
    };
    // SAFETY: `device` is a live local; the size is that of one element.
    let ok = unsafe {
        RegisterRawInputDevices(&[device], std::mem::size_of::<RAWINPUTDEVICE>() as u32).is_ok()
    };
    tracing::info!(ok, "touchpad: raw input registration");
    ok
}

pub(crate) fn unregister() {
    let device = RAWINPUTDEVICE {
        usUsagePage: USAGE_PAGE_DIGITIZER,
        usUsage: USAGE_TOUCHPAD,
        dwFlags: RIDEV_REMOVE,
        // Must be null with `RIDEV_REMOVE`.
        hwndTarget: HWND(std::ptr::null_mut()),
    };
    // SAFETY: as in `register`.
    unsafe {
        let _ = RegisterRawInputDevices(&[device], std::mem::size_of::<RAWINPUTDEVICE>() as u32);
    }
    DEVICES.with(|d| d.borrow_mut().clear());
}

/// Handles one `WM_INPUT`, returning whatever swipe events it completes.
pub(crate) fn on_raw_input(lparam: LPARAM) -> Vec<SwipeEvent> {
    let Some((device, reports, report_len)) = read_raw_input(lparam) else {
        return Vec::new();
    };
    DEVICES.with(|devices| {
        let mut devices = devices.borrow_mut();
        let entry = devices.entry(device).or_insert_with(|| {
            let parsed = describe(HANDLE(device as *mut c_void));
            match &parsed {
                Some(d) => tracing::info!(
                    slots = d.slots.len(),
                    x_mm_per_unit = d.x.mm_per_unit,
                    y_mm_per_unit = d.y.mm_per_unit,
                    "touchpad: device ready"
                ),
                None => tracing::warn!("touchpad: device descriptor unusable"),
            }
            parsed
        });
        let Some(dev) = entry.as_mut() else { return Vec::new() };

        let mut events = Vec::new();
        for report in reports.chunks_exact(report_len) {
            let Some((count, slots)) = parse_report(dev, report) else { continue };
            if let Some(frame) = dev.assembler.push(count, &slots) {
                events.extend(dev.recognizer.feed(&frame));
            }
        }
        events
    })
}

/// Pulls the HID payload out of a `WM_INPUT`: the device, its reports
/// back to back, and the length of one report.
fn read_raw_input(lparam: LPARAM) -> Option<(isize, Vec<u8>, usize)> {
    let handle = HRAWINPUT(lparam.0 as *mut c_void);
    let header_size = std::mem::size_of::<RAWINPUTHEADER>() as u32;
    // SAFETY: the first call asks only for the size; the second fills a
    // buffer of exactly that size. `u64` storage keeps the header's
    // pointer-sized fields aligned.
    unsafe {
        let mut size = 0u32;
        GetRawInputData(handle, RID_INPUT, None, &mut size, header_size);
        if (size as usize) < header_size as usize + 8 {
            return None;
        }
        let mut buf = vec![0u64; (size as usize).div_ceil(8)];
        let read = GetRawInputData(handle, RID_INPUT, Some(buf.as_mut_ptr() as *mut c_void), &mut size, header_size);
        if read != size {
            return None;
        }
        let bytes = std::slice::from_raw_parts(buf.as_ptr() as *const u8, size as usize);
        let header = &*(bytes.as_ptr() as *const RAWINPUTHEADER);
        if header.dwType != RIM_TYPEHID.0 {
            return None;
        }
        // `RAWHID` follows the header: report size, report count, bytes.
        let hid = &bytes[header_size as usize..];
        let report_len = u32::from_ne_bytes(hid[0..4].try_into().ok()?) as usize;
        let report_count = u32::from_ne_bytes(hid[4..8].try_into().ok()?) as usize;
        let data = hid.get(8..8 + report_len.checked_mul(report_count)?)?;
        if report_len == 0 {
            return None;
        }
        Some((header.hDevice.0 as isize, data.to_vec(), report_len))
    }
}

/// Reads a device's descriptor: where its contact slots are, and how
/// its coordinates map to millimetres.
fn describe(device: HANDLE) -> Option<Device> {
    // SAFETY: size query, then a fill of exactly that size. The preparsed
    // data is an opaque blob owned by this `Vec` from then on.
    let preparsed = unsafe {
        let mut size = 0u32;
        GetRawInputDeviceInfoW(device, RIDI_PREPARSEDDATA, None, &mut size);
        if size == 0 {
            return None;
        }
        let mut buf = vec![0u8; size as usize];
        let got = GetRawInputDeviceInfoW(device, RIDI_PREPARSEDDATA, Some(buf.as_mut_ptr() as *mut c_void), &mut size);
        if got == u32::MAX || got == 0 {
            return None;
        }
        buf
    };
    let handle = PHIDP_PREPARSED_DATA(preparsed.as_ptr() as isize);

    // SAFETY: `handle` points at `preparsed`, which outlives both calls;
    // `caps` is sized from the count HID itself reported.
    let value_caps = unsafe {
        let mut caps = HIDP_CAPS::default();
        if HidP_GetCaps(handle, &mut caps) != HIDP_STATUS_SUCCESS {
            return None;
        }
        let mut len = caps.NumberInputValueCaps;
        let mut value_caps = vec![HIDP_VALUE_CAPS::default(); len as usize];
        if len == 0 || HidP_GetValueCaps(HidP_Input, value_caps.as_mut_ptr(), &mut len, handle) != HIDP_STATUS_SUCCESS {
            return None;
        }
        value_caps.truncate(len as usize);
        value_caps
    };

    let mut slots: Vec<u16> = Vec::new();
    let mut x = None;
    let mut y = None;
    for cap in &value_caps {
        if cap.UsagePage != USAGE_PAGE_GENERIC {
            continue;
        }
        // SAFETY: `IsRange` says which arm of the union is live.
        let usage = unsafe {
            if cap.IsRange.as_bool() { cap.Anonymous.Range.UsageMin } else { cap.Anonymous.NotRange.Usage }
        };
        let scale = |fallback_mm| AxisScale {
            logical_min: cap.LogicalMin,
            mm_per_unit: mm_per_unit(
                (cap.LogicalMin, cap.LogicalMax),
                (cap.PhysicalMin, cap.PhysicalMax),
                cap.Units,
                cap.UnitsExp,
                fallback_mm,
            ),
        };
        match usage {
            USAGE_X => {
                slots.push(cap.LinkCollection);
                x.get_or_insert_with(|| scale(FALLBACK_WIDTH_MM));
            }
            USAGE_Y => {
                y.get_or_insert_with(|| scale(FALLBACK_HEIGHT_MM));
            }
            _ => {}
        }
    }
    slots.sort_unstable();
    slots.dedup();
    let (x, y) = (x?, y?);
    if slots.is_empty() || x.mm_per_unit <= 0.0 || y.mm_per_unit <= 0.0 {
        return None;
    }
    Some(Device {
        preparsed,
        slots,
        x,
        y,
        assembler: FrameAssembler::default(),
        recognizer: Recognizer::new(),
    })
}

/// Reads one report: its contact count and every slot.
fn parse_report(dev: &Device, report: &[u8]) -> Option<(usize, Vec<ReportContact>)> {
    let handle = PHIDP_PREPARSED_DATA(dev.preparsed.as_ptr() as isize);
    let value = |page: u16, link: u16, usage: u16| -> Option<u32> {
        let mut out = 0u32;
        // SAFETY: `handle` points at `dev.preparsed`; `report` is one
        // whole input report from that same device.
        let status = unsafe { HidP_GetUsageValue(HidP_Input, page, link, usage, &mut out, handle, report) };
        (status == HIDP_STATUS_SUCCESS).then_some(out)
    };

    // `HidP_GetUsages` wants a mutable report though it only reads it.
    let mut scratch = report.to_vec();
    let mut slots = Vec::with_capacity(dev.slots.len());
    for &link in &dev.slots {
        let (Some(raw_x), Some(raw_y)) = (value(USAGE_PAGE_GENERIC, link, USAGE_X), value(USAGE_PAGE_GENERIC, link, USAGE_Y)) else {
            // Not a touch report (a pad can send others on the same
            // collection): nothing here to read.
            return None;
        };
        let id = value(USAGE_PAGE_DIGITIZER, link, USAGE_CONTACT_ID).unwrap_or(link as u32);
        let mut pressed = [0u16; 8];
        let mut pressed_len = pressed.len() as u32;
        // SAFETY: as above; `pressed_len` is the capacity of `pressed`.
        let status = unsafe {
            HidP_GetUsages(HidP_Input, USAGE_PAGE_DIGITIZER, link, pressed.as_mut_ptr(), &mut pressed_len, handle, &mut scratch)
        };
        let tip = status == HIDP_STATUS_SUCCESS
            && pressed[..pressed_len.min(8) as usize].contains(&USAGE_TIP_SWITCH);
        slots.push(ReportContact {
            contact: Contact {
                id,
                x: (raw_x as i64 - dev.x.logical_min as i64) as f64 * dev.x.mm_per_unit,
                y: (raw_y as i64 - dev.y.logical_min as i64) as f64 * dev.y.mm_per_unit,
            },
            tip,
        });
    }

    // A pad with no contact-count usage sends one frame per report, so
    // every slot counts.
    let count = value(USAGE_PAGE_DIGITIZER, 0, USAGE_CONTACT_COUNT).map(|c| c as usize).unwrap_or(slots.len());
    Some((count, slots))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fingers(n: usize, x: f64, y: f64) -> Vec<Contact> {
        (0..n).map(|i| Contact { id: i as u32, x: x + i as f64 * 15.0, y }).collect()
    }

    #[test]
    fn three_fingers_resting_do_not_start_anything() {
        let mut r = Recognizer::new();
        assert_eq!(r.feed(&fingers(3, 20.0, 30.0)), None);
        assert_eq!(r.feed(&fingers(3, 20.2, 30.3)), None);
        assert_eq!(r.feed(&fingers(3, 20.1, 29.8)), None);
    }

    #[test]
    fn a_swipe_up_begins_vertical_with_the_travel_so_far() {
        let mut r = Recognizer::new();
        r.feed(&fingers(3, 20.0, 30.0));
        let ev = r.feed(&fingers(3, 20.3, 28.0));
        assert_eq!(ev, Some(SwipeEvent::Begin { axis: Axis::Vertical, delta_mm: -2.0 }));
    }

    #[test]
    fn a_swipe_sideways_begins_horizontal() {
        let mut r = Recognizer::new();
        r.feed(&fingers(3, 20.0, 30.0));
        let ev = r.feed(&fingers(3, 23.0, 30.4));
        assert_eq!(ev, Some(SwipeEvent::Begin { axis: Axis::Horizontal, delta_mm: 3.0 }));
    }

    #[test]
    fn updates_follow_the_axis_and_can_reverse() {
        let mut r = Recognizer::new();
        r.feed(&fingers(3, 20.0, 30.0));
        r.feed(&fingers(3, 20.0, 28.0));
        assert_eq!(r.feed(&fingers(3, 25.0, 25.0)), Some(SwipeEvent::Update { delta_mm: -3.0 }));
        // Back down again: the same gesture, the other sign.
        assert_eq!(r.feed(&fingers(3, 25.0, 27.0)), Some(SwipeEvent::Update { delta_mm: 2.0 }));
    }

    #[test]
    fn holding_still_mid_swipe_neither_updates_nor_ends() {
        let mut r = Recognizer::new();
        r.feed(&fingers(3, 20.0, 30.0));
        r.feed(&fingers(3, 20.0, 27.0));
        for _ in 0..50 {
            assert_eq!(r.feed(&fingers(3, 20.0, 27.0)), None);
        }
        assert_eq!(r.feed(&fingers(3, 20.0, 26.0)), Some(SwipeEvent::Update { delta_mm: -1.0 }));
    }

    #[test]
    fn lifting_ends_it_and_the_remaining_fingers_start_nothing() {
        let mut r = Recognizer::new();
        r.feed(&fingers(4, 20.0, 30.0));
        r.feed(&fingers(4, 20.0, 26.0));
        // One finger up: four became three, which is itself a swipe
        // count. It must not begin a second gesture on the way out.
        assert_eq!(r.feed(&fingers(3, 20.0, 26.0)), Some(SwipeEvent::End));
        assert_eq!(r.feed(&fingers(3, 20.0, 20.0)), None);
        assert_eq!(r.feed(&fingers(2, 20.0, 15.0)), None);
        assert_eq!(r.feed(&[]), None);
        // Fingers back down: a fresh gesture.
        r.feed(&fingers(3, 20.0, 30.0));
        assert!(matches!(r.feed(&fingers(3, 20.0, 27.0)), Some(SwipeEvent::Begin { .. })));
    }

    #[test]
    fn all_fingers_leaving_at_once_ends_it() {
        let mut r = Recognizer::new();
        r.feed(&fingers(3, 20.0, 30.0));
        r.feed(&fingers(3, 20.0, 26.0));
        assert_eq!(r.feed(&[]), Some(SwipeEvent::End));
    }

    #[test]
    fn two_fingers_are_a_scroll_not_a_swipe() {
        let mut r = Recognizer::new();
        r.feed(&fingers(2, 20.0, 30.0));
        assert_eq!(r.feed(&fingers(2, 20.0, 10.0)), None);
    }

    #[test]
    fn a_late_fourth_finger_does_not_read_as_motion() {
        let mut r = Recognizer::new();
        r.feed(&fingers(3, 20.0, 30.0));
        // The fourth finger shifts the centroid 7.5 mm sideways by
        // landing. That is not a swipe.
        assert_eq!(r.feed(&fingers(4, 20.0, 30.0)), None);
        assert_eq!(r.feed(&fingers(4, 20.0, 30.2)), None);
    }

    fn slot(id: u32, tip: bool) -> ReportContact {
        ReportContact { contact: Contact { id, x: id as f64, y: 0.0 }, tip }
    }

    #[test]
    fn a_report_carries_only_as_many_real_contacts_as_it_counts() {
        let mut a = FrameAssembler::default();
        // Five slots, three in use. The other two hold whatever was
        // there last.
        let slots = [slot(0, true), slot(1, true), slot(2, true), slot(9, true), slot(9, true)];
        let frame = a.push(3, &slots).expect("one report is the whole frame");
        assert_eq!(frame.iter().map(|c| c.id).collect::<Vec<_>>(), vec![0, 1, 2]);
    }

    #[test]
    fn a_frame_split_over_two_reports_is_joined() {
        let mut a = FrameAssembler::default();
        // Three fingers on a pad with two slots per report.
        assert_eq!(a.push(3, &[slot(0, true), slot(1, true)]), None);
        let frame = a.push(0, &[slot(2, true), slot(7, true)]).expect("second report completes it");
        assert_eq!(frame.len(), 3);
    }

    #[test]
    fn a_lifted_finger_is_in_the_frame_but_not_on_the_pad() {
        let mut a = FrameAssembler::default();
        let frame = a.push(3, &[slot(0, true), slot(1, false), slot(2, true)]).unwrap();
        assert_eq!(frame.len(), 2);
    }

    #[test]
    fn a_continuation_with_no_frame_open_is_dropped() {
        let mut a = FrameAssembler::default();
        assert_eq!(a.push(0, &[slot(0, true)]), None);
    }

    #[test]
    fn physical_units_give_millimetres() {
        // 0..1228 logical over 0..1040 physical, in 10^-2 cm: 104 mm.
        let scale = mm_per_unit((0, 1228), (0, 1040), 0x11, 0x0E, 999.0);
        assert!((scale * 1228.0 - 104.0).abs() < 1e-9);
        // The same pad described in 10^-2 inches.
        let scale = mm_per_unit((0, 1000), (0, 400), 0x13, 0x0E, 999.0);
        assert!((scale * 1000.0 - 101.6).abs() < 1e-9);
    }

    #[test]
    fn a_pad_with_no_physical_size_falls_back() {
        let scale = mm_per_unit((0, 2000), (0, 0), 0, 0, 100.0);
        assert!((scale * 2000.0 - 100.0).abs() < 1e-9);
    }
}

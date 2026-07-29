//! DirectComposition/Direct2D rendering foundation (Phase 1 of the
//! animation-fluidity work — see
//! `docs/superpowers/specs/2026-07-29-directcomposition-rendering-foundation-design.md`).
//! Owns process-wide GPU device setup and per-window composition
//! surfaces. If setup fails at any point, `is_enabled()` returns `false`
//! for the rest of the process's life and every caller falls back to its
//! existing GDI painting, unchanged.

use std::cell::RefCell;

use windows::core::Interface;
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Device, ID2D1Factory1, D2D1_FACTORY_TYPE_SINGLE_THREADED,
};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::DirectComposition::{DCompositionCreateDevice2, IDCompositionDevice};
use windows::Win32::Graphics::DirectWrite::{DWriteCreateFactory, IDWriteFactory, DWRITE_FACTORY_TYPE_SHARED};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;

pub(crate) struct GpuContext {
    pub(crate) dcomp_device: IDCompositionDevice,
    pub(crate) dwrite_factory: IDWriteFactory,
}

thread_local! {
    pub(crate) static GPU: RefCell<Option<GpuContext>> = const { RefCell::new(None) };
}

/// Sets up the process-wide D3D11/DirectComposition/DirectWrite devices,
/// once. Must be called before any GPU-rendered window is created. The
/// decision (GPU available or not) is made exactly here and never
/// revisited — every later caller just checks [`is_enabled`].
pub(crate) fn init() {
    match try_init() {
        Ok(ctx) => GPU.with(|g| *g.borrow_mut() = Some(ctx)),
        Err(e) => {
            tracing::warn!(error = ?e, "GPU rendering setup failed, falling back to GDI for the whole process");
        }
    }
}

fn try_init() -> windows::core::Result<GpuContext> {
    // SAFETY: plain device/factory creation; every output parameter is a
    // local on the stack, valid to write to for the duration of the call.
    unsafe {
        let mut d3d_device = None;
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            None,
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut d3d_device),
            None,
            None,
        )?;
        let d3d_device = d3d_device.expect("D3D11CreateDevice succeeded but returned no device");
        let dxgi_device: IDXGIDevice = d3d_device.cast()?;

        let d2d_factory: ID2D1Factory1 = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
        let d2d_device: ID2D1Device = d2d_factory.CreateDevice(&dxgi_device)?;

        let dcomp_device: IDCompositionDevice = DCompositionCreateDevice2(&d2d_device)?;
        let dwrite_factory: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;

        Ok(GpuContext { dcomp_device, dwrite_factory })
    }
}

/// Whether the process-wide GPU setup succeeded. Decided once, at
/// startup, by [`init`] — never re-checked or retried afterwards.
pub(crate) fn is_enabled() -> bool {
    GPU.with(|g| g.borrow().is_some())
}

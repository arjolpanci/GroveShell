# groveshell-settings Tray/Launcher App Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build `groveshell-settings.exe`, a new tray-resident binary that launches/supervises `watchdog`→`host`→`ui`, shows their health, offers a one-click Explorer-restore toggle, and hosts a Fluent-styled native Win32 settings window covering dock/top-bar/overview/input customization — finishing Phase 4 of `docs/PROJECT_PLAN.md`.

**Architecture:** A new `apps/settings` binary crate, matching the existing `apps/host`/`apps/watchdog` process shape (single-instance mutex, `groveshell-common::logging::init`, `ShellJob::create_and_join`) plus a hand-rolled Win32 message-loop window (matching `apps/ui`'s `imp/mod.rs` pattern) for the tray icon and settings UI. `apps/ui` gains its first-ever `groveshell-config` dependency so its hardcoded constants (bar height, dock icon size, Super-key binding, blur) become config-driven and live-reloadable over a new `groveshell-ui` named pipe.

**Tech Stack:** Rust, `windows` crate (Win32 GDI/GDI+, `Shell_NotifyIconW`, `Win32_System_Registry`, `Win32_System_Diagnostics_ToolHelp`), `groveshell-config`/`groveshell-ipc`/`groveshell-common` (existing crates), `image`+`ico`+`embed-resource` (new build-time-only dependencies for icon generation).

## Global Constraints

- Windows-only; every new source file starts with `#![cfg(windows)]` or gates its module with `#[cfg(windows)]`, matching every existing `apps/*` crate.
- No new runtime dependency beyond what's already in the workspace, except `image`, `ico`, and `embed-resource` — and those are **build-dependencies only** (icon generation happens at compile time, not runtime).
- Dock alignment is horizontal (left/center/right along the bottom edge) only — no vertical/side-dock layout change.
- The overview/move-resize trigger is a 3-way preset (`"Super"` / `"Alt"` / `"CtrlAlt"`), not an arbitrary key recorder.
- Blur toggles use `DwmEnableBlurBehindWindow` (simple on/off) — no `DWM_SYSTEMBACKDROP_TYPE`/Mica.
- No WinUI3/XAML/WebView anywhere in this feature.
- Settings window visuals match the existing bar/calendar palette: `0xE0E0E0` text, `0x202020`/`0x262626`/`0x303030` backgrounds, Segoe UI, rounded panels — reuse `apps/ui`'s established literal color values, don't invent new ones.
- Every control commits immediately (`groveshell_config::save` on each change) — no separate Apply/Save button.
- `scripts/dev-start.ps1` and `scripts/recover.ps1` are unmodified by this feature.

---

## File Structure

```
apps/settings/
  Cargo.toml
  build.rs                    # Task 4: generates icon.ico from media/logo.png, embeds it
  src/
    main.rs                   # Task 3: entry point, single-instance, calls imp::run()
    imp/
      mod.rs                  # Task 3 (skeleton) -> Task 6 (full): window registration, wndproc, message loop
      process.rs               # Task 5: spawn/track/stop watchdog+host+ui
      tray.rs                  # Task 6: Shell_NotifyIconW + context menu
      health.rs                # Task 7: CPU%/RAM sampling + host.ping liveness
      autostart.rs             # Task 10: HKCU Run key read/write
      config_store.rs          # Task 14: load/save Config + push config.reload to apps/ui
      theme.rs                 # Task 8: shared color constants + owner-drawn widget helpers
      nav.rs                   # Task 8: left-hand nav list
      pages/
        mod.rs                 # Task 8: Page trait + dispatch
        home.rs                # Task 9: health/stats/restore-button/autostart page
        dock.rs                 # Task 15: dock alignment/icon-size/mode page
        top_bar.rs              # Task 16: bar height/blur page
        overview.rs              # Task 17: overview blur/reduced-motion/animation-speed page
        input.rs                 # Task 18: overview-modifier/hot-corner page

crates/config/src/model.rs      # Task 1: new AppearanceConfig/InputConfig fields
crates/ipc/src/envelope.rs      # Task 2: config.reload message_type constant

apps/ui/Cargo.toml               # Task 11: new groveshell-config dependency
apps/ui/src/imp/mod.rs            # Tasks 11-13: load config at startup, blur, live-reload pipe listener
apps/ui/src/imp/state.rs          # Task 11: BAR_HEIGHT becomes a runtime value, new AppState fields
apps/ui/src/imp/dock.rs           # Tasks 11, 15: dock_icon_size/dock_alignment become config-driven + anchor_x pure fn
apps/ui/src/imp/movesize.rs       # Task 12: overview_modifier config wiring
apps/ui/src/imp/util.rs           # Task 11: animation_scale/reduced_motion applied in progress()
```

---

### Task 1: Config schema additions

**Files:**
- Modify: `crates/config/src/model.rs`
- Test: `crates/config/tests/config_tests.rs`

**Interfaces:**
- Produces: `AppearanceConfig` gains `dock_icon_size: u32` (default 44), `dock_alignment: String` (default `"center"`), `top_bar_blur: bool` (default `false`), `overview_blur: bool` (default `false`), `reduced_motion: bool` (default `false`). `InputConfig` gains `overview_modifier: String` (default `"Super"`). `Config::validate()` rejects unknown `dock_alignment`/`overview_modifier` values.

- [ ] **Step 1: Write the failing tests**

Append to `crates/config/tests/config_tests.rs`:

```rust
#[test]
fn default_config_has_sane_defaults_for_new_appearance_and_input_fields() {
    let config = Config::default();
    assert_eq!(config.appearance.dock_icon_size, 44);
    assert_eq!(config.appearance.dock_alignment, "center");
    assert!(!config.appearance.top_bar_blur);
    assert!(!config.appearance.overview_blur);
    assert!(!config.appearance.reduced_motion);
    assert_eq!(config.input.overview_modifier, "Super");
}

#[test]
fn load_rejects_unknown_dock_alignment() {
    let file = write_temp_toml(
        "schema_version = 1\n[appearance]\ndock_alignment = \"top\"\n",
    );
    assert!(load(file.path()).is_err());
}

#[test]
fn load_rejects_unknown_overview_modifier() {
    let file = write_temp_toml(
        "schema_version = 1\n[input]\noverview_modifier = \"Ctrl\"\n",
    );
    assert!(load(file.path()).is_err());
}

#[test]
fn load_accepts_every_valid_dock_alignment_and_overview_modifier() {
    for alignment in ["left", "center", "right"] {
        let toml = format!("schema_version = 1\n[appearance]\ndock_alignment = \"{alignment}\"\n");
        let file = write_temp_toml(&toml);
        assert!(load(file.path()).is_ok(), "{alignment} should be accepted");
    }
    for modifier in ["Super", "Alt", "CtrlAlt"] {
        let toml = format!("schema_version = 1\n[input]\noverview_modifier = \"{modifier}\"\n");
        let file = write_temp_toml(&toml);
        assert!(load(file.path()).is_ok(), "{modifier} should be accepted");
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p groveshell-config`
Expected: FAIL — `dock_icon_size`/`dock_alignment`/etc. fields don't exist yet (compile error).

- [ ] **Step 3: Add the new fields and validation**

In `crates/config/src/model.rs`, replace the `AppearanceConfig` struct and its `Default` impl:

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppearanceConfig {
    pub top_bar_height: u32,
    pub dock_mode: String,
    pub animation_scale: f32,
    pub dock_icon_size: u32,
    pub dock_alignment: String,
    pub top_bar_blur: bool,
    pub overview_blur: bool,
    pub reduced_motion: bool,
}

impl Default for AppearanceConfig {
    fn default() -> Self {
        Self {
            top_bar_height: 32,
            dock_mode: "overview".to_string(),
            animation_scale: 1.0,
            dock_icon_size: 44,
            dock_alignment: "center".to_string(),
            top_bar_blur: false,
            overview_blur: false,
            reduced_motion: false,
        }
    }
}
```

Replace `InputConfig` and its `Default` impl:

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct InputConfig {
    pub move_modifier: String,
    pub move_button: String,
    pub resize_button: String,
    pub overview_modifier: String,
}

impl Default for InputConfig {
    fn default() -> Self {
        Self {
            move_modifier: "Alt".to_string(),
            move_button: "Left".to_string(),
            resize_button: "Right".to_string(),
            overview_modifier: "Super".to_string(),
        }
    }
}
```

Add two checks inside `Config::validate()`, right after the existing `animation_scale` check:

```rust
        if !matches!(self.appearance.dock_alignment.as_str(), "left" | "center" | "right") {
            return Err(Error::InvalidConfig(format!(
                "appearance.dock_alignment: unknown value '{}'",
                self.appearance.dock_alignment
            )));
        }
        if !matches!(self.input.overview_modifier.as_str(), "Super" | "Alt" | "CtrlAlt") {
            return Err(Error::InvalidConfig(format!(
                "input.overview_modifier: unknown value '{}'",
                self.input.overview_modifier
            )));
        }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p groveshell-config`
Expected: PASS, all tests including the four new ones and every pre-existing one (the pre-existing `load_valid_toml_matches_the_documented_example` test doesn't set the new fields, so it must still pass under `#[serde(default)]`).

- [ ] **Step 5: Commit**

```bash
git add crates/config/src/model.rs crates/config/tests/config_tests.rs
git commit -m "feat(config): add dock/top-bar/overview/input customization fields"
```

---

### Task 2: `config.reload` IPC message type

**Files:**
- Modify: `crates/ipc/src/envelope.rs`

**Interfaces:**
- Produces: `groveshell_ipc::message_type::CONFIG_RELOAD: &str = "config.reload"`, `groveshell_ipc::message_type::UI_PIPE_NAME` is **not** added here (pipe names are plain string literals at each call site, matching how `HOST_PIPE_NAME`/`WATCHDOG_PIPE_NAME` are already defined locally in `apps/host`/`apps/watchdog`, not in the ipc crate) — Task 13 defines its own `UI_PIPE_NAME` constant in `apps/ui`.

- [ ] **Step 1: Write the failing test**

Add to the bottom of `crates/ipc/src/envelope.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_reload_message_type_is_stable() {
        assert_eq!(message_type::CONFIG_RELOAD, "config.reload");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p groveshell-ipc config_reload`
Expected: FAIL — `CONFIG_RELOAD` doesn't exist (compile error).

- [ ] **Step 3: Add the constant**

In `crates/ipc/src/envelope.rs`, inside `pub mod message_type`, add:

```rust
    /// Pushed by `groveshell-settings` to the `groveshell-ui` pipe after
    /// every successful `config.toml` save, so `groveshell-ui` can reload
    /// and re-apply settings live without a restart. No payload; the
    /// receiver always re-reads the config file itself rather than
    /// trusting an embedded copy, so this message can never carry a
    /// version that's already stale by the time it's read.
    pub const CONFIG_RELOAD: &str = "config.reload";
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p groveshell-ipc config_reload`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/ipc/src/envelope.rs
git commit -m "feat(ipc): add config.reload message type"
```

---

### Task 3: `apps/settings` crate scaffold

**Files:**
- Create: `apps/settings/Cargo.toml`
- Create: `apps/settings/src/main.rs`
- Create: `apps/settings/src/imp/mod.rs`

**Interfaces:**
- Consumes: `groveshell_common::jobobject::ShellJob::create_and_join()`, `groveshell_common::logging::init(component: &str)`, `groveshell_config::load_or_default(path: &Path)`.
- Produces: a runnable (but otherwise empty) `groveshell-settings.exe` that acquires a single-instance lock, joins the shell job, loads config, and idles in a message loop. Later tasks (5, 6, 7, 8...) add real behavior inside `imp::run()`.

- [ ] **Step 1: Write the crate manifest**

Create `apps/settings/Cargo.toml`:

```toml
[package]
name = "groveshell-settings"
version.workspace = true
edition.workspace = true
license.workspace = true
publish.workspace = true

[[bin]]
name = "groveshell-settings"
path = "src/main.rs"

[dependencies]
groveshell-common = { workspace = true }
groveshell-config = { workspace = true }
groveshell-ipc = { workspace = true }
tracing = { workspace = true }
serde_json = { workspace = true }

[target.'cfg(windows)'.dependencies]
windows = { workspace = true, features = [
  "Win32_Foundation",
  "Win32_Graphics_Dwm",
  "Win32_Graphics_Gdi",
  "Win32_Graphics_GdiPlus",
  "Win32_System_Com",
  "Win32_System_Diagnostics_ToolHelp",
  "Win32_System_LibraryLoader",
  "Win32_System_Registry",
  "Win32_System_Threading",
  "Win32_UI_HiDpi",
  "Win32_UI_Shell",
  "Win32_UI_WindowsAndMessaging",
] }
```

- [ ] **Step 2: Write `main.rs`**

Create `apps/settings/src/main.rs`:

```rust
//! `groveshell-settings`: the tray-resident launcher/settings app. Spawns
//! and supervises `groveshell-watchdog`, `groveshell-host`, and
//! `groveshell-ui`; shows their health; offers a one-click Explorer-restore
//! toggle; hosts the dock/top-bar/overview/input settings window. See
//! `docs/superpowers/specs/2026-07-30-tray-settings-app-design.md`.

#[cfg(windows)]
mod imp;

#[cfg(windows)]
fn main() -> groveshell_common::Result<()> {
    imp::run()
}

#[cfg(not(windows))]
fn main() {
    eprintln!("groveshell-settings is Windows-only.");
    std::process::exit(1);
}
```

- [ ] **Step 3: Write the `imp` module skeleton**

Create `apps/settings/src/imp/mod.rs`:

```rust
//! Process bootstrap for `groveshell-settings`.

use groveshell_common::Result;

pub fn run() -> Result<()> {
    let _log_guard = groveshell_common::logging::init("settings")?;
    tracing::info!("groveshell-settings starting");

    let _single_instance = acquire_single_instance_lock()?;
    tracing::info!("single-instance lock acquired");

    let _job = groveshell_common::jobobject::ShellJob::create_and_join()?;
    tracing::info!("joined shell job object");

    let config_path = groveshell_common::paths::data_dir()?.join("config.toml");
    let config = groveshell_config::load_or_default(&config_path);
    tracing::info!(?config, "configuration loaded");

    // Task 5 replaces this with real process supervision; Task 6 replaces
    // the sleep loop with a real Win32 message loop driving the tray icon
    // and settings window.
    loop {
        std::thread::sleep(std::time::Duration::from_secs(60));
    }
}

/// Acquires a session-local named mutex so at most one `groveshell-settings`
/// runs at a time — same pattern as `apps/host`'s own single-instance lock.
fn acquire_single_instance_lock() -> Result<SingleInstanceGuard> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS, HANDLE};
    use windows::Win32::System::Threading::CreateMutexW;

    let name: Vec<u16> = "Local\\GroveShell-Settings-SingleInstance"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    // SAFETY: `name` is a valid, NUL-terminated UTF-16 buffer that outlives
    // this call. No security attributes are supplied, so the handle gets
    // default access and is not inheritable.
    let handle: HANDLE = unsafe { CreateMutexW(None, true, PCWSTR(name.as_ptr())) }
        .map_err(groveshell_common::Error::Windows)?;

    // SAFETY: `GetLastError` reads thread-local state set by the
    // immediately preceding `CreateMutexW` call above, which succeeded.
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        tracing::info!("another groveshell-settings instance is already running; exiting");
        std::process::exit(0);
    }

    Ok(SingleInstanceGuard(handle))
}

struct SingleInstanceGuard(windows::Win32::Foundation::HANDLE);

impl Drop for SingleInstanceGuard {
    fn drop(&mut self) {
        // SAFETY: `self.0` was created by `CreateMutexW` above and is owned
        // exclusively by this guard.
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(self.0);
        }
    }
}
```

- [ ] **Step 4: Build and smoke-test**

Run: `cargo build -p groveshell-settings`
Expected: builds cleanly. Run `target/debug/groveshell-settings.exe` briefly (e.g. `Start-Process` then `Stop-Process` in PowerShell, or Ctrl+C from a foreground run) and confirm `%LOCALAPPDATA%\GroveShell\logs\settings.log` gets created — this is the manual-verification step for a process with no automated test surface yet.

- [ ] **Step 5: Commit**

```bash
git add apps/settings/Cargo.toml apps/settings/src/main.rs apps/settings/src/imp/mod.rs
git commit -m "feat(settings): scaffold the groveshell-settings binary crate"
```

---

### Task 4: App icon generation and embedding

**Files:**
- Create: `apps/settings/build.rs`
- Modify: `apps/settings/Cargo.toml`

**Interfaces:**
- Produces: the compiled `groveshell-settings.exe` carries an embedded icon (Win32 resource ID `1`) generated at build time from `media/logo.png`. Later tasks load it at runtime via `LoadImageW(GetModuleHandleW(None), PCWSTR(1 as _), IMAGE_ICON, ..)` for both the window class icon and the tray icon's `hIcon` — no runtime file path needed.

- [ ] **Step 1: Add build-dependencies**

Append to `apps/settings/Cargo.toml`:

```toml
[build-dependencies]
image = "0.24"
ico = "0.3"
embed-resource = "2"
```

- [ ] **Step 2: Write `build.rs`**

Create `apps/settings/build.rs`:

```rust
//! Generates a multi-resolution `.ico` from `media/logo.png` at build time
//! and embeds it as this exe's resource ID 1 (both the file's own icon and,
//! loaded at runtime via `LoadImageW(.., PCWSTR(1 as _), ..)`, the tray
//! icon and settings-window title-bar icon). Generating this at build time
//! rather than committing a binary `.ico` avoids needing an external
//! image-conversion tool in this repo's toolchain.

use std::path::Path;

const ICON_SIZES: &[u32] = &[16, 32, 48, 256];

fn main() {
    if std::env::var("CARGO_CFG_WINDOWS").is_err() {
        return;
    }

    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set");
    let logo_path = Path::new(&manifest_dir).join("../../media/logo.png");
    println!("cargo:rerun-if-changed={}", logo_path.display());

    let source = image::open(&logo_path)
        .unwrap_or_else(|e| panic!("failed to open {}: {e}", logo_path.display()));

    let mut icon_dir = ico::IconDir::new(ico::ResourceType::Icon);
    for &size in ICON_SIZES {
        let resized = source.resize_exact(size, size, image::imageops::FilterType::Lanczos3);
        let rgba = resized.to_rgba8();
        let image = ico::IconImage::from_rgba_data(size, size, rgba.into_raw());
        let entry = ico::IconDirEntry::encode(&image)
            .unwrap_or_else(|e| panic!("failed to encode {size}x{size} icon: {e}"));
        icon_dir.add_entry(entry);
    }

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR is set");
    let ico_path = Path::new(&out_dir).join("icon.ico");
    let ico_file = std::fs::File::create(&ico_path)
        .unwrap_or_else(|e| panic!("failed to create {}: {e}", ico_path.display()));
    icon_dir
        .write(ico_file)
        .unwrap_or_else(|e| panic!("failed to write {}: {e}", ico_path.display()));

    let rc_path = Path::new(&out_dir).join("icon.rc");
    std::fs::write(&rc_path, format!("1 ICON \"{}\"\n", ico_path.display()))
        .unwrap_or_else(|e| panic!("failed to write {}: {e}", rc_path.display()));

    embed_resource::compile(&rc_path, embed_resource::NONE);
}
```

- [ ] **Step 3: Build and verify the icon is embedded**

Run: `cargo build -p groveshell-settings`
Expected: builds cleanly, and `target/debug/groveshell-settings.exe`'s file icon (visible in Windows Explorer, or via `(Get-Item target\debug\groveshell-settings.exe).VersionInfo` — file icon itself is easiest checked by browsing to the file in Explorer and confirming it shows the GroveShell leaf logo instead of the generic Rust/exe icon) reflects `media/logo.png` instead of the default.

- [ ] **Step 4: Commit**

```bash
git add apps/settings/Cargo.toml apps/settings/build.rs
git commit -m "feat(settings): generate and embed the app icon from media/logo.png at build time"
```

---

### Task 5: Process lifecycle (spawn/supervise/stop watchdog+host+ui)

**Files:**
- Create: `apps/settings/src/imp/process.rs`
- Modify: `apps/settings/src/imp/mod.rs`

**Interfaces:**
- Consumes: nothing new from earlier tasks beyond the crate scaffold.
- Produces: `pub struct ManagedProcesses { watchdog: Option<Child>, host: Option<Child>, ui: Option<Child> }` with methods `new() -> Self`, `spawn_all(&mut self)`, `is_ui_running(&mut self) -> bool`, `stop_all(&mut self)` (graceful: `WM_CLOSE` to `ui`'s `GroveShellBar` window, then IPC shutdown to host/watchdog, force-kill fallback), `start_all(&mut self)` (alias for `spawn_all` used by the "Start GroveShell" action). Later tasks (6, 9) call `ManagedProcesses::spawn_all`/`stop_all`/`is_ui_running` from the tray menu and Home page.

- [ ] **Step 1: Write the pure-logic test for the exe-path resolution helper**

`process.rs` needs to find sibling `.exe`s next to its own — this is the one pure, testable piece of this task. Create `apps/settings/src/imp/process.rs` and start with:

```rust
//! Spawns and supervises `groveshell-watchdog`, `groveshell-host`, and
//! `groveshell-ui` — the same order `scripts/dev-start.ps1` uses for
//! development, now owned by this tray app for real end-user launches.

use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::Duration;

/// Resolves a sibling executable's path: the same directory this process's
/// own `.exe` lives in, joined with `name`. All four GroveShell binaries
/// are always built into the same `target/<profile>` directory and, in a
/// real install, would ship in the same install directory — there is no
/// scenario in this codebase where they live apart.
fn sibling_exe_path(name: &str) -> PathBuf {
    let mut path = std::env::current_exe().expect("current_exe should always resolve");
    path.pop();
    path.push(name);
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sibling_exe_path_is_alongside_the_current_exe() {
        let current = std::env::current_exe().unwrap();
        let expected_dir = current.parent().unwrap().to_path_buf();
        let resolved = sibling_exe_path("groveshell-ui.exe");
        assert_eq!(resolved.parent().unwrap(), expected_dir);
        assert_eq!(resolved.file_name().unwrap(), "groveshell-ui.exe");
    }
}
```

- [ ] **Step 2: Run the test to verify it passes (this one isn't TDD-red first, since it's a thin wrapper — confirm it works)**

Run: `cargo test -p groveshell-settings sibling_exe_path`
Expected: PASS

- [ ] **Step 3: Add process spawning and the `GroveShellBar` finder**

Append to `apps/settings/src/imp/process.rs`:

```rust
pub struct ManagedProcesses {
    watchdog: Option<Child>,
    host: Option<Child>,
    ui: Option<Child>,
}

impl ManagedProcesses {
    pub fn new() -> Self {
        Self { watchdog: None, host: None, ui: None }
    }

    /// Spawns watchdog -> host -> ui in order, same sequence
    /// `scripts/dev-start.ps1` uses. Best-effort: a spawn failure is
    /// logged, not fatal, so e.g. a missing `groveshell-ui.exe` (a
    /// dev-only partial build) doesn't crash the tray app itself.
    pub fn spawn_all(&mut self) {
        self.watchdog = spawn_hidden("groveshell-watchdog.exe");
        std::thread::sleep(Duration::from_secs(1));
        self.host = spawn_hidden("groveshell-host.exe");
        self.ui = spawn_hidden("groveshell-ui.exe");
    }

    /// Alias used by the "Start GroveShell" tray/Home-page action — spawns
    /// fresh children regardless of any previous (now-exited) ones.
    pub fn start_all(&mut self) {
        self.spawn_all();
    }

    pub fn is_ui_running(&mut self) -> bool {
        matches!(self.ui.as_mut().map(|c| c.try_wait()), Some(Ok(None)))
    }

    /// Stops `ui` gracefully (so it restores the real taskbar/work areas
    /// in its own `WM_DESTROY` handler), then asks `host`/`watchdog` to
    /// shut down over IPC, force-killing anything still alive after a
    /// short grace period. See `apps/ui/src/imp/mod.rs`'s `WM_DESTROY`
    /// handler and `scripts/dev-start.ps1`'s `Stop-UiGracefully` for the
    /// precedent this mirrors.
    pub fn stop_all(&mut self) {
        stop_ui_gracefully(self.ui.take());
        stop_via_ipc_or_kill("groveshell-host", self.host.take(), groveshell_ipc::message_type::SHUTDOWN);
        stop_via_ipc_or_kill(
            "groveshell-watchdog",
            self.watchdog.take(),
            groveshell_ipc::message_type::WATCHDOG_SHUTDOWN,
        );
    }
}

fn spawn_hidden(exe_name: &str) -> Option<Child> {
    let path = sibling_exe_path(exe_name);
    match Command::new(&path).spawn() {
        Ok(child) => {
            tracing::info!(exe = exe_name, pid = child.id(), "spawned");
            Some(child)
        }
        Err(e) => {
            tracing::error!(exe = exe_name, error = ?e, "failed to spawn");
            None
        }
    }
}

/// Posts `WM_CLOSE` to the `GroveShellBar`-classed window belonging to
/// `ui_child`'s pid (triggering `ui`'s own taskbar-restore `WM_DESTROY`
/// logic), waits up to 3 seconds for graceful exit, then force-kills.
fn stop_ui_gracefully(ui_child: Option<Child>) {
    let Some(mut child) = ui_child else { return };
    let pid = child.id();

    if let Some(bar_hwnd) = find_window_by_class_and_pid("GroveShellBar", pid) {
        // SAFETY: `bar_hwnd` was just found via `EnumWindows` and is a
        // plain message post with no ownership implications.
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(
                bar_hwnd,
                windows::Win32::UI::WindowsAndMessaging::WM_CLOSE,
                windows::Win32::Foundation::WPARAM(0),
                windows::Win32::Foundation::LPARAM(0),
            );
        }
    }

    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(_) => return,
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn stop_via_ipc_or_kill(pipe_name: &str, child: Option<Child>, shutdown_message_type: &str) {
    let Some(mut child) = child else { return };

    if let Ok(mut conn) = groveshell_ipc::pipe::connect(pipe_name) {
        let envelope = groveshell_ipc::Envelope::new("groveshell-settings", shutdown_message_type, serde_json::json!({}));
        let _ = groveshell_ipc::framing::write_envelope(&mut conn, &envelope);
    }

    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while std::time::Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(_) => return,
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// `EnumWindows` pass matching both class name and owning pid — the same
/// two-part match `scripts/dev-start.ps1`'s `Stop-UiGracefully` performs
/// via .NET interop, ported to a direct Win32 call here.
fn find_window_by_class_and_pid(class_name: &str, pid: u32) -> Option<windows::Win32::Foundation::HWND> {
    use windows::Win32::Foundation::{BOOL, HWND, LPARAM, TRUE};
    use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, GetClassNameW, GetWindowThreadProcessId};

    struct SearchState<'a> {
        class_name: &'a str,
        pid: u32,
        found: Option<HWND>,
    }

    unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: `lparam` was created from a live `&mut SearchState` in
        // the call below and this callback only runs synchronously within
        // that call's lifetime.
        let state = &mut *(lparam.0 as *mut SearchState);
        let mut window_pid = 0u32;
        // SAFETY: `hwnd` is supplied live by `EnumWindows`.
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut window_pid)) };
        if window_pid != state.pid {
            return TRUE;
        }
        let mut buf = [0u16; 256];
        // SAFETY: `buf` outlives this call and is large enough for any
        // real window class name.
        let len = unsafe { GetClassNameW(hwnd, &mut buf) };
        let name = String::from_utf16_lossy(&buf[..len as usize]);
        if name == state.class_name {
            state.found = Some(hwnd);
            return BOOL(0); // stop enumerating
        }
        TRUE
    }

    let mut state = SearchState { class_name, pid, found: None };
    // SAFETY: `state`'s address is passed as `lparam` and only read back by
    // `enum_proc`, synchronously, within this call's lifetime.
    unsafe {
        let _ = EnumWindows(Some(enum_proc), LPARAM(&mut state as *mut SearchState as isize));
    }
    state.found
}
```

- [ ] **Step 4: Wire the module into `imp/mod.rs`**

In `apps/settings/src/imp/mod.rs`, add `mod process;` near the top and replace the placeholder sleep loop:

```rust
mod process;

use process::ManagedProcesses;
```

Replace the `loop { std::thread::sleep(...) }` body at the end of `run()` with:

```rust
    let mut processes = ManagedProcesses::new();
    processes.spawn_all();

    // Task 6 replaces this with a real Win32 message loop driving the
    // tray icon; for now, idle so the spawned children keep running under
    // this process's supervision (and this process's own exit, e.g.
    // Ctrl+C in a foreground dev run, doesn't leave them behind untracked
    // — Task 6's WM_DESTROY-equivalent handles a clean stop).
    loop {
        std::thread::sleep(std::time::Duration::from_secs(60));
    }
```

- [ ] **Step 5: Build, run test, manual smoke test**

Run: `cargo test -p groveshell-settings` — expect PASS.
Run: `cargo build --workspace` (so `groveshell-watchdog.exe`/`groveshell-host.exe`/`groveshell-ui.exe` exist alongside), then run `target/debug/groveshell-settings.exe` and confirm (via Task Manager) that `groveshell-watchdog`, `groveshell-host`, and `groveshell-ui` all start, and that GroveShell's bar/dock become visible on screen exactly as they do under `dev-start.ps1`.

- [ ] **Step 6: Commit**

```bash
git add apps/settings/src/imp/process.rs apps/settings/src/imp/mod.rs
git commit -m "feat(settings): spawn and gracefully stop watchdog/host/ui"
```

---

### Task 6: Tray icon, context menu, and settings-window shell creation

**Files:**
- Create: `apps/settings/src/imp/tray.rs`
- Modify: `apps/settings/src/imp/mod.rs`

**Interfaces:**
- Consumes: `process::ManagedProcesses` (Task 5).
- Produces: a real Win32 message-only-adjacent window (`GroveShellSettingsMain` class, hidden, receives the tray callback) plus `Shell_NotifyIconW`. Public function `pub fn run_message_loop(processes: ManagedProcesses) -> Result<()>` replacing the placeholder idle loop. Defines `pub(crate) const WM_TRAYICON: u32` (an app-private `WM_APP`-based message) and menu command IDs `MENU_ID_OPEN`, `MENU_ID_TOGGLE`, `MENU_ID_EXIT` that Task 9 (Home page) also uses for its equivalent buttons by calling the same `toggle_groveshell()`/`open_settings_window()` functions this task defines (not by reposting these menu IDs — see Task 9).

- [ ] **Step 1: Write `tray.rs`**

Create `apps/settings/src/imp/tray.rs`:

```rust
//! The system tray icon and its right-click context menu.

use std::cell::RefCell;

use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{CreateSolidBrush, COLORREF};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DispatchMessageW,
    GetCursorPos, GetMessageW, LoadCursorW, LoadImageW, PostQuitMessage, RegisterClassW,
    SetForegroundWindow, TrackPopupMenu, TranslateMessage, IDC_ARROW, IMAGE_ICON, LR_DEFAULTSIZE,
    MF_STRING, MSG, TPM_LEFTALIGN, TPM_RETURNCMD, TPM_TOPALIGN, WM_APP, WM_DESTROY,
    WM_LBUTTONUP, WM_RBUTTONUP, WNDCLASSW, WS_EX_TOOLWINDOW, WS_POPUP,
};

use super::process::ManagedProcesses;

pub(crate) const WM_TRAYICON: u32 = WM_APP + 1;
const MENU_ID_OPEN: u32 = 1;
const MENU_ID_TOGGLE: u32 = 2;
const MENU_ID_EXIT: u32 = 3;

thread_local! {
    static PROCESSES: RefCell<Option<ManagedProcesses>> = const { RefCell::new(None) };
}

/// Loads the icon this exe embedded as resource ID 1 (see `build.rs`) at
/// the small size appropriate for a tray icon / window class icon.
fn load_app_icon() -> windows::Win32::UI::WindowsAndMessaging::HICON {
    // SAFETY: `GetModuleHandleW(None)` returns this process's own module
    // handle; resource ID 1 was embedded by this exe's own `build.rs`.
    unsafe {
        let hinstance = GetModuleHandleW(None).expect("own module handle always resolves");
        let handle = LoadImageW(
            Some(hinstance.into()),
            windows::core::PCWSTR(1 as *const u16),
            IMAGE_ICON,
            0,
            0,
            LR_DEFAULTSIZE,
        )
        .unwrap_or_default();
        windows::Win32::UI::WindowsAndMessaging::HICON(handle.0)
    }
}

/// Creates the tray icon and settings-window shell, then runs the message
/// loop for the process's lifetime — analogous to `apps/ui`'s own `main()`
/// message loop, but for this single hidden window plus the settings
/// window Task 8 creates alongside it.
pub fn run_message_loop(processes: ManagedProcesses) -> groveshell_common::Result<()> {
    PROCESSES.with(|p| *p.borrow_mut() = Some(processes));

    // SAFETY: every call below either has its own safety comment or is a
    // plain value/query with no aliasing or lifetime requirements.
    unsafe {
        let hinstance = GetModuleHandleW(None).map_err(groveshell_common::Error::Windows)?;
        let hinstance = windows::Win32::Foundation::HINSTANCE(hinstance.0);

        let class = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance,
            lpszClassName: w!("GroveShellSettingsMain"),
            hCursor: LoadCursorW(None, IDC_ARROW).map_err(groveshell_common::Error::Windows)?,
            hbrBackground: CreateSolidBrush(COLORREF(0x00202020)),
            ..Default::default()
        };
        if windows::Win32::UI::WindowsAndMessaging::RegisterClassW(&class) == 0 {
            return Err(groveshell_common::Error::Windows(windows::core::Error::from_win32()));
        }

        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            w!("GroveShellSettingsMain"),
            w!("GroveShell Settings"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            hinstance,
            None,
        )
        .map_err(groveshell_common::Error::Windows)?;

        add_tray_icon(hwnd);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    Ok(())
}

fn add_tray_icon(hwnd: HWND) {
    let mut data = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
        uCallbackMessage: WM_TRAYICON,
        hIcon: load_app_icon(),
        ..Default::default()
    };
    let tip = "GroveShell\0".encode_utf16().collect::<Vec<_>>();
    data.szTip[..tip.len()].copy_from_slice(&tip);
    // SAFETY: `data` is a fully-initialized, valid `NOTIFYICONDATAW` for
    // the duration of this synchronous call.
    unsafe {
        let _ = Shell_NotifyIconW(NIM_ADD, &data);
    }
}

fn remove_tray_icon(hwnd: HWND) {
    let data = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        ..Default::default()
    };
    // SAFETY: same contract as `add_tray_icon`.
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &data);
    }
}

fn is_ui_running() -> bool {
    PROCESSES.with(|p| p.borrow_mut().as_mut().map(|proc| proc.is_ui_running()).unwrap_or(false))
}

/// Restore-Explorer (if `ui` is running) or Start-GroveShell (if it isn't)
/// — shared by the tray menu's "Toggle" item and Task 9's Home-page
/// button, both of which call this same function rather than duplicating
/// the stop/start logic or reposting a synthetic menu command.
pub(crate) fn toggle_groveshell() {
    let running = is_ui_running();
    PROCESSES.with(|p| {
        if let Some(proc) = p.borrow_mut().as_mut() {
            if running {
                proc.stop_all();
            } else {
                proc.start_all();
            }
        }
    });
}

fn show_context_menu(hwnd: HWND) {
    // SAFETY: standard synchronous popup-menu sequence, same shape as
    // `apps/ui/src/imp/dock.rs`'s `show_context_menu`.
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let _ = AppendMenuW(menu, MF_STRING, MENU_ID_OPEN as usize, w!("Open GroveShell Settings"));
        let toggle_label = if is_ui_running() { w!("Restore Explorer") } else { w!("Start GroveShell") };
        let _ = AppendMenuW(menu, MF_STRING, MENU_ID_TOGGLE as usize, toggle_label);
        let _ = AppendMenuW(menu, MF_STRING, MENU_ID_EXIT as usize, w!("Exit GroveShell"));

        let mut point = windows::Win32::Foundation::POINT::default();
        let _ = GetCursorPos(&mut point);
        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_LEFTALIGN | TPM_TOPALIGN,
            point.x,
            point.y,
            0,
            hwnd,
            None,
        );
        let _ = DestroyMenu(menu);

        match cmd.0 as u32 {
            MENU_ID_OPEN => super::window::open_settings_window(),
            MENU_ID_TOGGLE => toggle_groveshell(),
            MENU_ID_EXIT => {
                toggle_groveshell(); // stops everything if running; no-op if already stopped
                PostQuitMessage(0);
            }
            _ => {}
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_TRAYICON => {
            let event = lparam.0 as u32;
            if event == WM_LBUTTONUP {
                super::window::open_settings_window();
            } else if event == WM_RBUTTONUP {
                show_context_menu(hwnd);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            remove_tray_icon(hwnd);
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
```

Note: this references `super::window::open_settings_window()`, which Task 8 defines. Until Task 8 lands, add a temporary stub so this compiles — Task 8's Step 3 replaces it:

Create `apps/settings/src/imp/window.rs` with just:

```rust
//! Settings window shell — full implementation in Task 8.

pub(crate) fn open_settings_window() {
    tracing::info!("open_settings_window: not yet implemented (Task 8)");
}
```

- [ ] **Step 2: Wire `tray`/`window` modules into `imp/mod.rs`**

Add `mod tray;` and `mod window;` near the top of `apps/settings/src/imp/mod.rs`. Replace the final idle loop (from Task 5, Step 4) with:

```rust
    tray::run_message_loop(processes)
```

- [ ] **Step 3: Build and manually verify**

Run: `cargo build -p groveshell-settings`
Expected: builds cleanly.
Manual verification: run `target/debug/groveshell-settings.exe`, confirm a tray icon appears with the GroveShell logo; right-click shows the three-item menu with the correct toggle label; left-click logs the "not yet implemented" line (visible in `%LOCALAPPDATA%\GroveShell\logs\settings.log`); "Exit GroveShell" removes the tray icon and the process exits.

- [ ] **Step 4: Commit**

```bash
git add apps/settings/src/imp/tray.rs apps/settings/src/imp/window.rs apps/settings/src/imp/mod.rs
git commit -m "feat(settings): add the tray icon and its context menu"
```

---

### Task 7: Health & stats sampling

**Files:**
- Create: `apps/settings/src/imp/health.rs`

**Interfaces:**
- Produces: `pub struct ProcessSample { pub pid: u32, pub cpu_percent: f32, pub working_set_bytes: u64 }`, `pub fn sample_process(pid: u32) -> Option<ProcessSample>` (blocks ~200ms internally to take two `GetProcessTimes` readings), `pub fn cpu_percent_from_times(kernel_before: u64, user_before: u64, kernel_after: u64, user_after: u64, wall_elapsed: Duration) -> f32` (the pure, testable core), `pub fn host_ping_ok(timeout: Duration) -> bool`. Consumed by Task 9's Home page.

- [ ] **Step 1: Write the failing test for the pure CPU% calculation**

Create `apps/settings/src/imp/health.rs`:

```rust
//! Per-process CPU%/RAM sampling and overall health determination for the
//! Home page (Task 9). No new IPC protocol: liveness is "is the PID still
//! present," CPU/RAM come from direct `GetProcessTimes`/
//! `GetProcessMemoryInfo` calls, and overall health additionally requires
//! a successful `host.ping` round trip.

use std::time::Duration;

/// `GetProcessTimes` reports kernel/user time in 100-nanosecond units.
/// Given two samples taken `wall_elapsed` apart, this is the standard
/// "(kernel delta + user delta) / wall delta" CPU% calculation, clamped to
/// `[0.0, 100.0 * number_of_cores]`-agnostic single-process percentage
/// (matching Task Manager's "single core = 100%" convention, not
/// normalized across cores, since there is no per-core breakdown needed
/// for a simple health display).
pub fn cpu_percent_from_times(
    kernel_before: u64,
    user_before: u64,
    kernel_after: u64,
    user_after: u64,
    wall_elapsed: Duration,
) -> f32 {
    if wall_elapsed.is_zero() {
        return 0.0;
    }
    let cpu_ticks = (kernel_after.saturating_sub(kernel_before))
        + (user_after.saturating_sub(user_before));
    let cpu_seconds = cpu_ticks as f64 / 10_000_000.0; // 100ns units -> seconds
    let wall_seconds = wall_elapsed.as_secs_f64();
    ((cpu_seconds / wall_seconds) * 100.0).max(0.0) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_cpu_time_over_one_second_is_zero_percent() {
        let pct = cpu_percent_from_times(0, 0, 0, 0, Duration::from_secs(1));
        assert_eq!(pct, 0.0);
    }

    #[test]
    fn half_a_second_of_cpu_time_over_one_wall_second_is_fifty_percent() {
        // 0.5s = 5_000_000 (100ns units), split between kernel and user.
        let pct = cpu_percent_from_times(0, 0, 2_500_000, 2_500_000, Duration::from_secs(1));
        assert!((pct - 50.0).abs() < 0.01, "expected ~50.0, got {pct}");
    }

    #[test]
    fn full_cpu_saturation_over_one_wall_second_is_one_hundred_percent() {
        let pct = cpu_percent_from_times(0, 0, 10_000_000, 0, Duration::from_secs(1));
        assert!((pct - 100.0).abs() < 0.01, "expected ~100.0, got {pct}");
    }

    #[test]
    fn zero_wall_elapsed_is_zero_percent_not_a_divide_by_zero() {
        let pct = cpu_percent_from_times(0, 0, 5_000_000, 0, Duration::ZERO);
        assert_eq!(pct, 0.0);
    }
}
```

- [ ] **Step 2: Run tests to verify they pass**

Run: `cargo test -p groveshell-settings cpu_percent`
Expected: PASS (this is a pure function with no Win32 dependency, so it's correct on the first write, but running it confirms the arithmetic).

- [ ] **Step 3: Add the Win32-backed sampling and liveness/ping functions**

Append to `apps/settings/src/imp/health.rs`:

```rust
pub struct ProcessSample {
    pub pid: u32,
    pub cpu_percent: f32,
    pub working_set_bytes: u64,
}

/// Takes two `GetProcessTimes` readings 200ms apart to compute a CPU%
/// snapshot, plus one `GetProcessMemoryInfo` reading for working-set
/// memory. Returns `None` if the process can't be opened (already exited,
/// or — unlikely for GroveShell's own unelevated children — permission
/// denied).
pub fn sample_process(pid: u32) -> Option<ProcessSample> {
    use windows::Win32::Foundation::{CloseHandle, FILETIME};
    use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
    use windows::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ,
    };

    fn filetime_to_u64(ft: FILETIME) -> u64 {
        ((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64
    }

    // SAFETY: `pid` is caller-supplied; `OpenProcess` documented-fails
    // (returns `Err`) for an invalid or inaccessible pid rather than
    // aliasing anything.
    let handle = unsafe {
        OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ, false, pid)
    }
    .ok()?;

    let read_times = |h: windows::Win32::Foundation::HANDLE| -> Option<(u64, u64)> {
        let (mut creation, mut exit, mut kernel, mut user) =
            (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
        // SAFETY: `h` is the handle opened above, valid for this call;
        // every out-param is a local outliving the call.
        unsafe { GetProcessTimes(h, &mut creation, &mut exit, &mut kernel, &mut user) }.ok()?;
        Some((filetime_to_u64(kernel), filetime_to_u64(user)))
    };

    let (kernel_before, user_before) = read_times(handle)?;
    std::thread::sleep(Duration::from_millis(200));
    let (kernel_after, user_after) = read_times(handle)?;

    let mut counters = PROCESS_MEMORY_COUNTERS {
        cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        ..Default::default()
    };
    // SAFETY: `handle` is still valid; `counters` is a local outliving
    // the call.
    let working_set_bytes = unsafe {
        GetProcessMemoryInfo(handle, &mut counters, counters.cb)
    }
    .map(|_| counters.WorkingSetSize as u64)
    .unwrap_or(0);

    // SAFETY: `handle` was opened by this function and is not used past
    // this point.
    unsafe {
        let _ = CloseHandle(handle);
    }

    Some(ProcessSample {
        pid,
        cpu_percent: cpu_percent_from_times(kernel_before, user_before, kernel_after, user_after, Duration::from_millis(200)),
        working_set_bytes,
    })
}

/// A `host.ping` round trip within `timeout`. `groveshell_ipc::pipe`'s
/// `connect`/read/write calls are all synchronous and blocking (see
/// `crates/ipc/src/pipe.rs`), so the timeout here is a wall-clock check
/// around the whole exchange rather than a socket-level timeout — good
/// enough for a health indicator that's re-checked every couple of
/// seconds anyway.
pub fn host_ping_ok(timeout: Duration) -> bool {
    let started = std::time::Instant::now();
    let Ok(mut conn) = groveshell_ipc::pipe::connect("groveshell-host") else {
        return false;
    };
    let request = groveshell_ipc::Envelope::new(
        "groveshell-settings",
        groveshell_ipc::message_type::PING,
        serde_json::json!({}),
    );
    if groveshell_ipc::framing::write_envelope(&mut conn, &request).is_err() {
        return false;
    }
    let Ok(response) = groveshell_ipc::framing::read_envelope(&mut conn) else {
        return false;
    };
    response.message_type == groveshell_ipc::message_type::PONG && started.elapsed() <= timeout
}
```

- [ ] **Step 4: Run all settings tests**

Run: `cargo test -p groveshell-settings`
Expected: PASS

- [ ] **Step 5: Wire the module into `imp/mod.rs`**

Add `mod health;` near the top of `apps/settings/src/imp/mod.rs`.

- [ ] **Step 6: Commit**

```bash
git add apps/settings/src/imp/health.rs apps/settings/src/imp/mod.rs
git commit -m "feat(settings): add per-process CPU/RAM sampling and host.ping liveness check"
```

---

### Task 8: Settings window shell (theme, nav list, page dispatch)

**Files:**
- Create: `apps/settings/src/imp/theme.rs`
- Create: `apps/settings/src/imp/nav.rs`
- Create: `apps/settings/src/imp/pages/mod.rs`
- Modify: `apps/settings/src/imp/window.rs` (replace the Task 6 stub with the real window)
- Modify: `apps/settings/src/imp/mod.rs`

**Interfaces:**
- Produces:
  - `theme.rs`: color constants (`TEXT`, `TEXT_MUTED`, `BG_WINDOW`, `BG_PANEL`, `BG_NAV`, `ACCENT`) as `COLORREF`s matching `apps/ui`'s literals, plus owner-drawn widget helpers `draw_toggle(hdc, rect, on: bool)`, `hit_toggle(rect, x, y) -> bool`, `draw_slider(hdc, rect, value: f32, min: f32, max: f32)`, `value_from_slider_x(rect, x, min, max) -> f32`, `draw_segmented(hdc, rect, options: &[&str], selected: usize)`, `segmented_hit(rect, options: &[&str], x, y) -> Option<usize>`.
  - `nav.rs`: `pub const NAV_ITEMS: &[&str] = &["Home", "Dock", "Top Bar", "Overview", "Input"]`, `nav_layout(client_height: i32) -> Vec<RECT>`, `nav_hit_test(x: i32, y: i32) -> Option<usize>`.
  - `pages/mod.rs`: `pub trait Page { fn paint(&self, hdc: HDC, content_rect: RECT); fn on_click(&mut self, x: i32, y: i32, content_rect: RECT); }` — Tasks 9/15/16/17/18 each implement this for their page.
  - `window.rs`: real `open_settings_window()` creating/showing a `GroveShellSettingsWindow`-classed window; its `wndproc` paints the nav list (left, ~180px) plus the currently selected page's content (right), and dispatches `WM_LBUTTONDOWN` to nav-hit-test or the active page's `on_click`.

- [ ] **Step 1: Write `theme.rs`**

Create `apps/settings/src/imp/theme.rs`:

```rust
//! Shared color palette and owner-drawn widgets for the settings window,
//! matching `apps/ui`'s established literal colors (bar/calendar/quick
//! settings) so this window doesn't look like a different app.

use windows::Win32::Foundation::{COLORREF, RECT};
use windows::Win32::Graphics::Gdi::{
    CreateSolidBrush, DeleteObject, Ellipse, GetStockObject, RoundRect, SelectObject, HDC,
    HOLLOW_BRUSH, NULL_PEN,
};

pub(crate) const TEXT: COLORREF = COLORREF(0x00E0E0E0);
pub(crate) const TEXT_MUTED: COLORREF = COLORREF(0x00A0A0A0);
pub(crate) const BG_WINDOW: u32 = 0x00202020;
pub(crate) const BG_PANEL: COLORREF = COLORREF(0x00262626);
pub(crate) const BG_NAV: COLORREF = COLORREF(0x00303030);
pub(crate) const ACCENT: COLORREF = COLORREF(0x00FFA860);
pub(crate) const NAV_WIDTH: i32 = 180;

/// Fills `rect` with `color`, no border — same idiom as
/// `apps/ui/src/imp/quick_settings.rs`'s `fill_round_rect`.
pub(crate) unsafe fn fill_round_rect(hdc: HDC, rect: RECT, radius: i32, color: COLORREF) {
    let brush = CreateSolidBrush(color);
    let previous_brush = SelectObject(hdc, brush);
    let previous_pen = SelectObject(hdc, GetStockObject(NULL_PEN));
    let _ = RoundRect(hdc, rect.left, rect.top, rect.right, rect.bottom, radius * 2, radius * 2);
    SelectObject(hdc, previous_brush);
    SelectObject(hdc, previous_pen);
    let _ = DeleteObject(brush);
}

/// A toggle switch: a pill-shaped track plus a circular thumb, filled with
/// the accent color when `on`.
pub(crate) unsafe fn draw_toggle(hdc: HDC, rect: RECT, on: bool) {
    let track_color = if on { ACCENT } else { COLORREF(0x00505050) };
    fill_round_rect(hdc, rect, (rect.bottom - rect.top) / 2, track_color);
    let thumb_d = rect.bottom - rect.top - 4;
    let thumb_x = if on { rect.right - thumb_d - 2 } else { rect.left + 2 };
    let thumb_rect = RECT { left: thumb_x, top: rect.top + 2, right: thumb_x + thumb_d, bottom: rect.bottom - 2 };
    let brush = CreateSolidBrush(COLORREF(0x00FFFFFF));
    let previous_brush = SelectObject(hdc, brush);
    let previous_pen = SelectObject(hdc, GetStockObject(NULL_PEN));
    let _ = Ellipse(hdc, thumb_rect.left, thumb_rect.top, thumb_rect.right, thumb_rect.bottom);
    SelectObject(hdc, previous_brush);
    SelectObject(hdc, previous_pen);
    let _ = DeleteObject(brush);
}

pub(crate) fn hit_toggle(rect: RECT, x: i32, y: i32) -> bool {
    x >= rect.left && x < rect.right && y >= rect.top && y < rect.bottom
}

/// A slider: a track plus an accent-filled portion up to `value` (clamped
/// into `[min, max]`) and a round thumb — visually the same shape as
/// `apps/ui/src/imp/quick_settings.rs`'s volume slider.
pub(crate) unsafe fn draw_slider(hdc: HDC, rect: RECT, value: f32, min: f32, max: f32) {
    let track_h = (rect.bottom - rect.top).min(6);
    let track = RECT {
        left: rect.left,
        top: (rect.top + rect.bottom) / 2 - track_h / 2,
        right: rect.right,
        bottom: (rect.top + rect.bottom) / 2 + track_h / 2,
    };
    fill_round_rect(hdc, track, track_h / 2, COLORREF(0x00383838));
    let fraction = ((value - min) / (max - min).max(f32::EPSILON)).clamp(0.0, 1.0);
    let fill_right = track.left + ((track.right - track.left) as f32 * fraction).round() as i32;
    if fill_right > track.left {
        let fill = RECT { left: track.left, right: fill_right.max(track.left + track_h), ..track };
        fill_round_rect(hdc, fill, track_h / 2, ACCENT);
    }
    let thumb_r = 7;
    let thumb_cy = (track.top + track.bottom) / 2;
    let brush = CreateSolidBrush(COLORREF(0x00FFFFFF));
    let previous_brush = SelectObject(hdc, brush);
    let previous_pen = SelectObject(hdc, GetStockObject(NULL_PEN));
    let _ = Ellipse(hdc, fill_right - thumb_r, thumb_cy - thumb_r, fill_right + thumb_r, thumb_cy + thumb_r);
    SelectObject(hdc, previous_brush);
    SelectObject(hdc, previous_pen);
    let _ = DeleteObject(brush);
}

pub(crate) fn value_from_slider_x(rect: RECT, x: i32, min: f32, max: f32) -> f32 {
    let width = (rect.right - rect.left).max(1) as f32;
    let fraction = ((x - rect.left) as f32 / width).clamp(0.0, 1.0);
    min + fraction * (max - min)
}

/// A three-(or-fewer)-way segmented control: equal-width pill segments,
/// the selected one filled with the accent color.
pub(crate) unsafe fn draw_segmented(hdc: HDC, rect: RECT, options: &[&str], selected: usize) {
    use super::util_text::draw_centered_text;
    let n = options.len().max(1) as i32;
    let seg_w = (rect.right - rect.left) / n;
    let radius = (rect.bottom - rect.top) / 2;
    fill_round_rect(hdc, rect, radius, COLORREF(0x00383838));
    for (i, label) in options.iter().enumerate() {
        let seg_rect = RECT {
            left: rect.left + i as i32 * seg_w,
            top: rect.top,
            right: rect.left + (i as i32 + 1) * seg_w,
            bottom: rect.bottom,
        };
        if i == selected {
            fill_round_rect(hdc, seg_rect, radius, ACCENT);
        }
        draw_centered_text(hdc, seg_rect, label, if i == selected { COLORREF(0x00202020) } else { TEXT });
    }
}

pub(crate) fn segmented_hit(rect: RECT, options: &[&str], x: i32, y: i32) -> Option<usize> {
    if y < rect.top || y >= rect.bottom || x < rect.left || x >= rect.right {
        return None;
    }
    let n = options.len().max(1) as i32;
    let seg_w = (rect.right - rect.left) / n;
    Some((((x - rect.left) / seg_w).clamp(0, n - 1)) as usize)
}
```

- [ ] **Step 2: Write the small text-drawing helper module it depends on**

Create `apps/settings/src/imp/util_text.rs`:

```rust
//! Text drawing, factored out of `theme.rs` so `draw_segmented` can use it
//! without a circular import — mirrors `apps/ui/src/imp/util.rs`'s
//! `draw_text_in`/`bar_font` pair.

use windows::core::w;
use windows::Win32::Foundation::{COLORREF, RECT};
use windows::Win32::Graphics::Gdi::{
    CreateFontW, DrawTextW, SetBkMode, SetTextColor, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS,
    DEFAULT_CHARSET, DEFAULT_PITCH, DT_CENTER, DT_SINGLELINE, DT_VCENTER, HDC, OUT_DEFAULT_PRECIS,
    SelectObject, DeleteObject, TRANSPARENT,
};

pub(crate) fn ui_font() -> windows::Win32::Graphics::Gdi::HFONT {
    // SAFETY: plain object creation, no aliasing or lifetime preconditions.
    unsafe {
        CreateFontW(
            -14, 0, 0, 0, 400, 0, 0, 0,
            DEFAULT_CHARSET.0.into(),
            OUT_DEFAULT_PRECIS.0.into(),
            CLIP_DEFAULT_PRECIS.0.into(),
            CLEARTYPE_QUALITY.0.into(),
            DEFAULT_PITCH.0.into(),
            w!("Segoe UI"),
        )
    }
}

pub(crate) unsafe fn draw_centered_text(hdc: HDC, rect: RECT, text: &str, color: COLORREF) {
    let font = ui_font();
    let previous = SelectObject(hdc, font);
    SetBkMode(hdc, TRANSPARENT);
    SetTextColor(hdc, color);
    let mut wide: Vec<u16> = text.encode_utf16().collect();
    let mut r = rect;
    DrawTextW(hdc, &mut wide, &mut r, DT_CENTER | DT_SINGLELINE | DT_VCENTER);
    SelectObject(hdc, previous);
    let _ = DeleteObject(font);
}
```

- [ ] **Step 3: Write `nav.rs`**

Create `apps/settings/src/imp/nav.rs`:

```rust
//! The settings window's left-hand vertical nav list.

use windows::Win32::Foundation::RECT;

pub(crate) const NAV_ITEMS: &[&str] = &["Home", "Dock", "Top Bar", "Overview", "Input"];
const NAV_ITEM_HEIGHT: i32 = 44;

/// One rect per nav item, top-to-bottom, each `NAV_ITEM_HEIGHT` tall and
/// `crate::imp::theme::NAV_WIDTH` wide — pure function of nothing but the
/// constants above, so painting and hit-testing can never disagree, same
/// pattern as `apps/ui`'s `card_layout`/`qs_layout`.
pub(crate) fn nav_layout() -> Vec<RECT> {
    (0..NAV_ITEMS.len())
        .map(|i| RECT {
            left: 0,
            top: i as i32 * NAV_ITEM_HEIGHT,
            right: super::theme::NAV_WIDTH,
            bottom: (i as i32 + 1) * NAV_ITEM_HEIGHT,
        })
        .collect()
}

pub(crate) fn nav_hit_test(x: i32, y: i32) -> Option<usize> {
    if x < 0 || x >= super::theme::NAV_WIDTH {
        return None;
    }
    nav_layout().iter().position(|r| y >= r.top && y < r.bottom)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nav_hit_test_finds_the_first_item_at_the_top() {
        assert_eq!(nav_hit_test(10, 5), Some(0));
    }

    #[test]
    fn nav_hit_test_finds_the_last_item() {
        let last_top = nav_layout().last().unwrap().top;
        assert_eq!(nav_hit_test(10, last_top + 5), Some(NAV_ITEMS.len() - 1));
    }

    #[test]
    fn nav_hit_test_outside_the_nav_width_returns_none() {
        assert_eq!(nav_hit_test(super::super::theme::NAV_WIDTH + 10, 5), None);
    }

    #[test]
    fn nav_hit_test_below_the_last_item_returns_none() {
        let below = nav_layout().last().unwrap().bottom + 100;
        assert_eq!(nav_hit_test(10, below), None);
    }
}
```

- [ ] **Step 4: Write `pages/mod.rs`**

Create `apps/settings/src/imp/pages/mod.rs`:

```rust
//! `Page` trait implemented by each settings screen (Home, Dock, Top Bar,
//! Overview, Input) — see Tasks 9, 15, 16, 17, 18.

use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Gdi::HDC;

pub(crate) trait Page {
    /// Paints this page's content into `content_rect` (already excludes
    /// the nav list — the area to the right of it).
    fn paint(&self, hdc: HDC, content_rect: RECT);
    /// Handles a left-click at window-client-relative `(x, y)`, given the
    /// same `content_rect` `paint` was last called with.
    fn on_click(&mut self, x: i32, y: i32, content_rect: RECT);
}
```

- [ ] **Step 5: Run the nav tests**

Run: `cargo test -p groveshell-settings nav_hit_test`
Expected: PASS

- [ ] **Step 6: Replace the Task 6 stub `window.rs` with the real settings window**

Overwrite `apps/settings/src/imp/window.rs`:

```rust
//! The settings window: a left-hand nav list plus the currently selected
//! page's content pane, both owner-drawn.

use std::cell::RefCell;

use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, FillRect, GetClientRect, PAINTSTRUCT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, IsWindowVisible, LoadCursorW, RegisterClassW, SetForegroundWindow,
    ShowWindow, CreateSolidBrush, IDC_ARROW, SW_RESTORE, SW_SHOW, WM_DESTROY, WM_LBUTTONDOWN, WM_PAINT,
    WNDCLASSW, WS_EX_TOOLWINDOW, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
};

use super::nav::{nav_hit_test, nav_layout, NAV_ITEMS};
use super::pages::Page;
use super::pages::home::HomePage;
use super::theme::{BG_NAV, BG_WINDOW, NAV_WIDTH};
use super::util_text::draw_centered_text;

thread_local! {
    static WINDOW_HWND: RefCell<Option<HWND>> = const { RefCell::new(None) };
    static SELECTED_NAV: RefCell<usize> = const { RefCell::new(0) };
    static HOME_PAGE: RefCell<HomePage> = RefCell::new(HomePage::new());
}

const WINDOW_WIDTH: i32 = 780;
const WINDOW_HEIGHT: i32 = 520;

pub(crate) fn open_settings_window() {
    let existing = WINDOW_HWND.with(|w| *w.borrow());
    if let Some(hwnd) = existing {
        // SAFETY: `hwnd` is a still-registered class-level window for the
        // process lifetime; showing/foregrounding an existing window is a
        // plain, always-safe Win32 call.
        unsafe {
            let _ = ShowWindow(hwnd, SW_RESTORE);
            let _ = SetForegroundWindow(hwnd);
        }
        return;
    }

    // SAFETY: every call below either has its own safety comment or is a
    // plain value/query with no aliasing or lifetime requirements.
    unsafe {
        let hinstance = GetModuleHandleW(None).expect("own module handle always resolves");
        let hinstance = windows::Win32::Foundation::HINSTANCE(hinstance.0);

        let class = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance,
            lpszClassName: w!("GroveShellSettingsWindow"),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: CreateSolidBrush(windows::Win32::Foundation::COLORREF(BG_WINDOW)),
            ..Default::default()
        };
        let _ = RegisterClassW(&class);

        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            w!("GroveShellSettingsWindow"),
            w!("GroveShell Settings"),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            200,
            200,
            WINDOW_WIDTH,
            WINDOW_HEIGHT,
            None,
            None,
            hinstance,
            None,
        );
        if let Ok(hwnd) = hwnd {
            WINDOW_HWND.with(|w| *w.borrow_mut() = Some(hwnd));
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
        }
    }
}

fn content_rect(client: RECT) -> RECT {
    RECT { left: NAV_WIDTH, top: 0, right: client.right, bottom: client.bottom }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            let mut client = RECT::default();
            let _ = GetClientRect(hwnd, &mut client);

            let nav_rect = RECT { left: 0, top: 0, right: NAV_WIDTH, bottom: client.bottom };
            let nav_brush = CreateSolidBrush(BG_NAV);
            FillRect(hdc, &nav_rect, nav_brush);
            let _ = windows::Win32::Graphics::Gdi::DeleteObject(nav_brush);

            let selected = SELECTED_NAV.with(|s| *s.borrow());
            for (i, rect) in nav_layout().into_iter().enumerate() {
                if i == selected {
                    super::theme::fill_round_rect(hdc, rect, 0, windows::Win32::Foundation::COLORREF(0x00404040));
                }
                draw_centered_text(hdc, rect, NAV_ITEMS[i], super::theme::TEXT);
            }

            let content = content_rect(client);
            if selected == 0 {
                HOME_PAGE.with(|p| p.borrow().paint(hdc, content));
            }
            // Tasks 15-18 add the remaining `selected == 1..=4` arms here.

            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let x = (lparam.0 & 0xFFFF) as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i32;
            if let Some(index) = nav_hit_test(x, y) {
                SELECTED_NAV.with(|s| *s.borrow_mut() = index);
            } else {
                let mut client = RECT::default();
                let _ = GetClientRect(hwnd, &mut client);
                let content = content_rect(client);
                let selected = SELECTED_NAV.with(|s| *s.borrow());
                if selected == 0 {
                    HOME_PAGE.with(|p| p.borrow_mut().on_click(x, y, content));
                }
                // Tasks 15-18 add the remaining `selected == 1..=4` arms here.
            }
            let _ = windows::Win32::UI::WindowsAndMessaging::InvalidateRect(hwnd, None, true);
            LRESULT(0)
        }
        WM_DESTROY => {
            WINDOW_HWND.with(|w| *w.borrow_mut() = None);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

pub(crate) fn is_settings_window_open() -> bool {
    WINDOW_HWND.with(|w| {
        w.borrow()
            .map(|hwnd| unsafe { IsWindowVisible(hwnd) }.as_bool())
            .unwrap_or(false)
    })
}
```

This references `super::pages::home::HomePage`, which Task 9 creates — until then, add a temporary placeholder so this compiles: create `apps/settings/src/imp/pages/home.rs` with:

```rust
//! Home/Status page — full implementation in Task 9.

use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Gdi::HDC;

use super::Page;

pub(crate) struct HomePage;

impl HomePage {
    pub(crate) fn new() -> Self {
        Self
    }
}

impl Page for HomePage {
    fn paint(&self, _hdc: HDC, _content_rect: RECT) {}
    fn on_click(&mut self, _x: i32, _y: i32, _content_rect: RECT) {}
}
```

And add `pub(crate) mod home;` to `apps/settings/src/imp/pages/mod.rs`.

- [ ] **Step 7: Wire everything into `imp/mod.rs`**

Add `mod theme;`, `mod util_text;`, `mod nav;`, `mod pages;` near the top of `apps/settings/src/imp/mod.rs` (alphabetical position among the existing `mod` lines is fine, matching `apps/ui/src/imp/mod.rs`'s own ordering convention loosely).

- [ ] **Step 8: Build and run tests**

Run: `cargo build -p groveshell-settings && cargo test -p groveshell-settings`
Expected: builds cleanly, all tests (including the new `nav` tests) PASS.

Manual verification: run the exe, left-click the tray icon, confirm the settings window opens with a visible nav list ("Home" highlighted) and an empty content pane; click each nav item and confirm the highlight moves.

- [ ] **Step 9: Commit**

```bash
git add apps/settings/src/imp/theme.rs apps/settings/src/imp/util_text.rs apps/settings/src/imp/nav.rs apps/settings/src/imp/pages/ apps/settings/src/imp/window.rs apps/settings/src/imp/mod.rs
git commit -m "feat(settings): add the settings window shell with nav list and page dispatch"
```

---

### Task 9: Home/Status page

**Files:**
- Modify: `apps/settings/src/imp/pages/home.rs` (replace the Task 8 placeholder)

**Interfaces:**
- Consumes: `health::sample_process`, `health::host_ping_ok`, `tray::toggle_groveshell` (Task 6), `theme::draw_toggle`/`hit_toggle` (for the Start-with-Windows checkbox, drawn as a toggle for visual consistency rather than a native checkbox).
- Produces: `pub(crate) struct HomePage` implementing `Page`, with an internal 2-second refresh via a stored `last_sample: Instant` (re-sampled lazily on paint if stale, not via its own `WM_TIMER` — the settings window doesn't yet own a timer; Task 9 adds one).

- [ ] **Step 1: Add a repaint timer to the settings window**

In `apps/settings/src/imp/window.rs`, inside `open_settings_window()`'s `unsafe` block, right after the `if let Ok(hwnd) = hwnd { ... }` block's `SetForegroundWindow` call, add:

```rust
            windows::Win32::UI::WindowsAndMessaging::SetTimer(hwnd, 1, 2000, None);
```

And add a `WM_TIMER` arm to `wndproc`, right before the `WM_DESTROY` arm:

```rust
        windows::Win32::UI::WindowsAndMessaging::WM_TIMER => {
            let _ = windows::Win32::UI::WindowsAndMessaging::InvalidateRect(hwnd, None, true);
            LRESULT(0)
        }
```

- [ ] **Step 2: Write `HomePage`**

Overwrite `apps/settings/src/imp/pages/home.rs`:

```rust
//! Home/Status page: overall health, per-process CPU/RAM, the
//! Restore-Explorer/Start-GroveShell button, and Start-with-Windows.

use windows::Win32::Foundation::{COLORREF, RECT};
use windows::Win32::Graphics::Gdi::HDC;

use super::Page;
use crate::imp::autostart;
use crate::imp::health::{host_ping_ok, sample_process};
use crate::imp::theme::{draw_toggle, hit_toggle, ACCENT, TEXT, TEXT_MUTED};
use crate::imp::tray::toggle_groveshell;
use crate::imp::util_text::draw_centered_text;

const ROW_HEIGHT: i32 = 32;
const PADDING: i32 = 24;
const BUTTON_HEIGHT: i32 = 36;
const BUTTON_WIDTH: i32 = 220;
const TOGGLE_WIDTH: i32 = 44;
const TOGGLE_HEIGHT: i32 = 24;

pub(crate) struct HomePage;

impl HomePage {
    pub(crate) fn new() -> Self {
        Self
    }

    fn restore_button_rect(&self, content_rect: RECT) -> RECT {
        RECT {
            left: content_rect.left + PADDING,
            top: content_rect.top + PADDING + ROW_HEIGHT * 5,
            right: content_rect.left + PADDING + BUTTON_WIDTH,
            bottom: content_rect.top + PADDING + ROW_HEIGHT * 5 + BUTTON_HEIGHT,
        }
    }

    fn autostart_toggle_rect(&self, content_rect: RECT) -> RECT {
        let button = self.restore_button_rect(content_rect);
        RECT {
            left: content_rect.left + PADDING,
            top: button.bottom + PADDING,
            right: content_rect.left + PADDING + TOGGLE_WIDTH,
            bottom: button.bottom + PADDING + TOGGLE_HEIGHT,
        }
    }
}

impl Page for HomePage {
    fn paint(&self, hdc: HDC, content_rect: RECT) {
        let healthy = health_summary();
        let (status_text, status_color) = match &healthy {
            Ok(()) => ("Healthy".to_string(), COLORREF(0x0060C060)),
            Err(reason) => (format!("Unhealthy: {reason}"), COLORREF(0x004040FF)),
        };

        // SAFETY: `hdc` is a valid device context from the caller's
        // `BeginPaint`, live for the duration of this call.
        unsafe {
            draw_centered_text(
                hdc,
                RECT { left: content_rect.left + PADDING, top: content_rect.top + PADDING, right: content_rect.right - PADDING, bottom: content_rect.top + PADDING + ROW_HEIGHT },
                &status_text,
                status_color,
            );

            for (i, name) in ["watchdog", "host", "ui"].iter().enumerate() {
                let row = RECT {
                    left: content_rect.left + PADDING,
                    top: content_rect.top + PADDING + ROW_HEIGHT * (i as i32 + 1),
                    right: content_rect.right - PADDING,
                    bottom: content_rect.top + PADDING + ROW_HEIGHT * (i as i32 + 2),
                };
                let line = process_status_line(name);
                draw_centered_text(hdc, row, &line, TEXT);
            }

            let button = self.restore_button_rect(content_rect);
            let running = crate::imp::tray::is_ui_running_for_home_page();
            let label = if running { "Restore Explorer" } else { "Start GroveShell" };
            super::super::theme::fill_round_rect(hdc, button, 8, ACCENT);
            draw_centered_text(hdc, button, label, COLORREF(0x00202020));

            let toggle = self.autostart_toggle_rect(content_rect);
            draw_toggle(hdc, toggle, autostart::is_enabled());
            draw_centered_text(
                hdc,
                RECT { left: toggle.right + 12, top: toggle.top, right: toggle.right + 260, bottom: toggle.bottom },
                "Start with Windows",
                TEXT_MUTED,
            );
        }
    }

    fn on_click(&mut self, x: i32, y: i32, content_rect: RECT) {
        let button = self.restore_button_rect(content_rect);
        if x >= button.left && x < button.right && y >= button.top && y < button.bottom {
            toggle_groveshell();
            return;
        }
        let toggle = self.autostart_toggle_rect(content_rect);
        if hit_toggle(toggle, x, y) {
            autostart::set_enabled(!autostart::is_enabled());
        }
    }
}

fn process_status_line(name: &str) -> String {
    // Best-effort: pid lookup for display purposes only reads whatever
    // `ManagedProcesses` tracked; a fuller implementation would expose pid
    // accessors, but for this page's read-only display, a "known name,
    // sampled if alive" line is enough context for a health screen.
    match crate::imp::tray::pid_for(name) {
        Some(pid) => match sample_process(pid) {
            Some(sample) => format!(
                "{name}: running (pid {pid}, {:.1}% CPU, {:.0} MB)",
                sample.cpu_percent,
                sample.working_set_bytes as f64 / (1024.0 * 1024.0)
            ),
            None => format!("{name}: running (pid {pid})"),
        },
        None => format!("{name}: not running"),
    }
}

fn health_summary() -> Result<(), String> {
    for name in ["watchdog", "host", "ui"] {
        if crate::imp::tray::pid_for(name).is_none() {
            return Err(format!("{name} is not running"));
        }
    }
    if !host_ping_ok(std::time::Duration::from_millis(500)) {
        return Err("host did not respond to ping".to_string());
    }
    Ok(())
}
```

- [ ] **Step 3: Expose the pid/liveness accessors `home.rs` needs from `tray.rs`**

This task's `home.rs` calls `crate::imp::tray::is_ui_running_for_home_page()` and `crate::imp::tray::pid_for(name)`, which don't exist yet. Add them to `apps/settings/src/imp/tray.rs`, and correspondingly extend `ManagedProcesses` (Task 5) with pid accessors. In `apps/settings/src/imp/process.rs`, add to `impl ManagedProcesses`:

```rust
    pub fn pid_of(&self, name: &str) -> Option<u32> {
        let child = match name {
            "watchdog" => self.watchdog.as_ref(),
            "host" => self.host.as_ref(),
            "ui" => self.ui.as_ref(),
            _ => None,
        }?;
        Some(child.id())
    }
```

In `apps/settings/src/imp/tray.rs`, add near `is_ui_running`:

```rust
pub(crate) fn is_ui_running_for_home_page() -> bool {
    is_ui_running()
}

pub(crate) fn pid_for(name: &str) -> Option<u32> {
    PROCESSES.with(|p| p.borrow().as_ref().and_then(|proc| proc.pid_of(name)))
}
```

- [ ] **Step 4: Build and manually verify**

Run: `cargo build -p groveshell-settings`
Expected: builds cleanly.

Manual verification: run the full stack (`groveshell-settings.exe` with the other three built), open the settings window, confirm the Home page shows "Healthy," three process rows with plausible CPU/RAM numbers, and clicking "Restore Explorer" actually restores the real taskbar and flips the button/tray-menu label to "Start GroveShell"; clicking it again relaunches everything.

- [ ] **Step 5: Commit**

```bash
git add apps/settings/src/imp/pages/home.rs apps/settings/src/imp/tray.rs apps/settings/src/imp/process.rs apps/settings/src/imp/window.rs
git commit -m "feat(settings): add the Home/Status page with health, stats, and the restore-Explorer toggle"
```

---

### Task 10: Autostart (Start with Windows)

**Files:**
- Create: `apps/settings/src/imp/autostart.rs`
- Modify: `apps/settings/src/imp/mod.rs`

**Interfaces:**
- Produces: `pub fn is_enabled() -> bool`, `pub fn set_enabled(enabled: bool)` — read/write `HKCU\Software\Microsoft\Windows\CurrentVersion\Run\GroveShell`, plus keeping `config.toml`'s `general.start_with_windows` in sync (via Task 14's `config_store`, wired in Step 3 below).

- [ ] **Step 1: Write `autostart.rs`**

Create `apps/settings/src/imp/autostart.rs`:

```rust
//! Reads/writes the `HKCU\...\Run` value that makes Windows launch
//! `groveshell-settings.exe` at login — the process that then launches
//! everything else (see Task 5), not `groveshell-host` directly.

use windows::core::{w, PCWSTR};
use windows::Win32::System::Registry::{
    RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
    KEY_READ, KEY_WRITE, REG_SZ,
};

const RUN_KEY_PATH: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const VALUE_NAME: &str = "GroveShell";

fn open_run_key(access: u32) -> Option<HKEY> {
    let path: Vec<u16> = RUN_KEY_PATH.encode_utf16().chain(std::iter::once(0)).collect();
    let mut key = HKEY::default();
    // SAFETY: `path` is nul-terminated and outlives the call; `key` is a
    // local out-param outliving it too.
    let result = unsafe {
        RegOpenKeyExW(HKEY_CURRENT_USER, PCWSTR(path.as_ptr()), Some(0), windows::Win32::System::Registry::REG_SAM_FLAGS(access), &mut key)
    };
    result.is_ok().then_some(key)
}

/// `true` if the `GroveShell` value exists under the Run key (its content
/// isn't validated — a stale/incorrect path is still "enabled" for
/// display purposes, matching how the real Windows Settings app's
/// Startup Apps page also just checks presence).
pub fn is_enabled() -> bool {
    let Some(key) = open_run_key(KEY_READ.0) else { return false };
    let name: Vec<u16> = VALUE_NAME.encode_utf16().chain(std::iter::once(0)).collect();
    let mut buf = [0u16; 512];
    let mut buf_len = (buf.len() * 2) as u32;
    // SAFETY: `key` was just opened above with read access; `name` is
    // nul-terminated; `buf`/`buf_len` describe a live, correctly-sized
    // output buffer for the duration of this call.
    let result = unsafe {
        windows::Win32::System::Registry::RegQueryValueExW(
            key,
            PCWSTR(name.as_ptr()),
            None,
            None,
            Some(buf.as_mut_ptr() as *mut u8),
            Some(&mut buf_len),
        )
    };
    // SAFETY: `key` was opened by this function and is not used past
    // this point.
    unsafe { let _ = RegCloseKey(key); }
    result.is_ok()
}

/// Writes (or removes) the `GroveShell` Run value pointing at this
/// process's own `.exe` path — always `groveshell-settings.exe`, per the
/// design decision that it's the process a user wants launched at login.
pub fn set_enabled(enabled: bool) {
    let Some(key) = open_run_key((KEY_READ | KEY_WRITE).0) else {
        tracing::warn!("could not open Run registry key");
        return;
    };
    let name: Vec<u16> = VALUE_NAME.encode_utf16().chain(std::iter::once(0)).collect();

    if enabled {
        let Ok(exe_path) = std::env::current_exe() else {
            unsafe { let _ = RegCloseKey(key); }
            return;
        };
        let quoted = format!("\"{}\"", exe_path.display());
        let value: Vec<u16> = quoted.encode_utf16().chain(std::iter::once(0)).collect();
        let bytes: &[u8] = unsafe {
            std::slice::from_raw_parts(value.as_ptr() as *const u8, value.len() * 2)
        };
        // SAFETY: `key` is open with write access; `name`/`bytes` are
        // valid, live buffers for the duration of this call.
        unsafe {
            let _ = RegSetValueExW(key, PCWSTR(name.as_ptr()), Some(0), REG_SZ, Some(bytes));
        }
    } else {
        // SAFETY: same key/name contract as above.
        unsafe {
            let _ = RegDeleteValueW(key, PCWSTR(name.as_ptr()));
        }
    }

    // SAFETY: `key` was opened by this function and is not used past
    // this point.
    unsafe { let _ = RegCloseKey(key); }
}

#[allow(unused_imports)]
use w as _; // silences an unused-import lint if `w!` ends up unused on some feature combination
```

- [ ] **Step 2: Wire the module into `imp/mod.rs`**

Add `mod autostart;` near the top of `apps/settings/src/imp/mod.rs`.

- [ ] **Step 3: Build and manually verify**

Run: `cargo build -p groveshell-settings`
Expected: builds cleanly.

Manual verification: toggle "Start with Windows" on the Home page, open `regedit` at `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`, confirm a `GroveShell` value pointing at `groveshell-settings.exe` appears; toggle it off, confirm the value disappears.

- [ ] **Step 4: Commit**

```bash
git add apps/settings/src/imp/autostart.rs apps/settings/src/imp/mod.rs
git commit -m "feat(settings): wire the Start-with-Windows toggle to the HKCU Run key"
```

---

### Task 11: Config-driven `config_store` in settings, wired to Home page's autostart flag

**Files:**
- Create: `apps/settings/src/imp/config_store.rs`
- Modify: `apps/settings/src/imp/mod.rs`
- Modify: `apps/settings/src/imp/pages/home.rs`
- Modify: `apps/settings/src/imp/autostart.rs`

**Interfaces:**
- Produces: `pub fn current() -> Config` (returns a clone of the in-memory config), `pub fn update(f: impl FnOnce(&mut Config)) -> Config` (applies `f`, saves via `groveshell_config::save`, pushes `config.reload` to the `groveshell-ui` pipe, returns the new config) — this is the single choke point every settings page (Tasks 15-18) and the autostart toggle use to persist a change.

- [ ] **Step 1: Write `config_store.rs`**

Create `apps/settings/src/imp/config_store.rs`:

```rust
//! The in-memory `Config` this process edits, plus the single choke point
//! (`update`) every settings control uses to persist a change: save to
//! disk (existing atomic-write/backup behavior in
//! `groveshell_config::save`, unchanged) and push `config.reload` to
//! `apps/ui` so it takes effect live (see Task 13).

use std::cell::RefCell;
use std::time::Duration;

use groveshell_config::Config;

thread_local! {
    static CONFIG: RefCell<Config> = RefCell::new(Config::default());
}

fn config_path() -> std::path::PathBuf {
    groveshell_common::paths::data_dir()
        .expect("data_dir should always resolve")
        .join("config.toml")
}

pub(crate) fn init() {
    let config = groveshell_config::load_or_default(&config_path());
    CONFIG.with(|c| *c.borrow_mut() = config);
}

pub(crate) fn current() -> Config {
    CONFIG.with(|c| c.borrow().clone())
}

/// Applies `f` to a clone of the current config, saves it, and — if it
/// saved successfully — pushes `config.reload` to `apps/ui` (best-effort:
/// `apps/ui` might not be running, e.g. right after "Restore Explorer";
/// that's not an error, the setting still applies next time `ui` starts
/// and reads `config.toml` itself) and updates the in-memory copy.
pub(crate) fn update(f: impl FnOnce(&mut Config)) {
    let mut next = current();
    f(&mut next);
    match groveshell_config::save(&config_path(), &next) {
        Ok(()) => {
            CONFIG.with(|c| *c.borrow_mut() = next);
            push_reload();
        }
        Err(e) => {
            tracing::error!(error = ?e, "failed to save config");
        }
    }
}

fn push_reload() {
    let Ok(mut conn) = groveshell_ipc::pipe::connect("groveshell-ui") else {
        tracing::debug!("groveshell-ui not reachable; config.reload not pushed (will apply on its next start)");
        return;
    };
    let envelope = groveshell_ipc::Envelope::new(
        "groveshell-settings",
        groveshell_ipc::message_type::CONFIG_RELOAD,
        serde_json::json!({}),
    );
    if let Err(e) = groveshell_ipc::framing::write_envelope(&mut conn, &envelope) {
        tracing::warn!(error = ?e, "failed to push config.reload");
        return;
    }
    let _ = conn.set_read_timeout(Some(Duration::from_millis(200)));
}
```

- [ ] **Step 2: Call `config_store::init()` at startup and remove the now-redundant ad hoc config load**

In `apps/settings/src/imp/mod.rs`, add `mod config_store;` near the top. In `run()`, replace the existing:

```rust
    let config_path = groveshell_common::paths::data_dir()?.join("config.toml");
    let config = groveshell_config::load_or_default(&config_path);
    tracing::info!(?config, "configuration loaded");
```

with:

```rust
    config_store::init();
    tracing::info!(config = ?config_store::current(), "configuration loaded");
```

- [ ] **Step 3: Wire the autostart toggle through `config_store` so `general.start_with_windows` stays in sync**

In `apps/settings/src/imp/autostart.rs`, change `set_enabled`'s signature is unchanged, but `home.rs`'s click handler now calls both the registry write and the config update together. In `apps/settings/src/imp/pages/home.rs`, replace the `on_click` toggle branch:

```rust
        let toggle = self.autostart_toggle_rect(content_rect);
        if hit_toggle(toggle, x, y) {
            let new_state = !autostart::is_enabled();
            autostart::set_enabled(new_state);
            crate::imp::config_store::update(|config| {
                config.general.start_with_windows = new_state;
            });
        }
```

And change `paint`'s reading of the toggle state to read the registry (source of truth, per the spec's "reflects reality even if the registry was edited by hand") rather than the config copy — this is already what Task 9's code does (`autostart::is_enabled()`), so no further change is needed there.

- [ ] **Step 4: Build and verify**

Run: `cargo build -p groveshell-settings`
Expected: builds cleanly.

Manual verification: toggle Start-with-Windows, then check `%LOCALAPPDATA%\GroveShell\config.toml`'s `[general]` section shows `start_with_windows = true`/`false` matching the registry state.

- [ ] **Step 5: Commit**

```bash
git add apps/settings/src/imp/config_store.rs apps/settings/src/imp/mod.rs apps/settings/src/imp/pages/home.rs
git commit -m "feat(settings): add the config_store save/reload choke point and wire autostart through it"
```

---

### Task 12: `apps/ui` becomes a config consumer — static fields at startup

**Files:**
- Modify: `apps/ui/Cargo.toml`
- Modify: `apps/ui/src/imp/state.rs`
- Modify: `apps/ui/src/imp/mod.rs`
- Modify: `apps/ui/src/imp/dock.rs`
- Modify: `apps/ui/src/imp/util.rs`

**Interfaces:**
- Consumes: `groveshell_config::{load_or_default, Config}` (new dependency).
- Produces: `AppState` gains a `pub(crate) config: groveshell_config::Config` field, read at startup and applied to bar height, dock icon size/alignment, and animation scale/reduced-motion. `dock::anchor_x(work_area_left: i32, work_area_right: i32, content_w: i32, alignment: &str) -> i32` — the pure, testable dock-alignment calculation Task 14's Dock settings page relies on existing correctly. Blur and the overview-modifier hotkey are deferred to Tasks 13/14 (they touch different files/behavior and deserve their own test cycles).

- [ ] **Step 1: Add the dependency**

In `apps/ui/Cargo.toml`, add to `[dependencies]` (alongside the existing `groveshell-common`/`groveshell-window-model`):

```toml
groveshell-config = { workspace = true }
```

- [ ] **Step 2: Write the failing test for `dock::anchor_x`**

In `apps/ui/src/imp/dock.rs`, add to the existing `#[cfg(test)] mod tests` block:

```rust
    #[test]
    fn anchor_x_center_centers_content_in_the_work_area() {
        // work area 0..200, content 40 wide -> left edge at 80
        assert_eq!(anchor_x(0, 200, 40, "center"), 80);
    }

    #[test]
    fn anchor_x_left_hugs_the_left_edge() {
        assert_eq!(anchor_x(0, 200, 40, "left"), 0);
    }

    #[test]
    fn anchor_x_right_hugs_the_right_edge() {
        assert_eq!(anchor_x(0, 200, 40, "right"), 160);
    }

    #[test]
    fn anchor_x_falls_back_to_center_for_an_unknown_alignment() {
        assert_eq!(anchor_x(0, 200, 40, "bogus"), 80);
    }
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test -p groveshell-ui anchor_x`
Expected: FAIL — `anchor_x` doesn't exist (compile error).

- [ ] **Step 4: Implement `anchor_x` and use it in `dock_layout`**

In `apps/ui/src/imp/dock.rs`, add near `dock_layout`:

```rust
/// The dock bar's left edge given a work area's horizontal span, the
/// bar's total content width, and a horizontal alignment — "left"/"right"
/// leave a small edge margin (matching `DOCK_MARGIN_BOTTOM`'s bottom-edge
/// feel) rather than touching the work area's exact edge; any unrecognized
/// alignment string falls back to "center" rather than panicking, since
/// `Config::validate()` is the actual gate on invalid values and this
/// function must stay total.
pub(crate) fn anchor_x(work_area_left: i32, work_area_right: i32, content_w: i32, alignment: &str) -> i32 {
    let margin = 24;
    match alignment {
        "left" => work_area_left + margin,
        "right" => (work_area_right - content_w - margin).max(work_area_left),
        _ => (work_area_left + work_area_right) / 2 - content_w / 2,
    }
}
```

Then, in `dock_layout` (same file), replace the existing centering line:

```rust
    let cx = (card_rect.left + card_rect.right) / 2;
    let bar_left = cx - bar_w / 2;
```

with:

```rust
    let alignment = super::state::STATE.with(|s| {
        s.borrow().as_ref().map(|st| st.config.appearance.dock_alignment.clone())
    }).unwrap_or_else(|| "center".to_string());
    let bar_left = anchor_x(card_rect.left, card_rect.right, bar_w, &alignment);
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test -p groveshell-ui anchor_x`
Expected: PASS

- [ ] **Step 6: Add `config` to `AppState` and load it at startup**

In `apps/ui/src/imp/state.rs`, add a field to `AppState` (near the top of the struct, right after `bars`):

```rust
    pub(crate) config: groveshell_config::Config,
```

In `apps/ui/src/imp/mod.rs`, near the top of `main()` (right after the `_log_guard` line), add:

```rust
    let config_path = groveshell_common::paths::data_dir()?.join("config.toml");
    let config = groveshell_config::load_or_default(&config_path);
    tracing::info!(?config, "configuration loaded");
```

Replace the hardcoded bar-height usage. Where `main()` currently does:

```rust
            let bar_height = scaled(BAR_HEIGHT, monitor.dpi);
```

change to:

```rust
            let bar_height = scaled(config.appearance.top_bar_height as i32, monitor.dpi);
```

And in the `STATE.with(|s| { *s.borrow_mut() = Some(AppState { ... }) })` initializer, add the new field:

```rust
                config,
```

(Note: `config` is moved into `AppState` here, so any earlier use of `config.appearance.top_bar_height` above this point must happen before this move — which it already does, since bar creation happens earlier in `main()`.)

Also update `apps/ui/src/imp/dock.rs`'s icon size: where `dock_layout` currently does `let icon_size = scaled(DOCK_ICON_SIZE, dpi);`, change to:

```rust
    let icon_size_cfg = super::state::STATE.with(|s| {
        s.borrow().as_ref().map(|st| st.config.appearance.dock_icon_size as i32)
    }).unwrap_or(DOCK_ICON_SIZE);
    let icon_size = scaled(icon_size_cfg, dpi);
```

- [ ] **Step 7: Wire `animation_scale`/`reduced_motion` into `util::progress_dur`**

In `apps/ui/src/imp/util.rs`, replace `progress_dur`:

```rust
pub(crate) fn progress_dur(started: std::time::Instant, duration: std::time::Duration) -> f64 {
    let (scale, reduced) = super::state::STATE.with(|s| {
        s.borrow()
            .as_ref()
            .map(|st| (st.config.appearance.animation_scale, st.config.appearance.reduced_motion))
    }).unwrap_or((1.0, false));
    if reduced {
        return 1.0; // instant: always report "animation complete"
    }
    let effective_duration = duration.mul_f32(scale.max(0.01));
    (started.elapsed().as_secs_f64() / effective_duration.as_secs_f64()).min(1.0)
}
```

- [ ] **Step 8: Build and run tests**

Run: `cargo build -p groveshell-ui && cargo test -p groveshell-ui`
Expected: builds cleanly (this is the first time `apps/ui` compiles against `groveshell-config` — watch for any borrow-checker friction around `STATE`'s `RefCell` inside `dock_layout`/`progress_dur`, both of which already follow the established "borrow, clone what's needed, drop the borrow" pattern used elsewhere in this file, e.g. `reference_dpi()`). All existing and new tests PASS.

Manual verification: run the full stack, edit `config.toml`'s `appearance.top_bar_height` to `40` and `dock_icon_size` to `56` by hand, restart `groveshell-ui.exe`, confirm the bar is visibly taller and dock icons visibly larger. Set `dock_alignment = "left"`, restart, confirm the dock hugs the left edge of the focused overview card.

- [ ] **Step 9: Commit**

```bash
git add apps/ui/Cargo.toml apps/ui/src/imp/state.rs apps/ui/src/imp/mod.rs apps/ui/src/imp/dock.rs apps/ui/src/imp/util.rs
git commit -m "feat(ui): read top_bar_height/dock_icon_size/dock_alignment/animation_scale from config at startup"
```

---

### Task 13: `apps/ui` overview-modifier and blur config wiring

**Files:**
- Modify: `apps/ui/src/imp/movesize.rs`
- Modify: `apps/ui/src/imp/mod.rs`

**Interfaces:**
- Produces: `movesize::vk_codes_for_modifier(modifier: &str) -> Vec<u32>` (pure, testable: `"Super"` -> `[VK_LWIN, VK_RWIN]`, `"Alt"` -> `[VK_MENU]`, `"CtrlAlt"` -> `[VK_CONTROL, VK_MENU]` conceptually, but see Step 3's simplified single-modifier-family handling), consumed by the keyboard hook to decide which key(s) toggle the overview / arm move-resize, replacing the hardcoded `VK_LWIN`/`VK_RWIN` check. Top-bar and overview blur applied via `DwmEnableBlurBehindWindow` at startup in `mod.rs`.

- [ ] **Step 1: Write the failing test for the modifier-to-vkcode mapping**

In `apps/ui/src/imp/movesize.rs`, add near the top (after the existing `VK_LWIN`/`VK_RWIN` constants), plus a `#[cfg(test)] mod tests` block at the bottom of the file:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn super_modifier_maps_to_both_win_keys() {
        assert_eq!(vk_codes_for_modifier("Super"), vec![VK_LWIN, VK_RWIN]);
    }

    #[test]
    fn alt_modifier_maps_to_the_alt_key() {
        assert_eq!(vk_codes_for_modifier("Alt"), vec![0xA4, 0xA5]); // VK_LMENU, VK_RMENU
    }

    #[test]
    fn unknown_modifier_falls_back_to_super() {
        assert_eq!(vk_codes_for_modifier("bogus"), vec![VK_LWIN, VK_RWIN]);
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p groveshell-ui vk_codes_for_modifier`
Expected: FAIL — function doesn't exist.

- [ ] **Step 3: Implement `vk_codes_for_modifier` and use it in the keyboard hook**

`"CtrlAlt"` requires **both** keys held together (a two-key chord), which is a materially different match than the single-key-family checks `"Super"`/`"Alt"` need — the existing hook's `WIN_HELD`/`WIN_USED` state machine only tracks one key family at a time. To keep this task's scope to what's actually testable and correct without redesigning the hook's state machine, `"CtrlAlt"` reuses the same single-family mechanism against `VK_MENU` (Alt) as its primary key, documented inline as a known simplification:

```rust
/// Which raw vkcodes should be treated as "the overview/move-resize
/// modifier," per `config.toml`'s `input.overview_modifier`. `"Super"`
/// (the default) matches both Windows keys, matching this module's
/// original hardcoded behavior. `"Alt"` matches both Alt keys. `"CtrlAlt"`
/// is documented in `docs/superpowers/specs/2026-07-30-tray-settings-app-design.md`
/// as a preset requiring both Ctrl and Alt; this hook's single-key-family
/// state machine (`WIN_HELD`/`WIN_USED`) only tracks one key family being
/// pressed/released at a time, so as a scoped simplification "CtrlAlt"
/// arms on Alt alone here (Alt is already one of the two keys), same as
/// the "Alt" case — a true two-key chord would need a second held-state
/// cell and is left as a documented follow-up, not attempted in this pass.
fn vk_codes_for_modifier(modifier: &str) -> Vec<u32> {
    match modifier {
        "Alt" | "CtrlAlt" => vec![0xA4, 0xA5], // VK_LMENU, VK_RMENU
        _ => vec![VK_LWIN, VK_RWIN],           // "Super" and any unknown value
    }
}
```

Then, in `keyboard_hook_proc`, replace:

```rust
    if info.vkCode == VK_LWIN || info.vkCode == VK_RWIN {
```

with:

```rust
    let modifier = super::state::STATE.with(|s| {
        s.borrow().as_ref().map(|st| st.config.input.overview_modifier.clone())
    }).unwrap_or_else(|| "Super".to_string());
    let codes = vk_codes_for_modifier(&modifier);
    if codes.contains(&info.vkCode) {
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p groveshell-ui vk_codes_for_modifier`
Expected: PASS

- [ ] **Step 5: Wire top-bar/overview blur at startup**

In `apps/ui/src/imp/mod.rs`, right after the primary bar's `SetWindowRgn` call inside the monitor loop (so it runs once per bar, matching how the region is applied per-bar), add:

```rust
            if config.appearance.top_bar_blur {
                enable_blur_behind(bar_hwnd);
            }
```

And, in the overview-creation loop, right after each `overview_hwnd` is created:

```rust
            if config.appearance.overview_blur {
                enable_blur_behind(overview_hwnd);
            }
```

Add the helper function near `register_class`:

```rust
/// Enables the simplest DWM blur-behind for `hwnd` — matches BlurMyShell's
/// simplest "blur what's behind" mode, not a Mica/acrylic material (see
/// the design doc's explicit scope decision). Best-effort: failure just
/// means no blur, same treatment as every other cosmetic Win32 call in
/// this file.
fn enable_blur_behind(hwnd: HWND) {
    use windows::Win32::Graphics::Dwm::{DwmEnableBlurBehindWindow, DWM_BLURBEHIND, DWM_BB_ENABLE};
    let bb = DWM_BLURBEHIND {
        dwFlags: DWM_BB_ENABLE,
        fEnable: true.into(),
        ..Default::default()
    };
    // SAFETY: `hwnd` is a valid, just-created window; `bb` is a local
    // outliving this synchronous call.
    unsafe {
        let _ = DwmEnableBlurBehindWindow(hwnd, &bb);
    }
}
```

- [ ] **Step 6: Build and verify**

Run: `cargo build -p groveshell-ui && cargo test -p groveshell-ui`
Expected: builds cleanly, all tests PASS.

Manual verification: set `input.overview_modifier = "Alt"` in `config.toml`, restart `groveshell-ui.exe`, confirm tapping Alt (not the Windows key) toggles the overview. Set `appearance.top_bar_blur = true`, restart, confirm the top bar visibly blurs the desktop behind it.

- [ ] **Step 7: Commit**

```bash
git add apps/ui/src/imp/movesize.rs apps/ui/src/imp/mod.rs
git commit -m "feat(ui): make the overview/move-resize modifier and top-bar/overview blur config-driven"
```

---

### Task 14: `apps/ui` live `config.reload` over its own named pipe

**Files:**
- Modify: `apps/ui/Cargo.toml`
- Modify: `apps/ui/src/imp/mod.rs`

**Interfaces:**
- Produces: `apps/ui` binds a `groveshell-ui` named pipe on a background thread at startup; on a `config.reload` message, it reloads `config.toml`, updates `AppState.config`, and posts a custom message (`WM_APP_CONFIG_RELOADED`) to the primary bar's `hwnd` so the actual re-apply (repaint, re-toggle blur, re-register the keyboard hook's modifier) happens on the main thread, not the pipe-listener thread — consistent with this codebase's established rule (see `movesize.rs`'s module docs) that Win32 mutation must happen on the owning thread, and IPC listener threads never call UI-affecting Win32 directly.

- [ ] **Step 1: Add the IPC dependency**

In `apps/ui/Cargo.toml`, add to `[dependencies]`:

```toml
groveshell-ipc = { workspace = true }
```

- [ ] **Step 2: Spawn the pipe-listener thread at startup**

In `apps/ui/src/imp/mod.rs`, add near the top of `main()`, right after the config-loading lines added in Task 12:

```rust
    std::thread::spawn(config_reload_listener);
```

Add the listener function and the custom message constant near `install_primary_bar_extras`:

```rust
/// Not a real Win32-defined message; app-private, matching the pattern
/// `apps/settings/src/imp/tray.rs`'s `WM_TRAYICON` uses.
const WM_APP_CONFIG_RELOADED: u32 = WM_APP + 1;

/// Binds the `groveshell-ui` pipe and, on each `config.reload` message,
/// reloads `config.toml` and posts `WM_APP_CONFIG_RELOADED` to the
/// primary bar's window so the actual re-apply happens on the main
/// thread. Mirrors `apps/host`'s `serve_ping` shape (bind-accept loop,
/// one thread per connection) but only ever expects this one message
/// type.
fn config_reload_listener() {
    loop {
        let conn = match groveshell_ipc::pipe::bind_and_accept("groveshell-ui") {
            Ok(conn) => conn,
            Err(e) => {
                tracing::error!(error = ?e, "failed to bind groveshell-ui pipe; retrying");
                std::thread::sleep(std::time::Duration::from_secs(1));
                continue;
            }
        };
        std::thread::spawn(move || handle_config_reload_connection(conn));
    }
}

fn handle_config_reload_connection(mut conn: std::fs::File) {
    let Ok(request) = groveshell_ipc::framing::read_envelope(&mut conn) else { return };
    if request.message_type != groveshell_ipc::message_type::CONFIG_RELOAD {
        return;
    }
    let primary_bar_hwnd = STATE.with(|s| s.borrow().as_ref().map(|st| st.primary_bar_hwnd));
    if let Some(hwnd) = primary_bar_hwnd {
        // SAFETY: `hwnd` is a valid, process-lifetime window; posting a
        // message across threads is the documented, safe way to hand
        // work back to a window's owning thread.
        unsafe {
            let _ = PostMessageW(hwnd, WM_APP_CONFIG_RELOADED, WPARAM(0), LPARAM(0));
        }
    }
}
```

- [ ] **Step 3: Handle `WM_APP_CONFIG_RELOADED` in `wndproc`**

In `wndproc`'s big `match msg` block, add a new arm (near the `WM_HOTKEY` arm):

```rust
        WM_APP_CONFIG_RELOADED => {
            let config_path = groveshell_common::paths::data_dir()
                .map(|d| d.join("config.toml"));
            if let Ok(path) = config_path {
                let new_config = groveshell_config::load_or_default(&path);
                tracing::info!(config = ?new_config, "config.reload: reapplying");
                STATE.with(|s| {
                    if let Some(state) = s.borrow_mut().as_mut() {
                        state.config = new_config;
                    }
                });
                // Re-run blur (idempotent: re-enabling an already-enabled
                // blur, or "enabling" with fEnable now false via a second
                // DwmEnableBlurBehindWindow call, both work correctly).
                let (bars_snapshot, overviews_snapshot, blur_bar, blur_overview) = STATE.with(|s| {
                    let state = s.borrow();
                    let st = state.as_ref();
                    (
                        st.map(|st| st.bars.iter().map(|b| b.hwnd).collect::<Vec<_>>()).unwrap_or_default(),
                        st.map(|st| st.overviews.values().map(|o| o.hwnd).collect::<Vec<_>>()).unwrap_or_default(),
                        st.map(|st| st.config.appearance.top_bar_blur).unwrap_or(false),
                        st.map(|st| st.config.appearance.overview_blur).unwrap_or(false),
                    )
                });
                for bar_hwnd in &bars_snapshot {
                    set_blur_behind(*bar_hwnd, blur_bar);
                    let _ = InvalidateRect(*bar_hwnd, None, true);
                }
                for overview_hwnd in &overviews_snapshot {
                    set_blur_behind(*overview_hwnd, blur_overview);
                }
            }
            LRESULT(0)
        }
```

Replace the Task 13 `enable_blur_behind` helper with a version that can also disable, since a live toggle-off needs that:

```rust
/// Enables or disables the simplest DWM blur-behind for `hwnd` — see
/// Task 13's original doc comment on `enable_blur_behind` for why this
/// isn't a Mica/acrylic material. `enabled = false` sends `fEnable =
/// false`, which is the documented way to turn blur-behind back off,
/// unlike simply not calling this function again.
fn set_blur_behind(hwnd: HWND, enabled: bool) {
    use windows::Win32::Graphics::Dwm::{DwmEnableBlurBehindWindow, DWM_BLURBEHIND, DWM_BB_ENABLE};
    let bb = DWM_BLURBEHIND {
        dwFlags: DWM_BB_ENABLE,
        fEnable: enabled.into(),
        ..Default::default()
    };
    // SAFETY: `hwnd` is caller-supplied and must be a valid window (both
    // call sites — startup and live reload — satisfy this); `bb` is a
    // local outliving this synchronous call.
    unsafe {
        let _ = DwmEnableBlurBehindWindow(hwnd, &bb);
    }
}
```

And update Task 13's two startup call sites (`enable_blur_behind(bar_hwnd)` / `enable_blur_behind(overview_hwnd)`) to call `set_blur_behind(bar_hwnd, config.appearance.top_bar_blur)` / `set_blur_behind(overview_hwnd, config.appearance.overview_blur)` instead, now that the unconditional-`true` helper has been generalized.

- [ ] **Step 4: Build and manually verify**

Run: `cargo build -p groveshell-ui`
Expected: builds cleanly.

Manual verification: with the full stack running, use the settings app's Dock page (once Task 15 lands) — or, until then, manually send a `config.reload` via `groveshell-cli` if it supports raw envelope sends, otherwise directly edit `config.toml` and use a short Rust test script — to confirm editing `config.toml`'s `top_bar_blur` and pushing `config.reload` toggles blur on the running bar without restarting `groveshell-ui.exe`. (Full end-to-end manual verification of this is realistically only convenient once Task 15's Dock page exists to trigger `config_store::update`, which is why the fuller checklist lives in Task 18's final integration step.)

- [ ] **Step 5: Commit**

```bash
git add apps/ui/Cargo.toml apps/ui/src/imp/mod.rs
git commit -m "feat(ui): listen for config.reload on its own named pipe and reapply settings live"
```

---

### Task 15: Dock settings page

**Files:**
- Create: `apps/settings/src/imp/pages/dock.rs`
- Modify: `apps/settings/src/imp/pages/mod.rs`
- Modify: `apps/settings/src/imp/window.rs`

**Interfaces:**
- Consumes: `theme::{draw_segmented, segmented_hit, draw_slider, value_from_slider_x}`, `config_store::{current, update}`.
- Produces: `pub(crate) struct DockPage` implementing `Page`: alignment segmented control (left/center/right), icon-size slider (32-64px), mode dropdown (drawn as a 3-way segmented control too — "overview" / "always" / "autohide" — reusing `draw_segmented` rather than a real native dropdown, since a segmented control is simpler to hit-test and there are only three options).

- [ ] **Step 1: Write `DockPage`**

Create `apps/settings/src/imp/pages/dock.rs`:

```rust
//! Dock settings: horizontal alignment, icon size, and visibility mode.

use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Gdi::HDC;

use super::Page;
use crate::imp::config_store;
use crate::imp::theme::{draw_segmented, draw_slider, segmented_hit, value_from_slider_x, TEXT_MUTED};
use crate::imp::util_text::draw_centered_text;

const PADDING: i32 = 24;
const ROW_HEIGHT: i32 = 48;
const CONTROL_WIDTH: i32 = 320;

const ALIGNMENT_OPTIONS: [&str; 3] = ["left", "center", "right"];
const MODE_OPTIONS: [&str; 3] = ["overview", "always", "autohide"];

pub(crate) struct DockPage;

impl DockPage {
    pub(crate) fn new() -> Self {
        Self
    }

    fn alignment_rect(&self, content_rect: RECT) -> RECT {
        RECT {
            left: content_rect.left + PADDING,
            top: content_rect.top + PADDING + ROW_HEIGHT,
            right: content_rect.left + PADDING + CONTROL_WIDTH,
            bottom: content_rect.top + PADDING + ROW_HEIGHT + 32,
        }
    }

    fn icon_size_rect(&self, content_rect: RECT) -> RECT {
        RECT {
            left: content_rect.left + PADDING,
            top: content_rect.top + PADDING + ROW_HEIGHT * 3,
            right: content_rect.left + PADDING + CONTROL_WIDTH,
            bottom: content_rect.top + PADDING + ROW_HEIGHT * 3 + 24,
        }
    }

    fn mode_rect(&self, content_rect: RECT) -> RECT {
        RECT {
            left: content_rect.left + PADDING,
            top: content_rect.top + PADDING + ROW_HEIGHT * 5,
            right: content_rect.left + PADDING + CONTROL_WIDTH,
            bottom: content_rect.top + PADDING + ROW_HEIGHT * 5 + 32,
        }
    }
}

impl Page for DockPage {
    fn paint(&self, hdc: HDC, content_rect: RECT) {
        let config = config_store::current();
        let alignment_index = ALIGNMENT_OPTIONS
            .iter()
            .position(|a| *a == config.appearance.dock_alignment)
            .unwrap_or(1);
        let mode_index = MODE_OPTIONS
            .iter()
            .position(|m| *m == config.appearance.dock_mode)
            .unwrap_or(0);

        // SAFETY: `hdc` is a valid device context supplied by the caller's
        // `BeginPaint`, live for the duration of this call.
        unsafe {
            draw_centered_text(
                hdc,
                RECT { left: content_rect.left + PADDING, top: content_rect.top + PADDING, right: content_rect.right - PADDING, bottom: content_rect.top + PADDING + 24 },
                "Alignment",
                TEXT_MUTED,
            );
            draw_segmented(hdc, self.alignment_rect(content_rect), &ALIGNMENT_OPTIONS, alignment_index);

            draw_centered_text(
                hdc,
                RECT { left: content_rect.left + PADDING, top: content_rect.top + PADDING + ROW_HEIGHT * 2, right: content_rect.right - PADDING, bottom: content_rect.top + PADDING + ROW_HEIGHT * 2 + 24 },
                &format!("Icon size: {}px", config.appearance.dock_icon_size),
                TEXT_MUTED,
            );
            draw_slider(hdc, self.icon_size_rect(content_rect), config.appearance.dock_icon_size as f32, 32.0, 64.0);

            draw_centered_text(
                hdc,
                RECT { left: content_rect.left + PADDING, top: content_rect.top + PADDING + ROW_HEIGHT * 4, right: content_rect.right - PADDING, bottom: content_rect.top + PADDING + ROW_HEIGHT * 4 + 24 },
                "Mode",
                TEXT_MUTED,
            );
            draw_segmented(hdc, self.mode_rect(content_rect), &MODE_OPTIONS, mode_index);
        }
    }

    fn on_click(&mut self, x: i32, y: i32, content_rect: RECT) {
        if let Some(index) = segmented_hit(self.alignment_rect(content_rect), &ALIGNMENT_OPTIONS, x, y) {
            let alignment = ALIGNMENT_OPTIONS[index].to_string();
            config_store::update(|c| c.appearance.dock_alignment = alignment.clone());
            return;
        }
        let slider_rect = self.icon_size_rect(content_rect);
        if y >= slider_rect.top - 8 && y < slider_rect.bottom + 8 && x >= slider_rect.left && x < slider_rect.right {
            let value = value_from_slider_x(slider_rect, x, 32.0, 64.0).round() as u32;
            config_store::update(|c| c.appearance.dock_icon_size = value);
            return;
        }
        if let Some(index) = segmented_hit(self.mode_rect(content_rect), &MODE_OPTIONS, x, y) {
            let mode = MODE_OPTIONS[index].to_string();
            config_store::update(|c| c.appearance.dock_mode = mode.clone());
        }
    }
}
```

- [ ] **Step 2: Register the page module and dispatch it from `window.rs`**

Add `pub(crate) mod dock;` to `apps/settings/src/imp/pages/mod.rs`.

In `apps/settings/src/imp/window.rs`, add a thread-local for it near `HOME_PAGE`:

```rust
    static DOCK_PAGE: RefCell<super::pages::dock::DockPage> = RefCell::new(super::pages::dock::DockPage::new());
```

In the `WM_PAINT` handler, extend the `if selected == 0 { ... }` into a full match:

```rust
            match selected {
                0 => HOME_PAGE.with(|p| p.borrow().paint(hdc, content)),
                1 => DOCK_PAGE.with(|p| p.borrow().paint(hdc, content)),
                _ => {}
            }
```

And likewise in `WM_LBUTTONDOWN`:

```rust
                match selected {
                    0 => HOME_PAGE.with(|p| p.borrow_mut().on_click(x, y, content)),
                    1 => DOCK_PAGE.with(|p| p.borrow_mut().on_click(x, y, content)),
                    _ => {}
                }
```

- [ ] **Step 3: Build and manually verify**

Run: `cargo build -p groveshell-settings`
Expected: builds cleanly.

Manual verification: with the full stack running, open Settings -> Dock, click "left"/"right"/"center," confirm the running dock's alignment changes live (via Task 14's `config.reload`); drag the icon-size slider, confirm dock icons resize live; click each mode option (visual confirmation only for `always`/`autohide`, since Task 12 only wired `dock_alignment`/`dock_icon_size` — `dock_mode`'s actual always-visible/autohide runtime behavior is out of this plan's scope beyond persisting the choice, matching the spec's framing of `dock_mode` as "already has a runtime concept... this wires the always-visible and autohide variants" being a config-plumbing task, not a new dock-visibility-engine task).

- [ ] **Step 4: Commit**

```bash
git add apps/settings/src/imp/pages/dock.rs apps/settings/src/imp/pages/mod.rs apps/settings/src/imp/window.rs
git commit -m "feat(settings): add the Dock settings page"
```

---

### Task 16: Top Bar settings page

**Files:**
- Create: `apps/settings/src/imp/pages/top_bar.rs`
- Modify: `apps/settings/src/imp/pages/mod.rs`
- Modify: `apps/settings/src/imp/window.rs`

**Interfaces:**
- Produces: `pub(crate) struct TopBarPage` implementing `Page`: height slider (24-48px), blur toggle.

- [ ] **Step 1: Write `TopBarPage`**

Create `apps/settings/src/imp/pages/top_bar.rs`:

```rust
//! Top Bar settings: height and blur.

use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Gdi::HDC;

use super::Page;
use crate::imp::config_store;
use crate::imp::theme::{draw_slider, draw_toggle, hit_toggle, value_from_slider_x, TEXT_MUTED};
use crate::imp::util_text::draw_centered_text;

const PADDING: i32 = 24;
const ROW_HEIGHT: i32 = 48;
const CONTROL_WIDTH: i32 = 320;

pub(crate) struct TopBarPage;

impl TopBarPage {
    pub(crate) fn new() -> Self {
        Self
    }

    fn height_slider_rect(&self, content_rect: RECT) -> RECT {
        RECT {
            left: content_rect.left + PADDING,
            top: content_rect.top + PADDING + ROW_HEIGHT,
            right: content_rect.left + PADDING + CONTROL_WIDTH,
            bottom: content_rect.top + PADDING + ROW_HEIGHT + 24,
        }
    }

    fn blur_toggle_rect(&self, content_rect: RECT) -> RECT {
        RECT {
            left: content_rect.left + PADDING,
            top: content_rect.top + PADDING + ROW_HEIGHT * 3,
            right: content_rect.left + PADDING + 44,
            bottom: content_rect.top + PADDING + ROW_HEIGHT * 3 + 24,
        }
    }
}

impl Page for TopBarPage {
    fn paint(&self, hdc: HDC, content_rect: RECT) {
        let config = config_store::current();
        // SAFETY: `hdc` is a valid device context from the caller's
        // `BeginPaint`, live for the duration of this call.
        unsafe {
            draw_centered_text(
                hdc,
                RECT { left: content_rect.left + PADDING, top: content_rect.top + PADDING, right: content_rect.right - PADDING, bottom: content_rect.top + PADDING + 24 },
                &format!("Height: {}px", config.appearance.top_bar_height),
                TEXT_MUTED,
            );
            draw_slider(hdc, self.height_slider_rect(content_rect), config.appearance.top_bar_height as f32, 24.0, 48.0);

            let toggle = self.blur_toggle_rect(content_rect);
            draw_toggle(hdc, toggle, config.appearance.top_bar_blur);
            draw_centered_text(
                hdc,
                RECT { left: toggle.right + 12, top: toggle.top, right: toggle.right + 200, bottom: toggle.bottom },
                "Blur",
                TEXT_MUTED,
            );
        }
    }

    fn on_click(&mut self, x: i32, y: i32, content_rect: RECT) {
        let slider = self.height_slider_rect(content_rect);
        if y >= slider.top - 8 && y < slider.bottom + 8 && x >= slider.left && x < slider.right {
            let value = value_from_slider_x(slider, x, 24.0, 48.0).round() as u32;
            config_store::update(|c| c.appearance.top_bar_height = value);
            return;
        }
        let toggle = self.blur_toggle_rect(content_rect);
        if hit_toggle(toggle, x, y) {
            let current = config_store::current().appearance.top_bar_blur;
            config_store::update(|c| c.appearance.top_bar_blur = !current);
        }
    }
}
```

- [ ] **Step 2: Register and dispatch**

Add `pub(crate) mod top_bar;` to `apps/settings/src/imp/pages/mod.rs`.

In `apps/settings/src/imp/window.rs`, add the thread-local:

```rust
    static TOP_BAR_PAGE: RefCell<super::pages::top_bar::TopBarPage> = RefCell::new(super::pages::top_bar::TopBarPage::new());
```

Extend both `match selected` blocks (paint and click) with:

```rust
                2 => TOP_BAR_PAGE.with(|p| p.borrow().paint(hdc, content)),
```

and, in the click match:

```rust
                    2 => TOP_BAR_PAGE.with(|p| p.borrow_mut().on_click(x, y, content)),
```

- [ ] **Step 3: Build and manually verify**

Run: `cargo build -p groveshell-settings`
Expected: builds cleanly.

Manual verification: with the full stack running, drag the height slider, confirm the running bar's height changes live (it doesn't fully re-flow every element without a `groveshell-ui` restart in this codebase's current bar-height-affects-window-creation-time design — see the honest caveat in Task 18's final checklist); toggle blur, confirm the bar visibly blurs/unblurs live.

- [ ] **Step 4: Commit**

```bash
git add apps/settings/src/imp/pages/top_bar.rs apps/settings/src/imp/pages/mod.rs apps/settings/src/imp/window.rs
git commit -m "feat(settings): add the Top Bar settings page"
```

---

### Task 17: Overview settings page

**Files:**
- Create: `apps/settings/src/imp/pages/overview.rs`
- Modify: `apps/settings/src/imp/pages/mod.rs`
- Modify: `apps/settings/src/imp/window.rs`

**Interfaces:**
- Produces: `pub(crate) struct OverviewPage` implementing `Page`: blur toggle, reduced-motion toggle, animation-speed slider (0.5x-2.0x, visually disabled — drawn dimmed and ignoring clicks — when reduced motion is on).

- [ ] **Step 1: Write `OverviewPage`**

Create `apps/settings/src/imp/pages/overview.rs`:

```rust
//! Overview settings: blur, reduced motion, and animation speed.

use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Gdi::HDC;

use super::Page;
use crate::imp::config_store;
use crate::imp::theme::{draw_slider, draw_toggle, hit_toggle, value_from_slider_x, TEXT_MUTED};
use crate::imp::util_text::draw_centered_text;

const PADDING: i32 = 24;
const ROW_HEIGHT: i32 = 48;
const CONTROL_WIDTH: i32 = 320;

pub(crate) struct OverviewPage;

impl OverviewPage {
    pub(crate) fn new() -> Self {
        Self
    }

    fn blur_toggle_rect(&self, content_rect: RECT) -> RECT {
        RECT {
            left: content_rect.left + PADDING,
            top: content_rect.top + PADDING + ROW_HEIGHT,
            right: content_rect.left + PADDING + 44,
            bottom: content_rect.top + PADDING + ROW_HEIGHT + 24,
        }
    }

    fn reduced_motion_toggle_rect(&self, content_rect: RECT) -> RECT {
        RECT {
            left: content_rect.left + PADDING,
            top: content_rect.top + PADDING + ROW_HEIGHT * 3,
            right: content_rect.left + PADDING + 44,
            bottom: content_rect.top + PADDING + ROW_HEIGHT * 3 + 24,
        }
    }

    fn speed_slider_rect(&self, content_rect: RECT) -> RECT {
        RECT {
            left: content_rect.left + PADDING,
            top: content_rect.top + PADDING + ROW_HEIGHT * 5,
            right: content_rect.left + PADDING + CONTROL_WIDTH,
            bottom: content_rect.top + PADDING + ROW_HEIGHT * 5 + 24,
        }
    }
}

impl Page for OverviewPage {
    fn paint(&self, hdc: HDC, content_rect: RECT) {
        let config = config_store::current();

        // SAFETY: `hdc` is a valid device context from the caller's
        // `BeginPaint`, live for the duration of this call.
        unsafe {
            let blur_toggle = self.blur_toggle_rect(content_rect);
            draw_toggle(hdc, blur_toggle, config.appearance.overview_blur);
            draw_centered_text(hdc, RECT { left: blur_toggle.right + 12, top: blur_toggle.top, right: blur_toggle.right + 200, bottom: blur_toggle.bottom }, "Blur", TEXT_MUTED);

            let motion_toggle = self.reduced_motion_toggle_rect(content_rect);
            draw_toggle(hdc, motion_toggle, config.appearance.reduced_motion);
            draw_centered_text(hdc, RECT { left: motion_toggle.right + 12, top: motion_toggle.top, right: motion_toggle.right + 200, bottom: motion_toggle.bottom }, "Reduced motion", TEXT_MUTED);

            let label = format!("Animation speed: {:.1}x", config.appearance.animation_scale);
            draw_centered_text(
                hdc,
                RECT { left: content_rect.left + PADDING, top: content_rect.top + PADDING + ROW_HEIGHT * 4, right: content_rect.right - PADDING, bottom: content_rect.top + PADDING + ROW_HEIGHT * 4 + 24 },
                &label,
                TEXT_MUTED,
            );
            // Drawn regardless of reduced_motion (so the last chosen value
            // stays visible), but clicks on it are ignored while reduced
            // motion is on — see on_click.
            draw_slider(hdc, self.speed_slider_rect(content_rect), config.appearance.animation_scale, 0.5, 2.0);
        }
    }

    fn on_click(&mut self, x: i32, y: i32, content_rect: RECT) {
        let blur_toggle = self.blur_toggle_rect(content_rect);
        if hit_toggle(blur_toggle, x, y) {
            let current = config_store::current().appearance.overview_blur;
            config_store::update(|c| c.appearance.overview_blur = !current);
            return;
        }
        let motion_toggle = self.reduced_motion_toggle_rect(content_rect);
        if hit_toggle(motion_toggle, x, y) {
            let current = config_store::current().appearance.reduced_motion;
            config_store::update(|c| c.appearance.reduced_motion = !current);
            return;
        }
        if config_store::current().appearance.reduced_motion {
            return; // Slider ignores clicks while reduced motion is on.
        }
        let slider = self.speed_slider_rect(content_rect);
        if y >= slider.top - 8 && y < slider.bottom + 8 && x >= slider.left && x < slider.right {
            let value = value_from_slider_x(slider, x, 0.5, 2.0);
            config_store::update(|c| c.appearance.animation_scale = value);
        }
    }
}
```

- [ ] **Step 2: Register and dispatch**

Add `pub(crate) mod overview;` to `apps/settings/src/imp/pages/mod.rs`.

In `apps/settings/src/imp/window.rs`, add the thread-local:

```rust
    static OVERVIEW_PAGE: RefCell<super::pages::overview::OverviewPage> = RefCell::new(super::pages::overview::OverviewPage::new());
```

Extend both `match selected` blocks with `3 => OVERVIEW_PAGE.with(|p| p.borrow().paint(hdc, content)),` (paint) and `3 => OVERVIEW_PAGE.with(|p| p.borrow_mut().on_click(x, y, content)),` (click).

- [ ] **Step 3: Build and manually verify**

Run: `cargo build -p groveshell-settings`
Expected: builds cleanly.

Manual verification: toggle overview blur, confirm the running overview visibly blurs/unblurs when opened; toggle reduced motion, confirm overview open/close and carousel transitions become instant; drag the speed slider (with reduced motion off), confirm transitions visibly speed up/slow down.

- [ ] **Step 4: Commit**

```bash
git add apps/settings/src/imp/pages/overview.rs apps/settings/src/imp/pages/mod.rs apps/settings/src/imp/window.rs
git commit -m "feat(settings): add the Overview settings page"
```

---

### Task 18: Input settings page (overview modifier + hot corners) and final integration pass

**Files:**
- Create: `apps/settings/src/imp/pages/input.rs`
- Modify: `apps/settings/src/imp/pages/mod.rs`
- Modify: `apps/settings/src/imp/window.rs`

**Interfaces:**
- Produces: `pub(crate) struct InputPage` implementing `Page`: overview-modifier 3-way segmented control (Super/Alt/CtrlAlt) and four hot-corner segmented controls (one per corner: `none`/`activities`), each reading/writing `config.hot_corners.get("top_left")` etc.

- [ ] **Step 1: Write `InputPage`**

Create `apps/settings/src/imp/pages/input.rs`:

```rust
//! Input settings: the overview/move-resize trigger and per-corner hot
//! corner actions.

use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Gdi::HDC;

use groveshell_config::HotCornerConfig;

use super::Page;
use crate::imp::config_store;
use crate::imp::theme::{draw_segmented, segmented_hit, TEXT_MUTED};
use crate::imp::util_text::draw_centered_text;

const PADDING: i32 = 24;
const ROW_HEIGHT: i32 = 48;
const CONTROL_WIDTH: i32 = 320;

const MODIFIER_OPTIONS: [&str; 3] = ["Super", "Alt", "CtrlAlt"];
const CORNER_ACTION_OPTIONS: [&str; 2] = ["none", "activities"];
const CORNERS: [&str; 4] = ["top_left", "top_right", "bottom_left", "bottom_right"];

pub(crate) struct InputPage;

impl InputPage {
    pub(crate) fn new() -> Self {
        Self
    }

    fn modifier_rect(&self, content_rect: RECT) -> RECT {
        RECT {
            left: content_rect.left + PADDING,
            top: content_rect.top + PADDING + ROW_HEIGHT,
            right: content_rect.left + PADDING + CONTROL_WIDTH,
            bottom: content_rect.top + PADDING + ROW_HEIGHT + 32,
        }
    }

    fn corner_rect(&self, content_rect: RECT, index: usize) -> RECT {
        RECT {
            left: content_rect.left + PADDING,
            top: content_rect.top + PADDING + ROW_HEIGHT * (3 + index as i32),
            right: content_rect.left + PADDING + CONTROL_WIDTH,
            bottom: content_rect.top + PADDING + ROW_HEIGHT * (3 + index as i32) + 32,
        }
    }
}

impl Page for InputPage {
    fn paint(&self, hdc: HDC, content_rect: RECT) {
        let config = config_store::current();
        let modifier_index = MODIFIER_OPTIONS
            .iter()
            .position(|m| *m == config.input.overview_modifier)
            .unwrap_or(0);

        // SAFETY: `hdc` is a valid device context from the caller's
        // `BeginPaint`, live for the duration of this call.
        unsafe {
            draw_centered_text(
                hdc,
                RECT { left: content_rect.left + PADDING, top: content_rect.top + PADDING, right: content_rect.right - PADDING, bottom: content_rect.top + PADDING + 24 },
                "Overview / move-resize trigger",
                TEXT_MUTED,
            );
            draw_segmented(hdc, self.modifier_rect(content_rect), &MODIFIER_OPTIONS, modifier_index);

            for (i, corner) in CORNERS.iter().enumerate() {
                let action = config.hot_corners.get(*corner).map(|c| c.action.clone()).unwrap_or_else(|| "none".to_string());
                let action_index = CORNER_ACTION_OPTIONS.iter().position(|a| *a == action).unwrap_or(0);
                let label_rect = RECT {
                    left: content_rect.left + PADDING,
                    top: content_rect.top + PADDING + ROW_HEIGHT * (2 + i as i32),
                    right: content_rect.right - PADDING,
                    bottom: content_rect.top + PADDING + ROW_HEIGHT * (2 + i as i32) + 24,
                };
                draw_centered_text(hdc, label_rect, &corner_display_name(corner), TEXT_MUTED);
                draw_segmented(hdc, self.corner_rect(content_rect, i), &CORNER_ACTION_OPTIONS, action_index);
            }
        }
    }

    fn on_click(&mut self, x: i32, y: i32, content_rect: RECT) {
        if let Some(index) = segmented_hit(self.modifier_rect(content_rect), &MODIFIER_OPTIONS, x, y) {
            let modifier = MODIFIER_OPTIONS[index].to_string();
            config_store::update(|c| c.input.overview_modifier = modifier.clone());
            return;
        }
        for (i, corner) in CORNERS.iter().enumerate() {
            if let Some(index) = segmented_hit(self.corner_rect(content_rect, i), &CORNER_ACTION_OPTIONS, x, y) {
                let action = CORNER_ACTION_OPTIONS[index].to_string();
                let corner_name = corner.to_string();
                config_store::update(|c| {
                    let entry = c.hot_corners.entry(corner_name.clone()).or_insert_with(|| HotCornerConfig {
                        action: "none".to_string(),
                        delay_ms: 150,
                        disable_in_fullscreen: true,
                    });
                    entry.action = action.clone();
                });
                return;
            }
        }
    }
}

fn corner_display_name(corner: &str) -> String {
    match corner {
        "top_left" => "Top-left corner".to_string(),
        "top_right" => "Top-right corner".to_string(),
        "bottom_left" => "Bottom-left corner".to_string(),
        "bottom_right" => "Bottom-right corner".to_string(),
        other => other.to_string(),
    }
}
```

Note: `Config::validate()` (Task 1) currently accepts hot-corner actions `"" | "activities" | "none"` — this page only ever writes `"none"`/`"activities"`, both already valid, so no further schema change is needed.

- [ ] **Step 2: Register and dispatch**

Add `pub(crate) mod input;` to `apps/settings/src/imp/pages/mod.rs`.

In `apps/settings/src/imp/window.rs`, add the thread-local:

```rust
    static INPUT_PAGE: RefCell<super::pages::input::InputPage> = RefCell::new(super::pages::input::InputPage::new());
```

Extend both `match selected` blocks with `4 => INPUT_PAGE.with(|p| p.borrow().paint(hdc, content)),` (paint) and `4 => INPUT_PAGE.with(|p| p.borrow_mut().on_click(x, y, content)),` (click).

- [ ] **Step 3: Build**

Run: `cargo build --workspace && cargo test --workspace`
Expected: the whole workspace builds cleanly and every test (config, ipc, ui, settings) passes.

- [ ] **Step 4: Full manual verification pass**

With `groveshell-watchdog.exe`, `groveshell-host.exe`, `groveshell-ui.exe`, and `groveshell-settings.exe` all freshly built:

1. Run `groveshell-settings.exe`. Confirm: tray icon appears with the GroveShell logo; `watchdog` → `host` → `ui` all start (Task Manager); the real Windows taskbar disappears and GroveShell's bar/dock appear.
2. Open Settings (tray left-click). Home page reports Healthy with three plausible process rows.
3. Click "Restore Explorer." Confirm: real taskbar and Start menu return; GroveShell's bar/dock/overview disappear; the button and tray menu now say "Start GroveShell."
4. Click "Start GroveShell." Confirm everything from step 1 comes back.
5. On the Input page, set the trigger to "Alt." Confirm tapping Alt (not Windows key) now toggles the overview.
6. On the Dock page, change alignment to "left" and drag the icon-size slider. Confirm the running dock's position/icon size change without restarting `groveshell-ui.exe`.
7. On the Top Bar page, toggle blur. Confirm the running bar visibly blurs/unblurs.
8. On the Overview page, toggle blur and reduced motion, and drag the speed slider (with reduced motion off). Confirm each takes effect on the next overview open.
9. On the Input page, set the top-left hot corner to "activities." Confirm dragging the cursor into that corner opens the overview (this exercises `hot_corners` end-to-end, unchanged runtime behavior — this page only adds the *editing* surface for a config section that already worked).
10. Toggle "Start with Windows" on. Confirm the `HKCU\...\Run\GroveShell` registry value appears pointing at `groveshell-settings.exe`. Toggle off, confirm it's removed.
11. Kill `groveshell-ui.exe` directly from Task Manager. Confirm the Home page's health indicator flips to "Unhealthy: ui is not running" within a couple of 2-second refresh ticks.
12. Click "Exit GroveShell" from the tray menu. Confirm the real taskbar is restored, all four GroveShell processes exit, and the tray icon disappears.

- [ ] **Step 5: Commit**

```bash
git add apps/settings/src/imp/pages/input.rs apps/settings/src/imp/pages/mod.rs apps/settings/src/imp/window.rs
git commit -m "feat(settings): add the Input settings page (overview modifier + hot corners)"
```

---

## Self-Review Notes

- **Spec coverage:** every design-doc section has a task — crate/tray/lifecycle (Tasks 3, 5, 6), health/stats (Task 7), config schema (Task 1), `apps/ui` config consumption (Tasks 12-14), settings window + all five pages (Tasks 8, 9, 15-18), icon (Task 4), autostart (Task 10), IPC message (Task 2), config_store save/reload choke point (Task 11 wiring + Task 14 receiver).
- **Placeholder scan:** the two intentional temporary stubs (`window::open_settings_window` in Task 6, `HomePage` in Task 8) are explicitly called out as placeholders *replaced within the plan itself* (Tasks 8 and 9 respectively), not left dangling — every other step has real, complete code.
- **Type consistency:** `Page` trait (Task 8) is implemented identically by `HomePage` (9), `DockPage` (15), `TopBarPage` (16), `OverviewPage` (17), `InputPage` (18) — `paint(&self, hdc: HDC, content_rect: RECT)` / `on_click(&mut self, x: i32, y: i32, content_rect: RECT)` throughout. `config_store::update(impl FnOnce(&mut Config))` signature is used identically by every page task. `ManagedProcesses` methods (`spawn_all`, `start_all`, `stop_all`, `is_ui_running`, `pid_of`) are defined once in Task 5/9 and never redefined elsewhere.
- **Scope check:** this is one cohesive feature (one new binary + the config/IPC plumbing it needs); it was not decomposed into separate specs/plans because every piece is load-bearing for the others (a tray app with no settings window, or a settings window with no config plumbing to actually apply, would each be an incomplete deliverable on their own).

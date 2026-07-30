//! Persisted, independent pinned-dock-app list — GroveShell's own record
//! of what's pinned, seeded once from the real Windows taskbar's current
//! pins (see `dock::taskbar_pinned_shortcuts`) and never consulting it
//! again afterward. Deliberately not part of `groveshell-config`'s shared
//! `Config`: that file is user-editable settings shared (and potentially
//! written) across other processes with its own schema/versioning and
//! backup-on-save behavior tuned for occasional hand-edited changes, not
//! frequent UI-driven pin/unpin/reorder writes.

use std::path::{Path, PathBuf};

/// Reorders `pins` so the entry at `from` ends up at `to`. Out-of-range
/// indices or `from == to` return `pins` unchanged (cloned) rather than
/// panicking — a stale index from a race between a rebuild and a
/// still-in-flight drag should never crash the drag-drop handler.
pub(crate) fn reorder(pins: &[PathBuf], from: usize, to: usize) -> Vec<PathBuf> {
    if from >= pins.len() || to >= pins.len() || from == to {
        return pins.to_vec();
    }
    let mut result = pins.to_vec();
    let item = result.remove(from);
    result.insert(to, item);
    result
}

/// The persisted pinned-list file's path: `<data_dir>/dock_pins.json`.
/// `None` only if the data directory itself can't be determined (see
/// `groveshell_common::paths::data_dir`).
pub(crate) fn pins_file_path() -> Option<PathBuf> {
    groveshell_common::paths::data_dir().ok().map(|d| d.join("dock_pins.json"))
}

/// Loads the persisted list. `None` if the file doesn't exist yet or
/// fails to parse — both cases mean "nothing persisted," which the
/// caller (`load_or_seed`) treats as "seed it."
pub(crate) fn load(path: &Path) -> Option<Vec<PathBuf>> {
    let data = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&data).ok()
}

/// Overwrites the persisted list. Best-effort: a failed write (e.g. the
/// data directory briefly unwritable) just means the next pin/unpin/
/// reorder tries again — there's no in-memory state lost, since the
/// caller's own `thread_local!` copy is already updated regardless.
pub(crate) fn save(path: &Path, pins: &[PathBuf]) {
    let Ok(data) = serde_json::to_string_pretty(pins) else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, data);
}

/// Loads the persisted list if it exists; otherwise calls `seed` (the
/// real taskbar's current pins, at startup) and persists that as the
/// starting point — from then on this file is authoritative and the
/// real taskbar is never consulted again.
pub(crate) fn load_or_seed(path: &Path, seed: impl FnOnce() -> Vec<PathBuf>) -> Vec<PathBuf> {
    if let Some(pins) = load(path) {
        return pins;
    }
    let seeded = seed();
    save(path, &seeded);
    seeded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reorder_moves_the_entry_from_one_index_to_another() {
        let pins = vec![PathBuf::from("a"), PathBuf::from("b"), PathBuf::from("c")];
        let result = reorder(&pins, 0, 2);
        assert_eq!(result, vec![PathBuf::from("b"), PathBuf::from("c"), PathBuf::from("a")]);
    }

    #[test]
    fn reorder_moving_backward_shifts_the_entries_between() {
        let pins = vec![PathBuf::from("a"), PathBuf::from("b"), PathBuf::from("c")];
        let result = reorder(&pins, 2, 0);
        assert_eq!(result, vec![PathBuf::from("c"), PathBuf::from("a"), PathBuf::from("b")]);
    }

    #[test]
    fn reorder_same_index_is_a_no_op() {
        let pins = vec![PathBuf::from("a"), PathBuf::from("b")];
        assert_eq!(reorder(&pins, 1, 1), pins);
    }

    #[test]
    fn reorder_out_of_range_index_returns_pins_unchanged() {
        let pins = vec![PathBuf::from("a"), PathBuf::from("b")];
        assert_eq!(reorder(&pins, 0, 5), pins);
        assert_eq!(reorder(&pins, 5, 0), pins);
    }
}

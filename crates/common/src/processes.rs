//! Finding a running process by the name of its executable.
//!
//! The shell is several processes that are not always started by the one
//! asking about them: the settings app may find a shell that a script
//! started, and the watchdog has to notice the UI dying without having
//! launched it. Neither has a handle to wait on, so they look by name.

/// The id of a running process whose executable is `exe_name`
/// (`"groveshell-ui.exe"`), compared without regard to case. `None` if
/// there is none, or if the process list could not be read.
#[cfg(windows)]
pub fn find(exe_name: &str) -> Option<u32> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };

    // SAFETY: the snapshot handle is closed before returning; `entry` is
    // a live local with its size field set, as the API requires.
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok()?;
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut found = None;
        let mut more = Process32FirstW(snapshot, &mut entry).is_ok();
        while more {
            if name_matches(&entry.szExeFile, exe_name) {
                found = Some(entry.th32ProcessID);
                break;
            }
            more = Process32NextW(snapshot, &mut entry).is_ok();
        }
        let _ = CloseHandle(snapshot);
        found
    }
}

/// Whether a fixed-size, NUL-terminated UTF-16 name buffer holds
/// `wanted`. What follows the terminator is left over from a previous
/// entry and is not part of the name.
pub fn name_matches(buffer: &[u16], wanted: &str) -> bool {
    let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..end]).eq_ignore_ascii_case(wanted)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buffer(name: &str, garbage: &str) -> Vec<u16> {
        let mut b: Vec<u16> = name.encode_utf16().collect();
        b.push(0);
        b.extend(garbage.encode_utf16());
        b
    }

    #[test]
    fn the_name_ends_at_its_terminator() {
        assert!(name_matches(&buffer("explorer.exe", "leftover.exe"), "explorer.exe"));
        assert!(!name_matches(&buffer("explorer.exe", "leftover.exe"), "leftover.exe"));
    }

    #[test]
    fn case_does_not_matter() {
        assert!(name_matches(&buffer("GroveShell-UI.EXE", ""), "groveshell-ui.exe"));
    }

    #[test]
    fn a_longer_name_that_starts_the_same_is_a_different_program() {
        assert!(!name_matches(&buffer("groveshell-ui.exe.bak", ""), "groveshell-ui.exe"));
        assert!(!name_matches(&buffer("", ""), "groveshell-ui.exe"));
    }

    #[cfg(windows)]
    #[test]
    fn this_test_process_can_find_itself() {
        let exe = std::env::current_exe().unwrap();
        let name = exe.file_name().unwrap().to_str().unwrap();
        assert_eq!(find(name).is_some(), true);
        assert_eq!(find("no-such-program-anywhere.exe"), None);
    }
}

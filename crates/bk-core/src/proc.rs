//! Spawning helper processes.

use std::ffi::OsStr;
use std::process::Command;

/// A command for background work (ssh checks, conda snapshots, ...). On Windows it runs without
/// a console window, so a GUI app doesn't flash one for every call.
pub fn background(program: impl AsRef<OsStr>) -> Command {
    #[allow(unused_mut)]
    let mut c = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    c
}

/// Exit code of a finished process as the 0–255 status shells report: signals become 128+N on
/// Unix, Ctrl+C (`STATUS_CONTROL_C_EXIT`) becomes 130 on Windows, and anything else outside the
/// range (crash codes like `0xC0000005`) becomes 1, so a failure is never reported as success.
pub fn exit_code(status: std::process::ExitStatus) -> i32 {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return 128 + sig;
        }
    }
    status.code().map(normalize_code).unwrap_or(1)
}

/// See [`exit_code`]; for raw codes from other sources (portable-pty, `std::process::exit`).
pub fn normalize_code(code: i32) -> i32 {
    const STATUS_CONTROL_C_EXIT: u32 = 0xC000_013A;
    if code as u32 == STATUS_CONTROL_C_EXIT {
        130
    } else if (0..=255).contains(&code) {
        code
    } else {
        1
    }
}

/// Whether a status means the process was stopped by Ctrl+C / SIGINT.
pub fn interrupted(status: std::process::ExitStatus) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if status.signal() == Some(2) {
            return true;
        }
    }
    status.code().is_some_and(|c| c == 130 || c as u32 == 0xC000_013A)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes() {
        assert_eq!(normalize_code(0), 0);
        assert_eq!(normalize_code(7), 7);
        assert_eq!(normalize_code(0xC000_013Au32 as i32), 130);
        assert_eq!(normalize_code(0xC000_0005u32 as i32), 1);
        assert_eq!(normalize_code(-1), 1);
        assert_eq!(normalize_code(256), 1);
    }
}

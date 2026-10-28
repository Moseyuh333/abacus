//! What the terminal in front of us can actually draw.
//!
//! Windows consoles decode the bytes a program writes with a codepage, not
//! UTF-8. Rust writes UTF-8 to the console handle, so on a console whose output
//! codepage is 936 -- the default on a Chinese Windows install -- every
//! non-ASCII glyph comes out as mojibake: the arrow in front of `Paste an API
//! key` was reported as `釟?`, and a provider name typed at a prompt is decoded
//! from the *input* codepage, so it arrives as invalid UTF-8.
//!
//! Switching the console to UTF-8 fixes display and input at once. When the
//! console refuses the switch, the glyph tables fall back to ASCII so the
//! interface degrades to plain characters instead of corrupting them.

use std::sync::atomic::{AtomicU8, Ordering};

const UNKNOWN: u8 = 0;
const UTF8: u8 = 1;
const NOT_UTF8: u8 = 2;

/// Cached answer for [`console_can_draw_unicode`]: probing the console is a
/// syscall, and the glyph set is consulted several times per drawn row.
static CONSOLE_UTF8: AtomicU8 = AtomicU8::new(UNKNOWN);

/// Put the console into UTF-8 mode, and report whether it now is.
///
/// Called once at startup, before anything is printed, so clap's help, the
/// setup wizard, `doctor` and the TUI all inherit the same answer. A console
/// that is not attached (piped output) needs nothing: UTF-8 bytes are exactly
/// what a pipe wants.
pub fn ensure_utf8_console() -> bool {
    match CONSOLE_UTF8.load(Ordering::Relaxed) {
        UTF8 => true,
        NOT_UTF8 => false,
        _ => {
            let ok = platform::switch_to_utf8();
            CONSOLE_UTF8.store(if ok { UTF8 } else { NOT_UTF8 }, Ordering::Relaxed);
            ok
        }
    }
}

/// Whether the active console can draw the Unicode glyph set.
///
/// Never switches anything: this is the read used by the glyph tables, so it
/// answers the same way before and after [`ensure_utf8_console`]. On a
/// platform without codepages the answer is always yes.
pub fn console_can_draw_unicode() -> bool {
    match CONSOLE_UTF8.load(Ordering::Relaxed) {
        UTF8 => true,
        NOT_UTF8 => false,
        _ => {
            let ok = platform::console_is_utf8();
            CONSOLE_UTF8.store(if ok { UTF8 } else { NOT_UTF8 }, Ordering::Relaxed);
            ok
        }
    }
}

#[cfg(windows)]
mod platform {
    use std::io::IsTerminal;

    const CP_UTF8: u32 = 65001;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetConsoleOutputCP() -> u32;
        fn SetConsoleOutputCP(codepage: u32) -> i32;
        fn GetConsoleCP() -> u32;
        fn SetConsoleCP(codepage: u32) -> i32;
    }

    /// A redirected stream has no codepage to speak of, so nothing to fix.
    fn attached() -> bool {
        std::io::stdout().is_terminal() || std::io::stderr().is_terminal()
    }

    pub(super) fn console_is_utf8() -> bool {
        if !attached() {
            return true;
        }
        // SAFETY: both calls read a console property and take no arguments.
        unsafe { GetConsoleOutputCP() == CP_UTF8 && GetConsoleCP() == CP_UTF8 }
    }

    pub(super) fn switch_to_utf8() -> bool {
        if !attached() {
            return true;
        }
        // SAFETY: the two setters take a codepage we control (65001, a constant
        // Windows understands) and return 0 on failure. The reads afterwards
        // are the same property reads as in `console_is_utf8`.
        unsafe {
            if GetConsoleOutputCP() != CP_UTF8 {
                SetConsoleOutputCP(CP_UTF8);
            }
            if GetConsoleCP() != CP_UTF8 {
                SetConsoleCP(CP_UTF8);
            }
            console_is_utf8()
        }
    }
}

#[cfg(not(windows))]
mod platform {
    /// A terminal on Unix speaks UTF-8 or something the font handles; there is
    /// no codepage to consult and nothing to switch.
    pub(super) fn console_is_utf8() -> bool {
        true
    }

    pub(super) fn switch_to_utf8() -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The probe is cached, so two calls must agree -- a glyph set that flips
    /// between frames would redraw the whole interface in another alphabet.
    #[test]
    fn the_console_probe_is_stable() {
        let first = console_can_draw_unicode();
        assert_eq!(first, console_can_draw_unicode());
    }

    /// On the platforms CI runs (Linux, macOS) there is no codepage, so the
    /// Unicode set is always drawable and startup must never report a failure.
    #[cfg(not(windows))]
    #[test]
    fn a_unix_terminal_needs_no_switch() {
        assert!(console_can_draw_unicode());
        assert!(ensure_utf8_console());
    }
}

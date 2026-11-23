use std::env;
use std::io::{self, IsTerminal};
use std::sync::OnceLock;

pub(crate) fn supports_styling() -> bool {
    static SUPPORTED: OnceLock<bool> = OnceLock::new();
    *SUPPORTED.get_or_init(|| {
        should_style(
            io::stdout().is_terminal(),
            io::stderr().is_terminal(),
            env::var_os("NO_COLOR").is_some(),
            env::var_os("TERM").is_some_and(|term| term == "dumb"),
            enable_ansi,
        )
    })
}

fn should_style(
    stdout_is_terminal: bool,
    stderr_is_terminal: bool,
    no_color: bool,
    dumb_terminal: bool,
    enable_ansi: impl FnOnce() -> bool,
) -> bool {
    stdout_is_terminal && stderr_is_terminal && !no_color && !dumb_terminal && enable_ansi()
}

#[cfg(windows)]
fn enable_ansi() -> bool {
    anstyle_query::windows::enable_virtual_terminal_processing().is_ok()
}

#[cfg(not(windows))]
fn enable_ansi() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::should_style;

    #[test]
    fn plain_output_skips_ansi_setup() {
        let enable_ansi = || panic!("plain output should not try to enable ANSI");
        assert!(!should_style(false, true, false, false, enable_ansi));
        assert!(!should_style(true, false, false, false, enable_ansi));
        assert!(!should_style(true, true, true, false, enable_ansi));
        assert!(!should_style(true, true, false, true, enable_ansi));
    }

    #[test]
    fn styling_requires_successful_ansi_setup() {
        assert!(!should_style(true, true, false, false, || false));
        assert!(should_style(true, true, false, false, || true));
    }
}

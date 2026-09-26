//! Which screen `suv` opens when it is given no command, or `home`.
//!
//! Kept pure so the whole decision table is tested without a terminal:
//! the caller detects the terminal and reads the preference, this decides.

use crate::config::HomeStartup;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartupRoute {
    /// Open the Home screen.
    Home,
    /// Print the command overview to stdout and exit successfully.
    ClassicHelp,
    /// Exactly what a bare `suv` has always done without a terminal: clap's
    /// missing-subcommand help on stderr, exit 2.
    LegacyMissingCommand,
    /// `suv home` asked for without a usable terminal: explain, exit 2.
    RejectHome,
}

/// Decide the route. The terminal is checked first, so neither a pipe nor a
/// dumb terminal ever reaches the preference; then an explicit `suv home`;
/// only then the saved startup preference.
pub const fn resolve_startup(
    explicit_home: bool,
    interactive: bool,
    dumb_terminal: bool,
    preference: HomeStartup,
) -> StartupRoute {
    let usable = interactive && !dumb_terminal;
    match (usable, explicit_home, preference) {
        (false, true, _) => StartupRoute::RejectHome,
        (false, false, _) => StartupRoute::LegacyMissingCommand,
        (true, true, _) | (true, false, HomeStartup::Home) => StartupRoute::Home,
        (true, false, HomeStartup::Help) => StartupRoute::ClassicHelp,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipes_never_open_home() {
        assert_eq!(
            resolve_startup(false, false, false, HomeStartup::Home),
            StartupRoute::LegacyMissingCommand
        );
        assert_eq!(
            resolve_startup(true, false, false, HomeStartup::Home),
            StartupRoute::RejectHome
        );
    }

    #[test]
    fn a_dumb_terminal_is_treated_like_no_terminal() {
        for preference in [HomeStartup::Home, HomeStartup::Help] {
            assert_eq!(
                resolve_startup(false, true, true, preference),
                StartupRoute::LegacyMissingCommand
            );
            assert_eq!(
                resolve_startup(true, true, true, preference),
                StartupRoute::RejectHome
            );
        }
    }

    #[test]
    fn bare_suv_at_a_terminal_follows_the_preference() {
        assert_eq!(
            resolve_startup(false, true, false, HomeStartup::Home),
            StartupRoute::Home
        );
        assert_eq!(
            resolve_startup(false, true, false, HomeStartup::Help),
            StartupRoute::ClassicHelp
        );
    }

    #[test]
    fn explicit_home_overrides_a_classic_help_preference() {
        for preference in [HomeStartup::Home, HomeStartup::Help] {
            assert_eq!(
                resolve_startup(true, true, false, preference),
                StartupRoute::Home
            );
        }
    }
}

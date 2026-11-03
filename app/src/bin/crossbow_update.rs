//! Finishes installing an update once Crossbow has quit: the app starts it
//! from `Contents/MacOS` with its update folder, then quits. It waits for
//! the app to end and for no other copy of it to run, checks the staged
//! app's signature again, swaps it in, checks it there, runs it once and
//! swaps back if that fails. It acts only on an install into the app it
//! sits in. It runs as the user, needs no entitlement, and logs no paths.

use crossbow_core::update::{codesign, install};
use std::time::Duration;

/// How long Crossbow may take to quit.
const QUIT_TIMEOUT: Duration = Duration::from_secs(120);

fn main() -> std::process::ExitCode {
    let mut args = std::env::args_os().skip(1);
    let (Some(state), None) = (args.next(), args.next()) else {
        eprintln!("usage: crossbow-update <update folder>");
        return std::process::ExitCode::from(2);
    };
    let state = std::path::PathBuf::from(state);
    let Some(here) = std::env::current_exe().ok().and_then(|exe| install::running_bundle(&exe)) else {
        eprintln!("crossbow-update: not inside Crossbow.app; nothing changed");
        return std::process::ExitCode::FAILURE;
    };
    let mut ops = install::System::new(&state, Box::new(codesign::System));
    match install::helper(&state, &here, &mut ops, |pid| install::wait_for_exit(pid, QUIT_TIMEOUT)) {
        Ok(finished) => {
            eprintln!("crossbow-update: {}", match finished {
                install::Finished::Nothing => "nothing to install".to_string(),
                install::Finished::NotQuit => "Crossbow didn't quit; nothing changed".to_string(),
                install::Finished::Probed => "installed; the new version confirms itself when it opens".to_string(),
                install::Finished::Done(outcome) => outcome.text(),
            });
            std::process::ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("crossbow-update: stopped ({}); Crossbow finishes or undoes it at its next start", error.kind());
            std::process::ExitCode::FAILURE
        }
    }
}

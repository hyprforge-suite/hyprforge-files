//! `hyprforge-files-admin` — see `hyprforge_files::admin`. Run by
//! `pkexec` on behalf of a Files window; it takes no arguments, reads no
//! configuration and no environment, and serves requests on standard
//! input until that closes or nothing is asked for five minutes.

use std::process::ExitCode;

fn main() -> ExitCode {
    if std::env::args_os().len() > 1 {
        eprintln!("hyprforge-files-admin takes no arguments; Files starts it through pkexec.");
        return ExitCode::from(2);
    }
    match hyprforge_files::admin::serve(std::io::stdin(), std::io::stdout().lock(), hyprforge_files::admin::IDLE) {
        Ok(()) => ExitCode::SUCCESS,
        // The window's end of the pipe is gone: nobody is left to tell.
        Err(_) => ExitCode::FAILURE,
    }
}

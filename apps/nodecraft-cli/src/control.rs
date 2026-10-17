//! Headless control for a trusted stdio parent; no socket, token file or keychain service.
use std::process::ExitCode;

use nodecraft_types::{CanvasId, ProjectId};

pub fn run() -> ExitCode {
    let mut new_id = super::random_bytes();
    let mut session = match nodecraft_automation::blank_session(ProjectId::from_bytes(new_id()), CanvasId::from_bytes(new_id())) {
        Ok(session) => session,
        Err(error) => {
            eprintln!("error: can't start control: {}", error.message);
            return ExitCode::FAILURE;
        }
    };
    match nodecraft_automation::control::stdio::serve(&mut session, &mut std::io::stdin().lock(), &mut std::io::stdout().lock()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: control stdio I/O failed: {error}");
            ExitCode::FAILURE
        }
    }
}

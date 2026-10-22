//! What a command would do, decided without doing it (#114): the in-canvas agent asks before it
//! acts, so assist mode can show the user a plan and auto mode can keep runs within its budget.
//! [`crate::Session::impact`] runs the command's handler exactly as dispatch would (same
//! validation, same enabled check, same panic guard) and classifies the effect it describes;
//! nothing is applied, and no service, file or run is started.
use crate::commands::Effect;
use nodecraft_types::{Currency, Error};
use std::collections::BTreeMap;

/// The class of change a command would make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Impact {
    /// Reads only: the document and the session stay as they are.
    Read,
    /// Changes the document in one undoable step (or undoes or redoes one).
    Edit,
    /// Starts a run that may spend up to its accepted amounts.
    Run(RunImpact),
    /// Asks the host for something outside the document: a network fetch, a file, the
    /// performance recorder.
    Host,
}

/// What a run accepted: the most it may spend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunImpact {
    /// The ceiling per currency, in micro-units.
    pub ceiling: BTreeMap<Currency, u64>,
    /// The caller agreed although part of the price isn't known.
    pub partial: bool,
    /// How many jobs of unknown price the run may admit.
    pub unpriced_jobs: u64,
}

impl Impact {
    pub(crate) fn of(effect: &Effect) -> Result<Self, Error> {
        Ok(match effect {
            Effect::None { .. } | Effect::Elements { request: crate::element_library::Request::List } => Self::Read,
            Effect::Edit { .. } | Effect::Undo | Effect::Redo | Effect::Place { .. } => Self::Edit,
            Effect::PreparedRun { prepared } => {
                Self::Run(RunImpact { ceiling: prepared.ceiling()?, partial: prepared.partial(), unpriced_jobs: prepared.unpriced_jobs() })
            }
            Effect::Open { .. }
            | Effect::CancelFile
            | Effect::Save { .. }
            | Effect::Service { .. }
            | Effect::Export { .. }
            | Effect::Perf { .. }
            | Effect::Elements { .. } => Self::Host,
        })
    }
}

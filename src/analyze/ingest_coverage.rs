//! Ingest coverage: every support-dir file (`lib/`, an in-repo engine's
//! `lib/`, `app/helpers/`, …) whose library-class ingest failed outside
//! survey mode (`App::ingest_drops`). The walk over those dirs tolerates
//! a failed file so one gap does not abort the app, which left the file
//! out of the IR in silence; each is now an `ingest_unsupported` error
//! at the construct that failed it.

use crate::diagnostic::Diagnostic;
use crate::App;

pub(super) fn diagnose(app: &App) -> Vec<Diagnostic> {
    app.ingest_drops.iter().map(|drop| Diagnostic::ingest_unsupported(drop.span, drop.message.clone())).collect()
}

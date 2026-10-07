//! Ingest coverage: every file whose library-class ingest failed and
//! that ingest went on without (`App::ingest_drops`): a support-dir file
//! (`lib/`, an in-repo engine's `lib/`, `app/helpers/`, …), whose walk
//! tolerates a failed file so one gap does not abort the app, or in
//! survey mode any per-file class ingest. That left the file out of the
//! IR in silence (or as a file-only punch-list line); each is now an
//! `ingest_unsupported` error at the construct that failed it. A
//! support-file definition the ingest carries but not as Ruby runs it
//! (`file_level_definition`: inside a file-level conditional, or after a
//! file-level `return`) is an `unsupported` error at that statement.

use crate::diagnostic::Diagnostic;
use crate::App;

pub(super) fn diagnose(app: &App) -> Vec<Diagnostic> {
    app.ingest_drops
        .iter()
        .map(|drop| match &drop.construct {
            // Only this statement: the rest of the file is carried.
            Some(construct) => Diagnostic::unsupported(drop.span, None, construct.as_str(), drop.message.clone()),
            None => Diagnostic::ingest_unsupported(drop.span, drop.message.clone()),
        })
        .collect()
}

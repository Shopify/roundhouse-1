//! A fixture entry the loader cannot place is dropped from the emitted
//! test data (`lower::fixtures::unloaded_fixture_entries`). Rails loads
//! it, so name each one instead of letting the tests run on less data.

use crate::diagnostic::Diagnostic;
use crate::span::Span;
use crate::App;

pub(super) fn diagnose(app: &App) -> Vec<Diagnostic> {
    crate::lower::fixtures::unloaded_fixture_entries(app)
        .into_iter()
        .map(|detail| Diagnostic::unsupported(Span::synthetic(), None, "fixture_entry", detail))
        .collect()
}

//! Database-identity refusals (`crate::database`): a manifest that is
//! absent, stale or mismatched (`ingest::database_manifest`), and every
//! model-to-database routing the emitted runtime does not carry
//! (`ingest::database_ownership`). Each is an error at its source span,
//! so an app that needs a database the runtime does not open is
//! reported unsupported instead of silently running on the primary.
//! The manifest's provenance that the tree cannot verify is a warning,
//! so it is visible and never presented as verified.

use crate::diagnostic::Diagnostic;
use crate::App;

pub(super) fn diagnose(app: &App) -> Vec<Diagnostic> {
    app.database_refusals
        .iter()
        .map(|r| {
            let mut d = Diagnostic::unsupported(r.span, None, r.construct.as_str(), r.detail.clone());
            if r.warning {
                d.severity = crate::diagnostic::Severity::Warning;
            }
            d
        })
        .collect()
}

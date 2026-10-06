//! Initializer coverage (`ingest::initializers`): every statement in a
//! `config/initializers/` file — the app's or an in-repo engine's —
//! that Rails runs at boot and no ingest reader applies to the emitted
//! app. Each is an `initializer_unsupported` error at the statement, in
//! initializer load order, so boot behavior the emitted program lacks is
//! reported rather than dropped in silence.

use crate::diagnostic::Diagnostic;
use crate::App;

pub(super) fn diagnose(app: &App) -> Vec<Diagnostic> {
    app.initializer_gaps
        .iter()
        .map(|gap| Diagnostic::initializer_unsupported(gap.span, gap.construct.as_str(), gap.detail.clone()))
        .collect()
}

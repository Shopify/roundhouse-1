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
//!
//! Also every top-level call of a support file whose declarations were
//! carried (`App::file_statement_gaps`): a `file_statement_unsupported`
//! error at the call, which Ruby runs on load and the emitted app does
//! not. A call is reported by one of the two only: a call inside a
//! file-level conditional, or after a file-level `return`, is not a
//! top-level call statement.

use crate::diagnostic::{Diagnostic, DiagnosticKind};
use crate::App;

pub(super) fn diagnose(app: &App) -> Vec<Diagnostic> {
    let drops = app.ingest_drops.iter().map(|drop| match &drop.construct {
        // Only this statement: the rest of the file is carried.
        Some(construct) => Diagnostic::unsupported(drop.span, None, construct.as_str(), drop.message.clone()),
        None => Diagnostic::ingest_unsupported(drop.span, drop.message.clone()),
    });
    let calls = app.file_statement_gaps.iter().map(|gap| {
        let kind = DiagnosticKind::FileStatementUnsupported {
            construct: gap.construct.as_str().into(),
            detail: gap.detail.clone(),
        };
        Diagnostic {
            span: gap.span,
            severity: Diagnostic::default_severity(&kind),
            message: Diagnostic::stub_text(&kind),
            kind,
        }
    });
    drops.chain(calls).chain(cross_file_redefinitions(app)).collect()
}

/// A method that two files' reopens of one class both define. Ruby keeps
/// the def from whichever file loads last. Ingest merged the reopens in
/// walk order, the order the tree already assumes for a module reopened
/// across files (`emit_and_run`'s shared struct factory takes the newer
/// `prefix` that way), but that is not shown to be Ruby's load order. So
/// it is a WARNING at each def after the first that comes from another
/// file: visible in a check, and not a refusal of an order the tree
/// already relies on. A redefinition within one file follows source
/// order and is not reported.
fn cross_file_redefinitions(app: &App) -> Vec<Diagnostic> {
    use crate::det::HashMap;
    let mut out = Vec::new();
    for lc in &app.library_classes {
        if !app.library_class_reopens.contains_key(&lc.name) {
            continue;
        }
        let mut first_file: HashMap<(&str, crate::dialect::MethodReceiver), crate::span::FileId> = HashMap::default();
        for method in &lc.methods {
            let key = (method.name.as_str(), method.receiver);
            let file = method.name_span.file;
            match first_file.get(&key) {
                None => {
                    first_file.insert(key, file);
                }
                Some(first) if *first != file => {
                    let mut diagnostic = Diagnostic::unsupported(
                        method.name_span,
                        None,
                        "method redefined across reopened files",
                        format!(
                            "`{}` is defined by more than one file that reopens `{}`; Ruby keeps the one loaded last, and the emitted class keeps the last in walk order, which is not verified to be the same",
                            method.name.as_str(),
                            lc.name.0.as_str()
                        ),
                    );
                    diagnostic.severity = crate::diagnostic::Severity::Warning;
                    out.push(diagnostic);
                }
                Some(_) => {}
            }
        }
    }
    out
}

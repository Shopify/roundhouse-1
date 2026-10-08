//! Ingest coverage: every file whose library-class ingest failed and
//! that ingest went on without (`App::ingest_drops`): a support-dir file
//! (`lib/`, an in-repo engine's `lib/`, `app/helpers/`, …), whose walk
//! tolerates a failed file so one gap does not abort the app, or in
//! survey mode any per-file class ingest. That left the file out of the
//! IR in silence (or as a file-only punch-list line); each is now an
//! `ingest_unsupported` error at the construct that failed it. A
//! support file's file-level statement its file program does not carry
//! (`App::file_statement_gaps`, from `ProgramItem::NotCarried`) is a
//! `file_statement_unsupported` error at that statement, which Ruby runs
//! on load and the emitted app names at boot instead
//! (`error[file_statement_not_carried]`). A statement the program
//! carries (a call, a require, a conditional definition, a guard) is
//! not reported. Each site has one report: an item inside a carried
//! conditional is reported, the conditional itself is not.

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
    drops
        .chain(calls)
        .chain(required_left_out_files(app))
        .chain(validation_gaps(app))
        .chain(cross_file_redefinitions(app))
        .collect()
}

/// A file program's `require` / `require_relative` of an app file whose
/// classes, modules and constants ingest left out (its
/// `ingest_unsupported` drop): graphql-client's client.rb requires
/// graphql/client/operation_definition.rb, which the tree does not
/// carry. Ruby loads that file there; the emitted program names the
/// require at boot instead (`error[file_require_not_carried]`), and this
/// reports it at the require, with the reason the file was left out, so
/// a check shows what the boot will lack. Resolved as the emitted
/// program resolves it: a relative path from the requiring file, else
/// the one app file under a `lib/` the feature names.
fn required_left_out_files(app: &App) -> Vec<Diagnostic> {
    use crate::app::ProgramItem;
    use std::collections::BTreeMap;
    use std::path::{Component, Path, PathBuf};
    let left_out: BTreeMap<&str, &str> = app
        .ingest_drops
        .iter()
        .filter(|drop| drop.construct.is_none())
        .filter_map(|drop| {
            let file = app.sources.get((drop.span.file.0 as usize).checked_sub(1)?)?;
            Some((file.path.as_str(), drop.message.as_str()))
        })
        .collect();
    if left_out.is_empty() {
        return Vec::new();
    }
    let normalize = |path: &Path| -> String {
        let mut out = PathBuf::new();
        for part in path.components() {
            match part {
                Component::CurDir => {}
                Component::ParentDir => {
                    out.pop();
                }
                other => out.push(other),
            }
        }
        out.display().to_string()
    };
    let shown = |path: &str| path.strip_prefix(app.root.as_str()).map(|p| p.trim_start_matches('/')).unwrap_or(path).to_string();
    let resolve = |from: &str, feature: &str, relative: bool| -> Option<String> {
        let rb = if feature.ends_with(".rb") { feature.to_string() } else { format!("{feature}.rb") };
        if relative {
            return Some(normalize(&Path::new(from).parent()?.join(&rb)));
        }
        let nested = format!("/lib/{rb}");
        let mut hits = app.sources.iter().filter(|source| source.path.ends_with(&nested));
        match (hits.next(), hits.next()) {
            (Some(one), None) => Some(one.path.clone()),
            _ => None,
        }
    };
    fn walk<'a>(items: &'a [ProgramItem], out: &mut Vec<(crate::span::Span, &'a str, bool)>) {
        for item in items {
            match item {
                ProgramItem::Require { span, feature, relative } => out.push((*span, feature.as_str(), *relative)),
                ProgramItem::Branch { then_items, else_items, .. } => {
                    walk(then_items, out);
                    walk(else_items, out);
                }
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    for program in &app.file_programs {
        let mut requires = Vec::new();
        walk(&program.items, &mut requires);
        for (span, feature, relative) in requires {
            let Some(target) = resolve(&program.source, feature, relative) else { continue };
            let Some(reason) = left_out.get(target.as_str()) else { continue };
            let written = format!("require{} {feature:?}", if relative { "_relative" } else { "" });
            let kind = DiagnosticKind::FileStatementUnsupported {
                construct: written.as_str().into(),
                detail: format!(
                    "it loads {}, whose classes, modules and constants ingest left out of the emitted app ({reason})",
                    shown(&target)
                ),
            };
            out.push(Diagnostic {
                span,
                severity: Diagnostic::default_severity(&kind),
                message: Diagnostic::stub_text(&kind),
                kind,
            });
        }
    }
    out
}

/// A `validates` validator or option ingest does not model
/// (`ValidationRule::Unsupported`): an error at the call, because the
/// emitted `validate` runs without it and accepts records Rails rejects.
/// A model's own declarations and a concern's `included do` ones, which
/// are spliced into each includer, are reported once each.
fn validation_gaps(app: &App) -> Vec<Diagnostic> {
    use crate::dialect::{ModelBodyItem, ValidationRule};
    let mut out = Vec::new();
    let concern_items = app.concern_model_items.values().flatten();
    let items = app.models.iter().flat_map(|m| m.body.iter()).chain(concern_items);
    let mut seen = crate::det::HashSet::default();
    for item in items {
        // `has_many …, autosave: true` validates (and saves) every
        // changed loaded record, its errors nested under the
        // association (`lines.sku`); the emitted model validates the
        // records a plain has_many does.
        if let ModelBodyItem::Association {
            assoc: crate::dialect::Association::HasMany { name, autosave: true, skip_validation, .. },
            ..
        } = item
        {
            if seen.insert((item.span(), "has_many autosave".to_string(), String::new())) {
                let validates = if *skip_validation { "" } else { "validates and " };
                out.push(Diagnostic::unsupported(
                    item.span(),
                    None,
                    "`has_many autosave: true`",
                    format!(
                        "on `{}`: Rails {validates}saves every changed loaded record with the owner (errors nested under `{}.<attribute>`); the emitted model treats it as a plain has_many",
                        name.as_str(),
                        name.as_str()
                    ),
                ));
            }
            continue;
        }
        let ModelBodyItem::Validation { validation, .. } = item else { continue };
        for rule in &validation.rules {
            let ValidationRule::Unsupported { construct, detail } = rule else { continue };
            let span = item.span();
            if !seen.insert((span, construct.clone(), detail.clone())) {
                continue;
            }
            out.push(Diagnostic::unsupported(
                span,
                None,
                construct.as_str(),
                format!(
                    "on `{}`, {detail}; the emitted model would skip this check",
                    validation.attribute.as_str()
                ),
            ));
        }
    }
    // An STI subclass's own declarations (a library class to ingest).
    out.extend(crate::lower::sti_subclass_validations::diagnose(app));
    // `uniqueness:` queries the model's table: a class outside the
    // ActiveRecord hierarchy has none (ActiveModel has no
    // UniquenessValidator, and Rails raises at load).
    for model in app.models.iter().filter(|m| m.parent.is_none()) {
        for item in &model.body {
            let ModelBodyItem::Validation { validation, .. } = item else { continue };
            fn unwrap(r: &ValidationRule) -> &ValidationRule {
                match r {
                    ValidationRule::Guarded { rule, .. } => rule,
                    other => other,
                }
            }
            let tableless_unique = validation.rules.iter().any(|r| matches!(unwrap(r), ValidationRule::Uniqueness { .. }));
            if tableless_unique {
                out.push(Diagnostic::unsupported(
                    item.span(),
                    None,
                    "`validates uniqueness:`",
                    format!(
                        "on `{}`, a uniqueness check on a class outside ActiveRecord (ActiveModel has no UniquenessValidator); the emitted model would skip this check",
                        validation.attribute.as_str()
                    ),
                ));
            }
            // AssociatedValidator is ActiveRecord's too.
            if validation.rules.iter().any(|r| matches!(unwrap(r), ValidationRule::Associated)) {
                out.push(Diagnostic::unsupported(
                    item.span(),
                    None,
                    "`validates_associated`",
                    format!(
                        "on `{}`, on a class outside ActiveRecord (ActiveModel has no AssociatedValidator); the emitted model would skip this check",
                        validation.attribute.as_str()
                    ),
                ));
            }
        }
    }
    out
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

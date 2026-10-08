//! `private_constant :NAME` in a model body.
//!
//! Ruby makes the constant private to the class: a read with an explicit
//! scope (`Widget::LIMIT`, `::Widget::LIMIT`, `self.class::LIMIT`) raises
//! NameError ("private constant Widget::LIMIT referenced"), wherever it is
//! written, the class's own methods included; a lexical read (`LIMIT`
//! inside the class) is unaffected, as are `const_get` and the value.
//!
//! The lowered program reads a model's constants through the class
//! (`Widget::LIMIT` in a scope's relation method, a nested class
//! autoloaded from its own file), so replaying the marker would break
//! the reads Ruby allows. The emitted class does not run it; instead
//! every scoped read of a constant made private that the source writes
//! through the class's name (`Widget::LIMIT`,
//! `::Widget::LIMIT`) is refused here, where Ruby would raise. What
//! remains different: a read through an expression scope
//! (`self.class::LIMIT`) is not refused, and `Widget.constants` lists
//! the private names, which Ruby leaves out.
//!
//! A marker naming a constant the body does not define raises NameError
//! when the class loads, so it stays refused with that reason. A sorbet
//! type alias (`Name = T.type_alias { ... }`) is defined in Ruby though
//! the ingest drops it, and counts.

use crate::dialect::Model;
use crate::expr::{Expr, ExprNode, Literal};

/// The names a `private_constant` statement marks, when every argument
/// is a symbol or string literal; None for any other statement.
pub(crate) fn marked_names(expr: &Expr) -> Option<Vec<String>> {
    let ExprNode::Send { recv, method, args, block: None, .. } = &*expr.node else { return None };
    if method.as_str() != "private_constant" || args.is_empty() {
        return None;
    }
    if recv.as_ref().is_some_and(|r| !matches!(&*r.node, ExprNode::SelfRef)) {
        return None;
    }
    args.iter()
        .map(|a| match &*a.node {
            ExprNode::Lit { value: Literal::Sym { value } } => Some(value.as_str().to_string()),
            ExprNode::Lit { value: Literal::Str { value } } => Some(value.clone()),
            _ => None,
        })
        .collect()
}

/// The constants `model`'s body defines: its constant writes and the
/// classes and modules nested in it.
fn defined_names(model: &Model, app: &crate::App) -> Vec<String> {
    let prefix = format!("{}::", model.name.0.as_str());
    let mut names: Vec<String> =
        super::collect_model_constants(model).into_iter().map(|(name, _)| name.as_str().to_string()).collect();
    let nested = app
        .library_classes
        .iter()
        .map(|c| c.name.0.as_str())
        .chain(app.models.iter().map(|m| m.name.0.as_str()))
        .filter_map(|name| name.strip_prefix(prefix.as_str()))
        .filter(|rest| !rest.contains("::"))
        .map(str::to_string);
    names.extend(nested);
    names
}

/// The marker's names `model` does not define, or empty when it claims
/// the statement. `marker` is the statement, whose file is the body's.
pub(crate) fn undefined(model: &Model, app: &crate::App, marker: &Expr, names: &[String]) -> Vec<String> {
    let defined = defined_names(model, app);
    let source = source_text(&app.sources, marker.span);
    names.iter().filter(|n| !defined.contains(n) && !type_alias_written(source, n)).cloned().collect()
}

/// `Name = T.type_alias { ... }`: Ruby defines the constant (a sorbet
/// type alias object), which the ingest drops unless the sorbet check
/// reads it (`ingest::library_class::is_sorbet_type_alias`), so it is
/// looked for in the class's source.
fn type_alias_written(source: &str, name: &str) -> bool {
    source.lines().any(|line| {
        line.trim_start()
            .strip_prefix(name)
            .and_then(|rest| rest.trim_start().strip_prefix('='))
            .is_some_and(|value| value.trim_start().starts_with("T.type_alias"))
    })
}

fn source_text(sources: &[crate::span::SourceFile], span: crate::span::Span) -> &str {
    (span.file.0 as usize)
        .checked_sub(1)
        .and_then(|i| sources.get(i))
        .map(|source| source.text.as_str())
        .unwrap_or("")
}

/// Every scoped read in the app's source of a constant a model makes
/// private: an error where Ruby raises NameError.
pub(crate) fn scoped_reads(app: &crate::App) -> Vec<crate::diagnostic::Diagnostic> {
    let mut private: Vec<(String, String)> = Vec::new();
    for model in &app.models {
        for item in &model.body {
            let crate::dialect::ModelBodyItem::Unknown { expr, .. } = item else { continue };
            for name in marked_names(expr).unwrap_or_default() {
                private.push((model.name.0.as_str().to_string(), name));
            }
        }
    }
    if private.is_empty() {
        return Vec::new();
    }
    let sources: &[crate::span::SourceFile] = &app.sources;
    let mut out = Vec::new();
    let mut seen: Vec<crate::span::Span> = Vec::new();
    let mut visit = |root: &Expr| {
        fn walk(
            e: &Expr,
            private: &[(String, String)],
            sources: &[crate::span::SourceFile],
            seen: &mut Vec<crate::span::Span>,
            out: &mut Vec<crate::diagnostic::Diagnostic>,
        ) {
            if let ExprNode::Const { path } = &*e.node
                && let [scope @ .., last] = path.as_slice()
                && !scope.is_empty()
                && !e.span.is_synthetic()
            {
                let written = (e.span.file.0 as usize)
                    .checked_sub(1)
                    .and_then(|i| sources.get(i))
                    .and_then(|source| source.text.get(e.span.start as usize..e.span.end as usize))
                    .unwrap_or("");
                // As the source writes it: a lexical read the analysis
                // qualified is not a scoped read.
                let scoped = written.contains("::") && written.trim_end().ends_with(last.as_str());
                let scope: Vec<&str> = scope.iter().map(|s| s.as_str()).filter(|s| !s.is_empty()).collect();
                let scope = scope.join("::");
                if scoped
                    && let Some((owner, name)) = private.iter().find(|(owner, name)| {
                        name == last.as_str()
                            && (*owner == scope || owner.ends_with(&format!("::{scope}")))
                    })
                    && !seen.contains(&e.span)
                {
                    seen.push(e.span);
                    out.push(crate::diagnostic::Diagnostic::unsupported(
                        e.span,
                        None,
                        format!("{scope}::{name}").as_str(),
                        format!(
                            "`{owner}` makes `{name}` private (`private_constant :{name}`), so Ruby raises NameError \
                             (private constant {owner}::{name} referenced) on this scoped read; the emitted program \
                             does not enforce constant privacy"
                        ),
                    ));
                }
            }
            e.node.for_each_child(&mut |c| walk(c, private, sources, seen, out));
        }
        walk(root, &private, sources, &mut seen, &mut out);
    };
    crate::lower::for_each_hook_body_ref(app, &mut visit);
    for view in &app.views {
        visit(&view.body);
    }
    crate::analyze::runtime_reads::test_roots(app, &mut visit);
    out
}

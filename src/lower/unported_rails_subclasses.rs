//! A class extending a Rails base the runtime does not port cannot
//! load in any emitted tree — drop it after analysis, and say so.
//!
//! Rails autoloads every `app/*` subdirectory and so does ingest
//! (`support_roots`), which is how lobsters' `app/mailboxes/` arrived:
//! `ApplicationMailbox < ActionMailbox::Base` was carried as a library
//! class and replayed verbatim (the rule for a gem's DSL base), and the
//! emitted tree then raised `NameError: uninitialized constant
//! ActionMailbox` from `app/models.rb` — no spec ran. There is no
//! `ActionMailbox` in `runtime/ruby/`, and a stub base would make the
//! file load while pretending an inbound-email pipeline exists.
//!
//! After analysis, not at ingest: the mailbox's `process` is app code,
//! and `check`, the editor and the MCP server type it like any other
//! method — those doors run the analyzer without these lowerings and
//! keep the class. Only the emit-bound drivers reach here, and for them
//! the honest shape is no class and a `lower_residue` line naming the
//! base, on every target and in a strict run (the survey ledger is
//! `--survey`-only). Transitive, because `InboxMailbox <
//! ApplicationMailbox` names a parent that no longer exists. First in
//! the pass order, so no later pass ledgers residue for a body that is
//! not going to emit.
//!
//! Except a railtie or engine on a PostgreSQL app: the only tree that
//! emits one is the CRuby tree (`project::target_files` refuses every
//! other target for PostgreSQL), and that tree ports `Rails::Railtie`
//! and `Rails::Engine` (runtime/rails_railtie.rb, rails_engine.rb), so
//! the class is carried, its body replayed, and boot runs its
//! initializers (`App::railties`). Likewise an Active Job argument
//! serializer (`ActiveJob::Serializers::ObjectSerializer`): the CRuby
//! tree ports the registry (runtime/active_job_callbacks.rb), and Core's
//! MoneyBagJobArgumentSerializer and shopify-money's railtie register
//! theirs there.

use crate::det::HashMap;

use crate::app::App;
use crate::diagnostic::Diagnostic;
use crate::ident::Symbol;
use crate::ingest::library_class::is_unported_rails_base;
use crate::span::Span;

pub fn apply_unported_rails_subclass_drop(app: &mut App) -> Vec<Diagnostic> {
    // Each doomed class → the base at the top of its chain. A fixpoint
    // because `library_classes` is in file order, and a subclass's file
    // can sort before its parent's.
    let mut doomed: HashMap<Symbol, Symbol> = HashMap::default();
    let carries_railties =
        app.primary_database_adapter() == Some(&crate::database::DatabaseAdapter::Postgresql);
    let unported = |base: &str| {
        is_unported_rails_base(base)
            && !(carries_railties
                && (crate::ingest::library_class::is_railtie_base(base)
                    || base == "ActiveJob::Serializers::ObjectSerializer"))
    };
    loop {
        let mut grew = false;
        for lc in &app.library_classes {
            if doomed.contains_key(&lc.name.0) {
                continue;
            }
            let Some(parent) = lc.parent.as_ref() else { continue };
            let base = if unported(parent.0.as_str()) {
                Some(parent.0.clone())
            } else {
                doomed.get(&parent.0).cloned()
            };
            if let Some(base) = base {
                doomed.insert(lc.name.0.clone(), base);
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }
    if doomed.is_empty() {
        return Vec::new();
    }
    let mut diags = Vec::new();
    for lc in &app.library_classes {
        let Some(base) = doomed.get(&lc.name.0) else { continue };
        let via = match lc.parent.as_ref() {
            Some(p) if p.0 != *base => format!(" through `{}`", p.0.as_str()),
            _ => String::new(),
        };
        // A `LibraryClass` carries no span of its own; its first method
        // (or, for a Railtie, its first `initializer`) points the line at
        // the right file.
        let initializers = railtie_initializers(lc);
        let span = initializers
            .first()
            .map(|(_, span)| *span)
            .or_else(|| lc.methods.first().map(|m| m.name_span))
            .unwrap_or_else(Span::synthetic);
        // A gem's Railtie is how it configures itself at boot. Its
        // initializer blocks are named: none of them runs in the emitted
        // app, so whatever they set stays at the gem's own defaults.
        let not_run = if initializers.is_empty() {
            String::new()
        } else {
            let names: Vec<String> = initializers.iter().map(|(name, _)| format!("{name:?}")).collect();
            format!("; its initializers {} do not run at boot", names.join(", "))
        };
        diags.push(super::residue_diagnostic(
            "unported_rails_subclasses",
            lc.name.0.as_str(),
            span,
            "unported base",
            format!(
                "class dropped: `{}` extends `{}`{via}, a Rails base the runtime does not \
                 port — no emitted tree defines that constant, so the class cannot load{not_run}",
                lc.name.0.as_str(),
                base.as_str(),
            ),
        ));
    }
    app.library_classes.retain(|lc| !doomed.contains_key(&lc.name.0));
    diags
}

/// The `initializer "name" do … end` calls in a class body (a
/// `Rails::Railtie`'s or `Rails::Engine`'s boot hooks), with their spans.
fn railtie_initializers(lc: &crate::dialect::LibraryClass) -> Vec<(String, Span)> {
    use crate::expr::{ExprNode, Literal};
    lc.unknown_calls
        .iter()
        .filter_map(|call| match &*call.node {
            ExprNode::Send { recv: None, method, args, .. } if method.as_str() == "initializer" => {
                match args.first().map(|a| &*a.node) {
                    Some(ExprNode::Lit { value: Literal::Str { value } }) => Some((value.clone(), call.span)),
                    _ => None,
                }
            }
            _ => None,
        })
        .collect()
}


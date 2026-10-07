//! Project explicit keyword producer provenance only after the source
//! destination has been classified. Native full forwarders retain `**value`;
//! ordinary calls rejoin the existing keyword lowerings without a new carrier.

use crate::App;
use crate::analyze::forwarding::{KeywordPolicy, keyword_calls_and_nameless_rest, keyword_refusal};
use crate::diagnostic::Diagnostic;
use crate::expr::{Expr, ExprNode};

pub(super) fn apply(app: &mut App) -> Vec<Diagnostic> {
    let (plans, mut diagnostics) = keyword_calls_and_nameless_rest(app);
    for (span, policy) in &plans {
        if policy.refused() {
            diagnostics.push(keyword_refusal(*span, *policy));
        }
    }
    fn project(e: &mut Expr, plans: &crate::det::HashMap<crate::span::Span, KeywordPolicy>) {
        if plans.get(&e.span) == Some(&KeywordPolicy::NativeRuby) {
            // Kept as written. Every target but the Ruby family refuses it
            // by this mark, which a later reclassification cannot clear.
            e.decisions |= crate::expr::RUBY_NATIVE_KEYWORD_SPLAT;
        }
        if plans.get(&e.span) == Some(&KeywordPolicy::Legacy) {
            let args = match &mut *e.node {
                ExprNode::Send { args, .. } | ExprNode::Super { args: Some(args), .. } => Some(args),
                _ => None,
            };
            if let Some(args) = args {
                for arg in args {
                    if let ExprNode::KeywordSplat { value } = &mut *arg.node {
                        *arg = std::mem::replace(value, super::typing::nil_lit());
                        arg.decisions |= crate::expr::ERASED_KEYWORD_SPLAT;
                    }
                }
            }
        }
        e.node.for_each_child_mut(&mut |c| project(c, plans));
    }
    super::for_each_forwarding_body(app, &mut |e| project(e, &plans));
    diagnostics
}

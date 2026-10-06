//! A test class's `teardown` blocks and own `def teardown` become one
//! `teardown` method (`ingest::test`), and a subclass's method overrides
//! its base's. Rails runs the `def teardown` chain first, then every
//! class's blocks as callbacks, the subclass's before the base's. The
//! merged methods reproduce that only when at most one class in the
//! chain declares blocks, every class below it reaches it through a
//! final `super`, and it has a `def` of its own if a base def sits above
//! it. Anything else would silently skip or reorder cleanup, so it is
//! refused by name. With no blocks this is plain Ruby overriding, as in
//! Rails.

use std::collections::HashMap;

use crate::diagnostic::Diagnostic;
use crate::dialect::{MethodDef, TestModule};
use crate::expr::{Expr, ExprNode};
use crate::App;

fn teardown(tm: &TestModule) -> Option<&MethodDef> {
    tm.helpers.iter().find(|m| m.name.as_str() == "teardown")
}

fn ends_in_super(expr: &Expr) -> bool {
    match &*expr.node {
        ExprNode::Super { .. } => true,
        ExprNode::Seq { exprs } => exprs.last().is_some_and(ends_in_super),
        _ => false,
    }
}

pub(super) fn diagnose(app: &App) -> Vec<Diagnostic> {
    let by_name: HashMap<&str, &TestModule> = app.test_modules.iter().map(|tm| (tm.name.0.as_str(), tm)).collect();
    let mut out = Vec::new();
    for tm in &app.test_modules {
        let Some(own) = teardown(tm) else { continue };
        // The classes with a teardown, this one first, then up its bases.
        let mut chain: Vec<&TestModule> = vec![tm];
        let mut parent = tm.parent.as_ref().and_then(|p| by_name.get(p.0.as_str()));
        while let Some(base) = parent {
            if chain.len() > 32 {
                break;
            }
            if teardown(base).is_some() {
                chain.push(base);
            }
            parent = base.parent.as_ref().and_then(|p| by_name.get(p.0.as_str()));
        }
        let blocks: Vec<usize> = (0..chain.len()).filter(|&i| chain[i].teardown_blocks).collect();
        let reason = match blocks.as_slice() {
            [] => None,
            [k] => {
                let k = *k;
                if let Some(i) = (0..k).find(|&i| !teardown(chain[i]).is_some_and(|m| ends_in_super(&m.body))) {
                    Some(format!(
                        "{}'s teardown does not end by calling super, so {}'s teardown blocks would not run last as Rails runs them",
                        chain[i].name.0, chain[k].name.0))
                } else if !chain[k].teardown_def && k + 1 < chain.len() {
                    Some(format!(
                        "{}'s teardown blocks replace {}'s def teardown, which Rails would still run after each test",
                        chain[k].name.0, chain[k + 1].name.0))
                } else {
                    None
                }
            }
            [a, b, ..] => Some(format!(
                "{} and {} both declare teardown blocks; Rails runs them after every def teardown, which the merged teardown methods do not reproduce",
                chain[*a].name.0, chain[*b].name.0)),
        };
        if let Some(reason) = reason {
            out.push(Diagnostic::unsupported(own.name_span, None, "test_teardown_chain", reason));
        }
    }
    out
}

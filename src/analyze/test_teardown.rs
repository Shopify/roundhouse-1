//! A test class's `teardown` blocks become its `teardown` method
//! (`ingest::test`). Rails runs a base's teardown blocks as callbacks
//! after every subclass test, whatever the subclass declares; here a
//! subclass's `teardown` replaces the base's method, so the base's blocks
//! run only if every teardown below them reaches it through `super`
//! (and contributes no blocks of its own, whose order would then differ).
//! Anything else would silently skip or reorder the base's cleanup, so it
//! is refused by name. A plain `def teardown` over a plain one is Ruby
//! overriding, as in Rails.

use std::collections::HashMap;

use crate::diagnostic::Diagnostic;
use crate::dialect::{MethodDef, TestModule};
use crate::expr::{Expr, ExprNode};
use crate::App;

fn teardown(tm: &TestModule) -> Option<&MethodDef> {
    tm.helpers.iter().find(|m| m.name.as_str() == "teardown")
}

fn calls_super(expr: &Expr) -> bool {
    if matches!(&*expr.node, ExprNode::Super { .. }) {
        return true;
    }
    let mut found = false;
    expr.node.for_each_child(&mut |child| found |= calls_super(child));
    found
}

pub(super) fn diagnose(app: &App) -> Vec<Diagnostic> {
    let by_name: HashMap<&str, &TestModule> = app.test_modules.iter().map(|tm| (tm.name.0.as_str(), tm)).collect();
    let mut out = Vec::new();
    for tm in &app.test_modules {
        let Some(own) = teardown(tm) else { continue };
        // Whether the teardown run so far still reaches the next one up.
        let mut reaches = !tm.teardown_blocks && calls_super(&own.body);
        let mut parent = tm.parent.as_ref().and_then(|p| by_name.get(p.0.as_str()));
        let mut depth = 0;
        while let Some(base) = parent {
            if let Some(base_teardown) = teardown(base) {
                if base.teardown_blocks && !reaches {
                    out.push(Diagnostic::unsupported(own.name_span, None, "test_teardown_chain", format!(
                        "{}'s teardown replaces {}'s teardown blocks, which Rails would also run after each test",
                        tm.name.0, base.name.0)));
                    break;
                }
                reaches = reaches && !base.teardown_blocks && calls_super(&base_teardown.body);
            }
            depth += 1;
            if depth > 32 {
                break;
            }
            parent = base.parent.as_ref().and_then(|p| by_name.get(p.0.as_str()));
        }
    }
    out
}

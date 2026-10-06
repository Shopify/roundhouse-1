//! A test class's `teardown` blocks become its `teardown` method
//! (`ingest::test`), so one declared under a base that has its own
//! replaces the base's: the base cleanup silently stops running, where
//! Rails runs the child's and then the base's. Refused by name until the
//! chain is modeled.

use std::collections::HashMap;

use crate::diagnostic::Diagnostic;
use crate::dialect::TestModule;
use crate::App;

pub(super) fn diagnose(app: &App) -> Vec<Diagnostic> {
    let by_name: HashMap<&str, &TestModule> = app.test_modules.iter().map(|tm| (tm.name.0.as_str(), tm)).collect();
    let teardown = |tm: &TestModule| tm.helpers.iter().find(|m| m.name.as_str() == "teardown").map(|m| m.name_span);
    let mut out = Vec::new();
    for tm in &app.test_modules {
        let Some(span) = teardown(tm) else { continue };
        let mut parent = tm.parent.as_ref().and_then(|p| by_name.get(p.0.as_str()));
        let mut seen = 0;
        while let Some(base) = parent {
            if teardown(base).is_some() {
                out.push(Diagnostic::unsupported(span, None, "test_teardown_chain", format!(
                    "{}'s teardown replaces {}'s, which Rails would also run after each test",
                    tm.name.0, base.name.0)));
                break;
            }
            seen += 1;
            if seen > 32 { break; }
            parent = base.parent.as_ref().and_then(|p| by_name.get(p.0.as_str()));
        }
    }
    out
}

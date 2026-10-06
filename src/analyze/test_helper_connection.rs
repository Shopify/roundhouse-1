//! A test helper's own `establish_connection` (`ingest::test::
//! helper_connections`) is not carried into the emitted tree: the
//! harness opens one SQLite file database per test file
//! (`runtime/spinel/test/test_helper.rb`, `_test_db_path`), the shape
//! Rails' own tests use. So the emitted tests never run on the database
//! the helper names, and each call is named rather than dropped.
//!
//! A literal `adapter: "sqlite3"` is a warning: the tests still run on
//! SQLite, through the same driver, but on the harness's file and not
//! the helper's database (`:memory:` included), so a run is not the
//! helper's invocation. Any other adapter, or one the call does not
//! spell as a literal, is an error: running those tests on SQLite would
//! stand SQLite in for a database it is not.

use crate::diagnostic::{Diagnostic, Severity};
use crate::expr::{Expr, ExprNode, Literal};
use crate::App;

/// The call's options as `key: value` pairs, when it passes one hash
/// with symbol keys: a string or symbol value as its text, an integer
/// as written, anything else as `None`.
fn options(call: &Expr) -> Option<Vec<(String, Option<String>)>> {
    let ExprNode::Send { args, .. } = &*call.node else { return None };
    let [hash] = args.as_slice() else { return None };
    let ExprNode::Hash { entries, .. } = &*hash.node else { return None };
    entries
        .iter()
        .map(|(k, v)| {
            let ExprNode::Lit { value: Literal::Sym { value: key } } = &*k.node else { return None };
            let value = match &*v.node {
                ExprNode::Lit { value: Literal::Str { value } } => Some(format!("{value:?}")),
                ExprNode::Lit { value: Literal::Sym { value } } => Some(format!("{:?}", value.as_str())),
                ExprNode::Lit { value: Literal::Int { value } } => Some(value.to_string()),
                _ => None,
            };
            Some((key.as_str().to_string(), value))
        })
        .collect()
}

pub(super) fn diagnose(app: &App) -> Vec<Diagnostic> {
    app.test_helper_connections
        .iter()
        .map(|call| {
            let options = options(call);
            let adapter = options
                .as_ref()
                .and_then(|o| o.iter().find(|(k, _)| k == "adapter"))
                .and_then(|(_, v)| v.as_deref())
                .map(|v| v.trim_matches('"'));
            if adapter == Some("sqlite3") {
                let written = options
                    .as_ref()
                    .unwrap()
                    .iter()
                    .map(|(k, v)| format!("{k}: {}", v.as_deref().unwrap_or("…")))
                    .collect::<Vec<_>>()
                    .join(", ");
                let mut d = Diagnostic::unsupported(
                    call.span,
                    None,
                    "test_helper_connection",
                    format!(
                        "the test helper's `establish_connection({written})` is not carried: the emitted \
                         tests run on the harness's own SQLite file database (tmp/test/<test file>.sqlite3), \
                         not on this one"
                    ),
                );
                d.severity = Severity::Warning;
                d
            } else {
                let why = match adapter {
                    Some(other) => format!("adapter `{other}` is not modeled"),
                    None => "its adapter is not a literal the check can read".to_string(),
                };
                Diagnostic::unsupported(
                    call.span,
                    None,
                    "test_helper_connection",
                    format!(
                        "the test helper's `establish_connection` is not supported: {why}, and the emitted \
                         tests would run on SQLite in its place"
                    ),
                )
            }
        })
        .collect()
}

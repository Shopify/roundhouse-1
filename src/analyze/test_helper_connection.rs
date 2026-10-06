//! A test helper's own `establish_connection` (`ingest::test::
//! helper_connections`) names the database its tests run on. The
//! emitted harness otherwise opens one SQLite file database per test
//! file (`runtime/spinel/test/test_helper.rb`, `_test_db_path`), so a
//! connection it cannot open is refused, never replaced by that file.
//!
//! One form is opened as written (`honored_database`): a single
//! `ActiveRecord::Base.establish_connection(adapter: "sqlite3",
//! database: ":memory:")` the helper runs as it loads, in an app with
//! no schema of its own. Rails' sqlite3 adapter gives each connection,
//! one per thread, its own empty in-memory database with the adapter's
//! default pragmas; the CRuby tree's `Db` opens exactly that
//! (`runtime/spinel/db_cruby.rb`, memory mode) and `project` points the
//! harness at it. Every other target refuses the call at emit.
//!
//! Everything else is an error: another adapter or database, an option
//! the check cannot read or the harness does not model, a call made
//! under a condition or in a method, more than one call, and an app
//! whose schema the harness would load into a database the helper
//! leaves empty.

use crate::diagnostic::Diagnostic;
use crate::expr::{Expr, ExprNode, Literal};
use crate::App;

/// The receiver is `ActiveRecord::Base`, plain or rooted.
fn on_base(call: &Expr) -> bool {
    let ExprNode::Send { recv: Some(recv), .. } = &*call.node else { return false };
    let ExprNode::Const { path } = &*recv.node else { return false };
    path.iter().map(|s| s.as_str()).skip_while(|s| s.is_empty()).eq(["ActiveRecord", "Base"])
}

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
                ExprNode::Lit { value: Literal::Str { value } } => Some(value.clone()),
                ExprNode::Lit { value: Literal::Sym { value } } => Some(value.as_str().to_string()),
                ExprNode::Lit { value: Literal::Int { value } } => Some(value.to_string()),
                _ => None,
            };
            Some((key.as_str().to_string(), value))
        })
        .collect()
}

/// Why the helper's connections cannot be opened as written, or `None`
/// when they can (or when the helper makes none).
fn refusal(app: &App) -> Option<String> {
    let connection = match app.test_helper_connections.as_slice() {
        [] => return None,
        [connection] => connection,
        _ => return Some("the helper connects more than once".to_string()),
    };
    if !connection.top_level {
        return Some(
            "the call is not a statement the helper runs as it loads (it is under a condition, \
             in a class body or `begin`, or in a method)"
                .to_string(),
        );
    }
    if !on_base(&connection.call) {
        return Some("its receiver is not `ActiveRecord::Base`".to_string());
    }
    let Some(options) = options(&connection.call) else {
        return Some("its options are not a hash the check can read".to_string());
    };
    if let Some((key, _)) = options.iter().find(|(k, _)| k != "adapter" && k != "database") {
        return Some(format!("option `{key}` is not modeled"));
    }
    let value = |key: &str| options.iter().find(|(k, _)| k == key).and_then(|(_, v)| v.as_deref());
    match value("adapter") {
        Some("sqlite3") => {}
        Some(other) => return Some(format!("adapter `{other}` is not modeled")),
        None => return Some("its adapter is not a literal the check can read".to_string()),
    }
    match value("database") {
        Some(":memory:") => {}
        Some(other) => return Some(format!("database `{other}` is not modeled (only `:memory:` is)")),
        None => return Some("its database is not a literal the check can read".to_string()),
    }
    if !app.schema.tables.is_empty() {
        return Some(
            "the harness would load the app's schema into the in-memory database the helper \
             leaves empty"
                .to_string(),
        );
    }
    None
}

/// The database the emitted harness opens for the helper's own
/// connection, when the helper makes the one connection it can open.
pub fn honored_database(app: &App) -> Option<&'static str> {
    (!app.test_helper_connections.is_empty() && refusal(app).is_none()).then_some(":memory:")
}

pub(super) fn diagnose(app: &App) -> Vec<Diagnostic> {
    let Some(why) = refusal(app) else { return Vec::new() };
    app.test_helper_connections
        .iter()
        .map(|connection| {
            Diagnostic::unsupported(
                connection.call.span,
                None,
                "test_helper_connection",
                format!(
                    "the test helper's `establish_connection` is not supported: {why}; the emitted \
                     tests would run on another database in its place"
                ),
            )
        })
        .collect()
}

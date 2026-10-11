//! The `initializer` blocks an application class body registers.
//!
//! ```ruby
//! module Sample
//!   class Application < Rails::Application
//!     initializer(:configure_asset_host, after: :set_autoload_paths) do |app|
//!       config.asset_host = AssetHostBuilder.build(…)
//!     end
//!   end
//! end
//! ```
//!
//! `Rails::Initializable::ClassMethods#initializer` (railties 8.1.4
//! initializable.rb) records the block; `Application#initialize!` sorts
//! every railtie's, every engine's and the application's by `before:` /
//! `after:` and runs each with the application as `self` and as its
//! argument. The block writes config keys other code reads
//! (`Rails.application.config.merchant_asset_host`) that no
//! `config.k = v` line of the body does, so lifting only those left the
//! key unset.
//!
//! The statement is carried whole, as written, on the `Rails::Application`
//! reopen's class body: the reopen's `initializer` call registers it, and
//! the railtie runtime runs it among the other initializers
//! (`Rails::Engine.application_initializers`). A statement that does not
//! ingest as app code is returned with the reason instead, so the caller
//! reports it; nothing is dropped silently.

use ruby_prism::Node;

use crate::expr::Expr;

use super::util::{constant_id_str, constant_path_of, flatten_statements};

/// The carried `initializer` statements of every `class X <
/// Rails::Application` body in `source`, in source order, and the ones
/// that could not be carried with their span and reason.
pub(super) fn extract(source: &[u8], file: &str) -> (Vec<Expr>, Vec<(usize, usize, String)>) {
    let result = super::prism::parse(source, file);
    let root = result.node();
    let mut carried = Vec::new();
    let mut refused = Vec::new();
    if let Some(program) = root.as_program_node() {
        walk(program.statements().body().iter().collect(), file, &mut carried, &mut refused);
    }
    (carried, refused)
}

fn walk(stmts: Vec<Node<'_>>, file: &str, carried: &mut Vec<Expr>, refused: &mut Vec<(usize, usize, String)>) {
    for stmt in stmts {
        if let Some(module) = stmt.as_module_node() {
            walk(module.body().map(flatten_statements).unwrap_or_default(), file, carried, refused);
            continue;
        }
        let Some(class) = stmt.as_class_node() else { continue };
        if !is_application_class(&class) {
            walk(class.body().map(flatten_statements).unwrap_or_default(), file, carried, refused);
            continue;
        }
        for statement in class.body().map(flatten_statements).unwrap_or_default() {
            let Some(call) = statement.as_call_node() else { continue };
            if call.receiver().is_some() || constant_id_str(&call.name()) != "initializer" {
                continue;
            }
            let loc = statement.location();
            if call.block().and_then(|b| b.as_block_node()).is_none() {
                refused.push((loc.start_offset(), loc.end_offset(), "an `initializer` without a block raises in Rails".into()));
                continue;
            }
            let before = super::survey::recorded_len();
            match super::expr::ingest_expr(&statement, file) {
                Ok(expr) => match super::survey::recorded_since(before) {
                    Some(gap) => refused.push((
                        loc.start_offset(),
                        loc.end_offset(),
                        format!("the initializer body does not ingest as app code: {gap}"),
                    )),
                    None => carried.push(expr),
                },
                Err(err) => refused.push((
                    loc.start_offset(),
                    loc.end_offset(),
                    format!("the initializer body does not ingest as app code: {err}"),
                )),
            }
        }
    }
}

fn is_application_class(class: &ruby_prism::ClassNode<'_>) -> bool {
    class
        .superclass()
        .and_then(|parent| constant_path_of(&parent))
        .is_some_and(|path| path.join("::") == "Rails::Application")
}

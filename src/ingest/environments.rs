//! `config/environments/<env>.rb`: the per-environment `config.eager_load`.
//!
//! Rails decides at boot whether to load every autoloadable file
//! (`Rails::Application::Finisher`'s `eager_load!`, railties 8.1.4
//! finisher.rb:76) or to load each one when its constant is first
//! referenced. Core writes `config.eager_load = BootLib.test_eager_load?`
//! in test.rb: lazy on a local run, eager on main and in full CI. The
//! emitted app makes the same choice from the same expression, so each
//! environment's assignment is read here as app code.
//!
//! An assignment the emitted app cannot evaluate on its own is kept as
//! a refusal naming why, and the emitted boot raises it when that
//! environment is selected: under a condition (`if ENV[…]` around the
//! write), or reading a local the environment file computed earlier
//! (Core's development.rb: `config.eager_load = eager_load`).

use std::path::Path;

use crate::app::EagerLoadSetting;
use crate::vfs::Vfs;

pub(super) fn ingest_eager_load<V: Vfs + ?Sized>(vfs: &V, dir: &Path) -> Vec<EagerLoadSetting> {
    let env_dir = dir.join("config/environments");
    if !vfs.is_dir(&env_dir) {
        return Vec::new();
    }
    let Ok(mut files) = super::app::read_rb_files(vfs, &env_dir) else {
        return Vec::new();
    };
    files.sort();
    let mut out = Vec::new();
    for path in files {
        // Only `config/environments/<env>.rb`, not a nested helper.
        if path.parent() != Some(env_dir.as_path()) {
            continue;
        }
        let Some(env) = path.file_stem().and_then(|s| s.to_str()) else { continue };
        let Ok(source) = vfs.read(&path) else { continue };
        let file = path.display().to_string();
        super::sources::register(&file, &String::from_utf8_lossy(&source));
        if let Some(value) = read_setting(&source, &file) {
            out.push(EagerLoadSetting { env: env.to_string(), value });
        }
    }
    out
}

fn read_setting(source: &[u8], file: &str) -> Option<Result<crate::expr::Expr, String>> {
    let result = super::prism::parse(source, file);
    let root = result.node();
    let stmts = root
        .as_program_node()
        .map(|p| p.statements().body().iter().collect::<Vec<_>>())
        .unwrap_or_default();
    let mut found: Option<Result<crate::expr::Expr, String>> = None;
    super::app::walk_config_statements(stmts, None, &mut |stmt, app_param| {
        if let Some(value) = direct_assignment(stmt, app_param) {
            found = Some(if reads_local(&value) {
                Err(format!(
                    "`config.eager_load = {}` reads a local of {file}",
                    slice(source, &value)
                ))
            } else {
                super::expr::ingest_expr(&value, file).map_err(|err| err.to_string())
            });
            return;
        }
        // A write anywhere else in the statement: under a condition or
        // in a block, so whether it runs is not this file's top level.
        if writes_eager_load(stmt) {
            let line = 1 + source[..stmt.location().start_offset()].iter().filter(|b| **b == b'\n').count();
            found = Some(Err(format!("`config.eager_load` is assigned under a condition at {file}:{line}")));
        }
    });
    found
}

/// `config.eager_load = <value>` as a statement of the configure block.
fn direct_assignment<'pr>(
    stmt: &ruby_prism::Node<'pr>,
    app_param: Option<&str>,
) -> Option<ruby_prism::Node<'pr>> {
    let call = stmt.as_call_node()?;
    if super::util::constant_id_str(&call.name()) != "eager_load=" {
        return None;
    }
    let path = super::app::config_path_in(&call.receiver()?, 0, app_param)?;
    if !path.is_empty() {
        return None;
    }
    call.arguments()?.arguments().iter().next()
}

/// A `….eager_load = …` call anywhere inside `node`.
fn writes_eager_load(node: &ruby_prism::Node<'_>) -> bool {
    struct Writes(bool);
    impl<'pr> ruby_prism::Visit<'pr> for Writes {
        fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
            if super::util::constant_id_str(&node.name()) == "eager_load=" {
                self.0 = true;
            }
            ruby_prism::visit_call_node(self, node);
        }
    }
    let mut v = Writes(false);
    ruby_prism::Visit::visit(&mut v, node);
    v.0
}

fn reads_local(node: &ruby_prism::Node<'_>) -> bool {
    struct Locals(bool);
    impl<'pr> ruby_prism::Visit<'pr> for Locals {
        fn visit_local_variable_read_node(&mut self, _: &ruby_prism::LocalVariableReadNode<'pr>) {
            self.0 = true;
        }
    }
    let mut v = Locals(false);
    ruby_prism::Visit::visit(&mut v, node);
    v.0
}

fn slice<'a>(source: &'a [u8], node: &ruby_prism::Node<'_>) -> std::borrow::Cow<'a, str> {
    let loc = node.location();
    String::from_utf8_lossy(&source[loc.start_offset()..loc.end_offset()])
}

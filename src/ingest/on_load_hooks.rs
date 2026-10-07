//! `ActiveSupport.on_load(:hook) do … end` at the top of a boot
//! initializer, run where Rails runs it.
//!
//! Rails registers the block when the initializer runs and executes it
//! when the framework calls `ActiveSupport.run_load_hooks(:hook, base)`
//! (activesupport 8.1.4 lazy_load_hooks.rb): `base.class_eval(&block)`
//! for a Module base, so `self` inside the block is `base`. A hook whose
//! base was already loaded runs at registration.
//!
//! WHICH HOOKS. Only a hook the emitted runtime fires is lifted
//! ([`FIRED_HOOKS`]). `:active_record_postgresqladapter` fires when
//! Active Record resolves the adapter class for the primary
//! (activerecord 8.1.4 postgresql_adapter.rb:1210, reached from
//! `establish_connection` → `resolve_pool_config` → `db_config.validate!`
//! → `adapter_class`), before any connection is opened. The PostgreSQL
//! tree's `DatabaseConfiguration.configure!` is that establish
//! (`project::apply_on_load_hooks`), so the hook is lifted only when the
//! app is compiled for PostgreSQL. Any other hook stays an
//! `initializer_unsupported` gap, as before.
//!
//! WHAT IS LIFTED. The block's statements, ingested as app code with
//! their real spans, become one method on the `Rails::Application`
//! reopen, `__on_load_<hook>_<n>(base)`, `n` counting hooks in boot
//! order. `self` in the body reads the parameter (the block's own
//! parameter name when it declares one), which is what `class_eval`
//! gives the block. Statement by statement:
//!
//! * `require "x"`, anywhere in the statement: a runtime port
//!   ([`RUNTIME_PORTS`]) compiles to `require_relative` of the port, from
//!   the reopen's emitted file (config/application.rb); a lib file boot
//!   already loads is a no-op (`nil`), as at top level. Any other
//!   require refuses the statement.
//! * a `prepend` / `include` / `extend` call refuses the statement: the
//!   runtime has no framework modules to mix into, so the mixin cannot
//!   be applied ([`MIXIN_TARGETS`] gives the reason per target).
//! * a call with an implicit receiver (other than `require`) refuses the
//!   statement: in Rails it resolves on the adapter class, and the
//!   lifted method's implicit receiver is the application.
//! * `def`, `class`, `module`, `yield`, `return`, a block-local `self`
//!   rebinding (`instance_eval`, `class_eval`) refuse the statement.
//!
//! A kept statement is consumed; a refused one is not, so
//! `initializers::survey` descends into the hook and reports each
//! refused statement as its own `initializer_unsupported` gap with the
//! reason. The rest of the hook runs without it.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use ruby_prism::Node;

use crate::app::LoadHook;
use crate::dialect::MethodDef;
use crate::expr::{Expr, ExprNode, Literal};
use crate::ident::{Symbol, VarId};
use crate::vfs::Vfs;

use super::util::{constant_id_str, constant_path_segments_strs, flatten_statements};

/// Load hooks the emitted runtime fires, with the base it passes.
pub const FIRED_HOOKS: &[(&str, &str)] =
    &[("active_record_postgresqladapter", "ActiveRecord::ConnectionAdapters::PostgreSQLAdapter")];

/// `require` names that load a runtime port instead of a gem file, and
/// the port's stem under the emitted `runtime/`.
pub const RUNTIME_PORTS: &[(&str, &str)] =
    &[("semian/activerecord_postgresql_adapter", "semian_activerecord_postgresql_adapter")];

/// Mixin targets a hook names that the runtime does not carry, with why
/// the mixin has nothing to apply to.
const MIXIN_TARGETS: &[(&str, &str)] = &[(
    "ActiveRecord::ConnectionAdapters::PostgreSQL::SchemaStatements",
    "no schema reflection in the runtime (columns come from the compiled schema); \
     Rails 8.1.4 reaches new_column_from_field only from `columns` (abstract/schema_statements.rb:112)",
)];

/// Lift every top-level `ActiveSupport.on_load(:<fired hook>)` block of
/// `files` (boot order) into a method on the Application reopen.
pub(super) fn lift<V: Vfs + ?Sized>(
    vfs: &V,
    files: &[PathBuf],
    load_path: &[PathBuf],
    loaded_support: &HashSet<PathBuf>,
    methods: &mut Vec<MethodDef>,
) -> Vec<LoadHook> {
    let mut lifted = Vec::new();
    for entry in files {
        let Ok(bytes) = vfs.read(entry) else { continue };
        let file = entry.display().to_string();
        super::sources::register(&file, &String::from_utf8_lossy(&bytes));
        let result = super::prism::parse(&bytes, &file);
        let root = result.node();
        let Some(program) = root.as_program_node() else { continue };
        for stmt in program.statements().body().iter() {
            let Some((hook, param, body)) = on_load_block(&stmt) else { continue };
            if !FIRED_HOOKS.iter().any(|(h, _)| *h == hook) {
                continue;
            }
            let name = format!("__on_load_{hook}_{}", lifted.len());
            let mut kept: Vec<Expr> = Vec::new();
            for inner in body {
                let (start, end) = (inner.location().start_offset(), inner.location().end_offset());
                if let Some(reason) = refusal(&inner, &param) {
                    super::initializers::refuse(&file, start, end, reason);
                    continue;
                }
                let mut expr = match super::expr::ingest_expr(&inner, &file) {
                    Ok(expr) => expr,
                    Err(err) => {
                        super::initializers::refuse(
                            &file,
                            start,
                            end,
                            format!("on_load(:{hook}) statement does not ingest as app code: {err}"),
                        );
                        continue;
                    }
                };
                match rewrite(&mut expr, &param, load_path, loaded_support, vfs) {
                    Ok(()) => {
                        kept.push(expr);
                        super::initializers::consume(&file, start, end);
                    }
                    Err(reason) => super::initializers::refuse(&file, start, end, reason),
                }
            }
            let Ok(mut synth) = crate::runtime_src::parse_methods(&format!("def {name}({param})\n  nil\nend\n"))
            else {
                continue;
            };
            let Some(mut method) = synth.pop() else { continue };
            if !kept.is_empty() {
                let span = kept[0].span;
                method.body = Expr::new(span, ExprNode::Seq { exprs: kept });
            }
            // The `on_load` call itself is not consumed: the survey
            // descends into its block, where each kept statement is.
            methods.push(method);
            lifted.push(LoadHook { hook: hook.clone(), method: name.clone(), file: file.clone() });
        }
    }
    lifted
}

/// `ActiveSupport.on_load(:hook) do |param| … end` (no options): the
/// hook, the parameter name the body's `self` maps to, and the body.
fn on_load_block<'pr>(stmt: &Node<'pr>) -> Option<(String, String, Vec<Node<'pr>>)> {
    let call = stmt.as_call_node()?;
    if constant_id_str(&call.name()) != "on_load" {
        return None;
    }
    let receiver = call.receiver()?;
    if constant_path_segments_strs(&receiver)?.join("::") != "ActiveSupport" {
        return None;
    }
    let args: Vec<Node<'pr>> = call.arguments()?.arguments().iter().collect();
    let [arg] = args.as_slice() else { return None };
    let hook = String::from_utf8_lossy(arg.as_symbol_node()?.unescaped()).into_owned();
    let block = call.block()?.as_block_node()?;
    let param = match block.parameters().and_then(|p| p.as_block_parameters_node()) {
        None => "__on_load_base".to_string(),
        Some(params) => {
            let required: Vec<Node<'pr>> =
                params.parameters()?.requireds().iter().collect();
            let [one] = required.as_slice() else { return None };
            constant_id_str(&one.as_required_parameter_node()?.name()).to_string()
        }
    };
    let body = block.body().map(flatten_statements).unwrap_or_default();
    Some((hook, param, body))
}

/// Why `stmt` cannot run inside the lifted method, if it cannot.
fn refusal(stmt: &Node<'_>, param: &str) -> Option<String> {
    struct Scan {
        reason: Option<String>,
    }
    impl<'pr> ruby_prism::Visit<'pr> for Scan {
        fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
            if self.reason.is_some() {
                return;
            }
            let name = constant_id_str(&node.name());
            if matches!(name, "prepend" | "include" | "extend") {
                let target = node
                    .receiver()
                    .and_then(|r| constant_path_segments_strs(&r))
                    .map(|p| p.join("::"))
                    .unwrap_or_else(|| "self".to_string());
                let why = MIXIN_TARGETS
                    .iter()
                    .find(|(t, _)| *t == target)
                    .map(|(_, why)| why.to_string())
                    .unwrap_or_else(|| "the runtime applies no mixin from a load hook".to_string());
                let short = target.rsplit("::").next().unwrap_or(&target).to_string();
                self.reason = Some(format!("`{name}` into {short} is not applied: {why} (target {target})"));
                return;
            }
            if matches!(name, "instance_eval" | "class_eval" | "module_eval" | "instance_exec" | "class_exec") {
                self.reason = Some(format!("`{name}` rebinds self inside the load hook"));
                return;
            }
            if node.receiver().is_none() && !node.is_variable_call() && name != "require" {
                self.reason = Some(format!(
                    "`{name}` has an implicit receiver, the hook's base class in Rails; the lifted hook cannot send it there"
                ));
                return;
            }
            ruby_prism::visit_call_node(self, node);
        }
        fn visit_def_node(&mut self, _: &ruby_prism::DefNode<'pr>) {
            self.reason.get_or_insert_with(|| "`def` inside the load hook defines on the base class".into());
        }
        fn visit_class_node(&mut self, _: &ruby_prism::ClassNode<'pr>) {
            self.reason.get_or_insert_with(|| "a class body inside the load hook".into());
        }
        fn visit_module_node(&mut self, _: &ruby_prism::ModuleNode<'pr>) {
            self.reason.get_or_insert_with(|| "a module body inside the load hook".into());
        }
        fn visit_yield_node(&mut self, _: &ruby_prism::YieldNode<'pr>) {
            self.reason.get_or_insert_with(|| "`yield` inside the load hook".into());
        }
        fn visit_return_node(&mut self, _: &ruby_prism::ReturnNode<'pr>) {
            self.reason.get_or_insert_with(|| "`return` inside the load hook".into());
        }
    }
    let _ = param;
    let mut scan = Scan { reason: None };
    ruby_prism::Visit::visit(&mut scan, stmt);
    scan.reason
}

/// `self` → the hook parameter; `require` → port / no-op, or the reason
/// the statement cannot keep it.
fn rewrite<V: Vfs + ?Sized>(
    expr: &mut Expr,
    param: &str,
    load_path: &[PathBuf],
    loaded_support: &HashSet<PathBuf>,
    vfs: &V,
) -> Result<(), String> {
    let mut failure: Option<String> = None;
    walk(expr, &mut |e| {
        if failure.is_some() {
            return;
        }
        if matches!(&*e.node, ExprNode::SelfRef) {
            *e.node = ExprNode::Var { id: VarId(0), name: Symbol::from(param) };
            return;
        }
        let ExprNode::Send { recv: None, method, args, block: None, .. } = &mut *e.node else { return };
        if method.as_str() != "require" {
            return;
        }
        let feature = match args.as_slice() {
            [arg] => match &*arg.node {
                ExprNode::Lit { value: Literal::Str { value } } => value.clone(),
                _ => {
                    failure = Some("`require` of a computed name inside the load hook".into());
                    return;
                }
            },
            _ => {
                failure = Some("`require` with other than one argument".into());
                return;
            }
        };
        if let Some((_, stem)) = RUNTIME_PORTS.iter().find(|(name, _)| *name == feature) {
            // The reopen is emitted at config/application.rb.
            *method = Symbol::from("require_relative");
            if let ExprNode::Lit { value: Literal::Str { value } } = &mut *args[0].node {
                *value = format!("../runtime/{stem}");
            }
            return;
        }
        let file = if feature.ends_with(".rb") { feature.clone() } else { format!("{feature}.rb") };
        let target: Option<PathBuf> = load_path.iter().map(|lib| lib.join(&file)).find(|p| vfs.exists(p));
        if target.as_ref().is_some_and(|t| loaded_support.contains(t)) {
            *e.node = ExprNode::Lit { value: Literal::Nil };
            return;
        }
        failure = Some(match target {
            Some(t) => format!(
                "`require {feature:?}` loads {}, which boot does not load as support code",
                relative(&t)
            ),
            None => format!("`require {feature:?}` is neither a runtime port nor a lib file of the tree"),
        });
    });
    match failure {
        Some(reason) => Err(reason),
        None => Ok(()),
    }
}

fn walk(expr: &mut Expr, f: &mut impl FnMut(&mut Expr)) {
    f(expr);
    expr.node.for_each_child_mut(&mut |child| walk(child, f));
}

fn relative(path: &Path) -> String {
    path.display().to_string()
}

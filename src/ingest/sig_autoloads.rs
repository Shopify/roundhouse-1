//! What a sorbet-runtime `sig` does when its method first runs.
//!
//! `sig { … }` is an annotation to the analyzer (`sorbet_sig` reads it as
//! a signature seed) and is not carried as code. At runtime it is not
//! inert: sorbet-runtime wraps the method it precedes, and the method's
//! first call runs the block (`T::Private::Methods.run_sig`), so every
//! constant the block names is read then, which autoloads its file.
//!
//! Core depends on that. hedwig 1.68.0's lib/hedwig.rb has
//!
//! ```ruby
//! sig { returns(Hedwig::Configuration) }
//! def configuration
//!   @configuration || Kernel.raise(NotConfiguredError)
//! end
//! ```
//!
//! and `Hedwig::Configuration` is an `autoload` whose file ends with
//! `ActiveSupport.run_load_hooks(:hedwig, Hedwig)`. Core's
//! background_queue/job.rb reads `Hedwig.configuration.queues` in its class
//! body; that first call loads configuration.rb, whose hook runs Core's
//! `on_load(:hedwig) { … Hedwig.configure … }` (background_queue.rb), and
//! only then does the body read `@configuration`. Without the sig's read
//! the emitted call raised `Hedwig::NotConfiguredError` (unit-1000 r7c: 806
//! of 935 setups).
//!
//! So each library-class method a `sig` precedes begins by reading, in
//! the sig's order, the constants of the tree that the block names
//! (`_ = Hedwig::Configuration`), each resolved as Ruby's lexical lookup
//! would from the `def`'s scopes. A constant the tree does not carry
//! (`String`, `T::Boolean`, a gem's) has no file of the tree to load and
//! is not read. After the first call the reads load nothing.

use crate::ingest::prism_api::PrismLocation;
use ruby_prism::{Node, Visit};

use crate::det::{HashMap, HashSet};
use crate::dialect::LibraryClass;
use crate::expr::{Expr, ExprNode, LValue};
use crate::ident::{Symbol, VarId};
use crate::span::{SourceFile, Span};

use super::util::{class_name_path, constant_id_str, constant_path_segments_strs, module_name_path};

/// Prefix each `sig`-preceded method of `classes` with reads of the
/// constants of `known` its sig names.
pub(super) fn carry_sig_autoloads(classes: &mut [LibraryClass], sources: &[SourceFile], known: &HashSet<String>) {
    let mut reads: HashMap<(u32, u32), Vec<String>> = HashMap::default();
    for (index, source) in sources.iter().enumerate() {
        if !source.path.ends_with(".rb") || !(source.text.contains("sig {") || source.text.contains("sig do")) {
            continue;
        }
        let parsed = ruby_prism::parse(source.text.as_bytes());
        let file = index as u32 + 1;
        let node = parsed.node();
        if let Some(program) = node.as_program_node() {
            walk(&program.statements().as_node(), &[], file, known, &mut reads);
        }
    }
    if reads.is_empty() {
        return;
    }
    for lc in classes.iter_mut() {
        for method in &mut lc.methods {
            let at = method.name_span;
            let Some(names) = reads.get(&(at.file.0, at.start)) else { continue };
            let span = Span::synthetic();
            let mut exprs: Vec<Expr> = names
                .iter()
                .map(|name| {
                    let path = name.split("::").map(Symbol::from).collect();
                    let value = Expr::new(span, ExprNode::Const { path });
                    Expr::new(span, ExprNode::Assign { target: LValue::Var { id: VarId(0), name: Symbol::from("_") }, value })
                })
                .collect();
            let body = std::mem::replace(&mut method.body, Expr::new(span, ExprNode::Seq { exprs: Vec::new() }));
            let body_span = body.span;
            match *body.node {
                ExprNode::Seq { exprs: rest } => exprs.extend(rest),
                node => exprs.push(Expr::new(body_span, node)),
            }
            method.body = Expr::new(body_span, ExprNode::Seq { exprs });
        }
    }
}

/// The statements of one body: a `sig` applies to the method the next
/// statement defines (`def`, or `private def …`).
fn walk(node: &Node<'_>, scope: &[String], file: u32, known: &HashSet<String>, out: &mut HashMap<(u32, u32), Vec<String>>) {
    let Some(stmts) = node.as_statements_node() else { return };
    let mut pending: Option<Vec<Vec<String>>> = None;
    for stmt in stmts.body().iter() {
        if let Some(call) = stmt.as_call_node()
            && call.receiver().is_none()
            && constant_id_str(&call.name()) == "sig"
            && let Some(block) = call.block().and_then(|b| b.as_block_node())
        {
            let mut named = Named::default();
            named.visit_block_node(&block);
            pending = Some(named.paths);
            continue;
        }
        let sig = pending.take();
        if let Some(def) = defined(&stmt) {
            if let Some(paths) = sig {
                let resolved = resolve(&paths, scope, known);
                if !resolved.is_empty() {
                    out.insert((file, def.name_loc().start_offset() as u32), resolved);
                }
            }
            continue;
        }
        let nested = if let Some(class) = stmt.as_class_node() {
            class_name_path(&class).map(|path| (path, class.body()))
        } else if let Some(module) = stmt.as_module_node() {
            module_name_path(&module).map(|path| (path, module.body()))
        } else if let Some(singleton) = stmt.as_singleton_class_node() {
            singleton.body().map(|body| {
                walk(&body, scope, file, known, out);
            });
            None
        } else {
            None
        };
        if let Some((path, Some(body))) = nested {
            let mut inner = scope.to_vec();
            inner.extend(path);
            walk(&body, &inner, file, known, out);
        }
    }
}

/// The `def` a statement runs: itself, or the argument of a call that
/// returns its name (`private def x`, `module_function def x`).
fn defined<'pr>(stmt: &Node<'pr>) -> Option<ruby_prism::DefNode<'pr>> {
    if let Some(def) = stmt.as_def_node() {
        return Some(def);
    }
    let call = stmt.as_call_node()?;
    if call.receiver().is_some() {
        return None;
    }
    let args = call.arguments()?;
    let mut args = args.arguments().iter();
    let first = args.next()?;
    if args.next().is_some() {
        return None;
    }
    first.as_def_node()
}

/// Each written path, resolved from the innermost scope outward; one
/// that names no class or module of the tree is left out. Sorbet's own
/// `T::…` is never the tree's.
fn resolve(paths: &[Vec<String>], scope: &[String], known: &HashSet<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for path in paths {
        if path.first().is_none_or(|head| head == "T") {
            continue;
        }
        let found = (0..=scope.len()).rev().find_map(|depth| {
            let name = scope[..depth].iter().chain(path.iter()).cloned().collect::<Vec<_>>().join("::");
            known.contains(&name).then_some(name)
        });
        if let Some(name) = found
            && !out.contains(&name)
        {
            out.push(name);
        }
    }
    out
}

/// The outermost constant paths a block names, in source order.
#[derive(Default)]
struct Named {
    paths: Vec<Vec<String>>,
}

impl<'pr> Visit<'pr> for Named {
    fn visit_constant_read_node(&mut self, node: &ruby_prism::ConstantReadNode<'pr>) {
        self.paths.push(vec![constant_id_str(&node.name()).to_string()]);
    }

    fn visit_constant_path_node(&mut self, node: &ruby_prism::ConstantPathNode<'pr>) {
        if let Some(path) = constant_path_segments_strs(&node.as_node()) {
            self.paths.push(path);
        }
    }
}

//! App config an in-repo engine's class body writes.
//!
//! An engine is a Railtie: `config` in its class body is the
//! application's configuration (railties 8.1.4 engine.rb, `Engine#config`
//! is `Engine::Configuration`, and a key it does not know falls through
//! `Railtie::Configuration#method_missing` to the shared
//! `@@options` every railtie and the application read). So
//!
//! ```ruby
//! module PlatformComponent
//!   module Essentials
//!     class Engine < Rails::Engine
//!       config.platform_essentials = essentials_config = Essentials::Config.for(Rails.env).new(ENV.to_hash)
//! ```
//!
//! defines `Rails.configuration.platform_essentials` exactly as the
//! same line in config/application.rb would, and is lifted the same way
//! (`extract_config_assignments`, `lower::config_reader`).
//!
//! THE VALUE KEEPS ITS LEXICAL SCOPE. It runs in the engine's class
//! body, so `Essentials` resolves through `PlatformComponent::Essentials::
//! Engine`, `PlatformComponent::Essentials`, `PlatformComponent`, then
//! the top level. The lifted reader lives on the `Rails::Application`
//! reopen, where that lookup would not find it, so each bare constant is
//! rewritten to the absolute name the engine's lookup finds among the
//! classes ingest carries. A name no carried class answers is left as
//! written: the top-level lookup is what the engine body falls back to.
//!
//! REFUSED, not lifted: a value that reads the engine's own state —
//! `self`, a receiverless method call (`root.join(…)` is the ENGINE's
//! root), or a local of the class body. The key then has no reader, and
//! its reads stay `Rails.configuration.<key>`, which check reports.
//! Only unconditional statements directly in the class body count; a
//! write under `if` is not lifted.


use ruby_prism::{Node, Visit};

use super::util::{class_name_path, constant_id_str, flatten_statements, module_name_path};

/// One lifted engine assignment: the key path and the value's source,
/// rewritten into absolute constants.
pub(super) struct EngineAssignment {
    pub path: Vec<String>,
    pub value: String,
    pub start: usize,
    pub end: usize,
}

/// The config writes of every `class X < Rails::Engine` body in
/// `source`. `known` answers whether a fully qualified name is a carried
/// constant (a class, module or namespace prefix of one).
pub(super) fn extract(source: &[u8], file: &str, known: &dyn Fn(&str) -> bool) -> Vec<EngineAssignment> {
    let result = super::prism::parse(source, file);
    let root = result.node();
    let Some(program) = root.as_program_node() else { return Vec::new() };
    let mut out = Vec::new();
    walk(program.statements().body().iter().collect(), &[], source, file, known, &mut out);
    out
}

fn walk<'pr>(
    stmts: Vec<Node<'pr>>,
    nesting: &[String],
    source: &[u8],
    file: &str,
    known: &dyn Fn(&str) -> bool,
    out: &mut Vec<EngineAssignment>,
) {
    for stmt in stmts {
        let (segments, body, engine) = if let Some(class) = stmt.as_class_node() {
            (class_name_path(&class), class.body(), super::app::is_rails_engine_class(&class))
        } else if let Some(module) = stmt.as_module_node() {
            (module_name_path(&module), module.body(), false)
        } else {
            continue;
        };
        let Some(segments) = segments else { continue };
        let cref = match nesting.last() {
            Some(outer) => format!("{outer}::{}", segments.join("::")),
            None => segments.join("::"),
        };
        let mut inner = nesting.to_vec();
        inner.push(cref);
        let body = body.map(flatten_statements).unwrap_or_default();
        if engine {
            for stmt in &body {
                if let Some(a) = assignment(stmt, &inner, source, file, known) {
                    out.push(a);
                }
            }
        }
        walk(body, &inner, source, file, known, out);
    }
}

fn assignment(
    stmt: &Node<'_>,
    nesting: &[String],
    source: &[u8],
    file: &str,
    known: &dyn Fn(&str) -> bool,
) -> Option<EngineAssignment> {
    let call = stmt.as_call_node()?;
    let name = constant_id_str(&call.name()).to_string();
    let base = name.strip_suffix('=')?;
    if base.is_empty() || base.ends_with(['=', '!', '<', '>']) {
        return None;
    }
    // Only a bare `config` root: inside the engine, `Rails.application.
    // config` is the same object, but a write through it is not the
    // engine idiom and is left to the application-file reader.
    let receiver = call.receiver()?;
    if !bare_config_root(&receiver) {
        return None;
    }
    let mut path = super::app::config_receiver_path(&Some(receiver), None)?;
    path.push(base.to_string());
    let args = call.arguments()?;
    let mut value = args.arguments().iter().next()?;
    // `config.k = local = v`: the local is the engine body's, and the
    // reader answers `v`.
    while let Some(write) = value.as_local_variable_write_node() {
        value = write.value();
    }
    let start = stmt.location().start_offset();
    let end = stmt.location().end_offset();
    let mut scan = ValueScan { nesting, known, base: value.location().start_offset(), edits: Vec::new(), refusal: None, block_depth: 0 };
    scan.visit(&value);
    if let Some(reason) = scan.refusal {
        super::initializers::refuse(file, start, end, format!("engine config `{}` is not lifted: {reason}", path.join(".")));
        return None;
    }
    let loc = value.location();
    let mut text = String::from_utf8_lossy(&source[loc.start_offset()..loc.end_offset()]).into_owned();
    scan.edits.sort_by_key(|(s, _, _)| std::cmp::Reverse(*s));
    for (s, e, replacement) in scan.edits {
        text.replace_range(s..e, &replacement);
    }
    Some(EngineAssignment { path, value: text, start, end })
}

/// `config` with no receiver and no arguments, at the root of the chain.
fn bare_config_root(node: &Node<'_>) -> bool {
    let Some(call) = node.as_call_node() else { return false };
    if constant_id_str(&call.name()) == "config" {
        return call.receiver().is_none() && call.arguments().is_none();
    }
    call.receiver().is_some_and(|r| bare_config_root(&r))
}

struct ValueScan<'a> {
    nesting: &'a [String],
    known: &'a dyn Fn(&str) -> bool,
    base: usize,
    edits: Vec<(usize, usize, String)>,
    refusal: Option<String>,
    block_depth: u32,
}

impl ValueScan<'_> {
    fn refuse(&mut self, reason: String) {
        if self.refusal.is_none() {
            self.refusal = Some(reason);
        }
    }

    /// The absolute name Ruby's lexical lookup finds for `name` from the
    /// engine body, innermost scope first, among carried constants.
    fn resolve(&self, name: &str) -> Option<String> {
        self.nesting.iter().rev().map(|cref| format!("{cref}::{name}")).find(|full| (self.known)(full))
    }
}

impl<'pr> Visit<'pr> for ValueScan<'_> {
    fn visit_constant_read_node(&mut self, node: &ruby_prism::ConstantReadNode<'pr>) {
        let name = constant_id_str(&node.name());
        if let Some(full) = self.resolve(name) {
            let loc = node.location();
            self.edits.push((loc.start_offset() - self.base, loc.end_offset() - self.base, format!("::{full}")));
        }
    }

    fn visit_self_node(&mut self, _node: &ruby_prism::SelfNode<'pr>) {
        self.refuse("it reads the engine's `self`".to_string());
    }

    fn visit_local_variable_read_node(&mut self, node: &ruby_prism::LocalVariableReadNode<'pr>) {
        // A read at least as many scopes up as the blocks it sits in is
        // a local of the class body.
        if node.depth() >= self.block_depth {
            let name = constant_id_str(&node.name()).to_string();
            self.refuse(format!("it reads `{name}`, a local of the engine body"));
        }
    }

    fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
        if node.receiver().is_none() {
            let name = constant_id_str(&node.name());
            if !matches!(name, "lambda" | "proc") {
                self.refuse(format!("it calls `{name}` on the engine"));
            }
        }
        ruby_prism::visit_call_node(self, node);
    }

    fn visit_block_node(&mut self, node: &ruby_prism::BlockNode<'pr>) {
        self.block_depth += 1;
        ruby_prism::visit_block_node(self, node);
        self.block_depth -= 1;
    }

    fn visit_lambda_node(&mut self, node: &ruby_prism::LambdaNode<'pr>) {
        self.block_depth += 1;
        ruby_prism::visit_lambda_node(self, node);
        self.block_depth -= 1;
    }
}

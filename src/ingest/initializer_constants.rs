//! Constants a boot initializer assigns at its top level.
//!
//! Rails `load`s every `config/initializers/**/*.rb` of every engine and
//! of the application at boot (railties engine.rb
//! `load_config_initializers`), so `ShopifyTracer =
//! OpenTelemetry.tracer_provider.tracer("Shopify")` in Shopify Core's
//! essentials `00_tracing.rb` binds `ShopifyTracer` on Object before any
//! app code runs, and the app reads it everywhere. Nothing modeled the
//! statement, so each read was "constant not supported".
//!
//! WHAT IS TAKEN. A program-level `NAME = value` of a file in
//! `App::initializer_files`, with the value ingested as app code at its
//! real span (a trailing `#: Type` ascription included). One file's
//! constants become one `LibraryClassOrigin::FileConstants` holder,
//! named for its first constant, like an `app/models` file of constants
//! alone: the analyzer types each value through the constant registry,
//! keyed by the source declaration every read resolves to, and the Ruby
//! family writes the constants at the top level of the holder's file.
//! `App::initializer_constants` lists the holders in initializer order;
//! the CRuby boot (`project::apply_initializer_constants`) requires each
//! holder's file in that order, after the gems' railtie initializers and
//! before the app's code loads, which is where Rails runs the file
//! (`load_config_initializers` precedes Finisher's
//! `setup_main_autoloader`). Only the constant writes of the file run
//! there; every other statement stays its own `initializer_unsupported`
//! gap, as before.
//!
//! WHAT IS REFUSED, by name at the statement (the survey's gap detail):
//! * a value that reads a local of the file (`config = …; X =
//!   config[:x]`): the statement that assigns it does not run;
//! * a value that reads the file's own location (`__FILE__`, `__dir__`),
//!   which the emitted file does not share;
//! * a second assignment of a name an earlier initializer assigned, or a
//!   name an app class, module or model also declares;
//! * a value ingest cannot represent.
//!
//! NOT TAKEN (left to the readers that already own them, or a gap):
//! sorbet type aliases, factory classes (`X = Struct.new(…) do`) and
//! `X = Class.new(Parent)` class declarations, constant-path writes
//! (`A::B = …`) and writes nested in a block, branch or body.

use crate::ingest::prism_api::PrismLocation;
use crate::det::HashSet;

use ruby_prism::Node;

use crate::App;
use crate::app::InitializerConstants;
use crate::expr::{Expr, ExprNode, Literal};
use crate::{ClassId, Symbol};

use super::util::constant_id_str;

/// One taken `NAME = value` statement, waiting for [`keep`].
pub(super) struct Candidate {
    file: String,
    start: usize,
    end: usize,
    name: Symbol,
    value: Expr,
}

/// The program-level constant writes of one initializer that this
/// module takes. A write it recognizes and cannot take is refused at
/// its statement (`initializers::refuse`).
pub(super) fn extract(source: &[u8], file: &str) -> Vec<Candidate> {
    super::sources::register(file, &String::from_utf8_lossy(source));
    let result = super::prism::parse(source, file);
    let root = result.node();
    let Some(program) = root.as_program_node() else { return Vec::new() };
    let mut out = Vec::new();
    for stmt in program.statements().body().iter() {
        let Some(write) = stmt.as_constant_write_node() else { continue };
        let value = write.value();
        if super::library_class::is_sorbet_type_alias(&value)
            || super::factory_class::is_file_factory_class_write(&stmt)
            || is_class_new_declaration(&value)
        {
            continue;
        }
        let loc = stmt.location();
        let (start, end) = (loc.start_offset(), loc.end_offset());
        let name = constant_id_str(&write.name()).to_string();
        if let Some(reason) = unrunnable_value(&value) {
            super::initializers::refuse(
                file,
                start,
                end,
                format!("`{name}` is not defined at boot: its value {reason}"),
            );
            continue;
        }
        let ingested = match super::expr::ingest_expr(&value, file) {
            // Survey mode stands a synthetic nil in for a value that did
            // not ingest; that is not the authored value.
            Ok(expr) if !(expr.span.is_synthetic() && matches!(&*expr.node, ExprNode::Lit { value: Literal::Nil })) => {
                expr
            }
            Ok(_) => {
                super::initializers::refuse(
                    file,
                    start,
                    end,
                    format!("`{name}` is not defined at boot: its value does not ingest"),
                );
                continue;
            }
            Err(err) => {
                super::initializers::refuse(
                    file,
                    start,
                    end,
                    format!("`{name}` is not defined at boot: its value does not ingest: {err}"),
                );
                continue;
            }
        };
        out.push(Candidate { file: file.to_string(), start, end, name: Symbol::from(name), value: ingested });
    }
    out
}

/// The `(name, statement start)` of each write [`extract`] took from
/// `file`, which the older initializer class reader must no longer
/// hoist into that file's first class.
pub(super) fn taken_in<'a>(candidates: &'a [Candidate], file: &str) -> Vec<(&'a str, usize)> {
    candidates.iter().filter(|c| c.file == file).map(|c| (c.name.as_str(), c.start)).collect()
}

/// Turn the candidates (in initializer order) into one holder per file,
/// after every app class is known, so a name an app declaration owns is
/// refused instead of written twice.
pub(super) fn keep(app: &mut App, dir: &std::path::Path, candidates: Vec<Candidate>) {
    let declared: HashSet<String> = app
        .library_classes
        .iter()
        .map(|c| c.name.0.as_str().to_string())
        // A file-level constant an app file wrote, carried on its first class.
        .chain(app.library_classes.iter().flat_map(|c| c.top_level_constants.iter().map(|t| t.name.as_str().to_string())))
        .chain(app.models.iter().map(|m| m.name.0.as_str().to_string()))
        .chain(app.controllers.iter().map(|c| c.name.0.as_str().to_string()))
        .collect();
    let relative = |file: &str| {
        std::path::Path::new(file).strip_prefix(dir).map_or(file.to_string(), |p| p.display().to_string())
    };
    let mut first_file: crate::det::HashMap<String, String> = crate::det::HashMap::default();
    let mut groups: Vec<(String, Vec<(Symbol, Expr, bool)>)> = Vec::new();
    for Candidate { file, start, end, name, value } in candidates {
        if let Some(earlier) = first_file.get(name.as_str()) {
            super::initializers::refuse(
                &file,
                start,
                end,
                format!("`{name}` is reassigned (not modeled); first assigned by {}", relative(earlier)),
            );
            continue;
        }
        if declared.contains(name.as_str()) {
            super::initializers::refuse(
                &file,
                start,
                end,
                format!("`{name}` is also declared by app code; the two are not merged"),
            );
            continue;
        }
        first_file.insert(name.as_str().to_string(), file.clone());
        super::initializers::consume(&file, start, end);
        match groups.last_mut() {
            Some((f, constants)) if *f == file => constants.push((name, value, false)),
            _ => groups.push((file, vec![(name, value, false)])),
        }
    }
    for (file, constants) in groups {
        let names: Vec<Symbol> = constants.iter().map(|(name, _, _)| name.clone()).collect();
        let mut holder = super::library_class::file_constants_holder(&names[0]);
        super::library_class::carry_file_constants(&mut holder, constants);
        let file = relative(&file);
        app.initializer_constants.push(InitializerConstants {
            file,
            holder: ClassId(names[0].clone()),
            names,
        });
        app.library_classes.push(holder);
    }
}

/// `Class.new(Parent)` with no block: a class declaration
/// (`library_class::class_new_declaration`), not a value.
pub(super) fn is_class_new_declaration(value: &Node<'_>) -> bool {
    let Some(call) = value.as_call_node() else { return false };
    call.block().is_none()
        && constant_id_str(&call.name()) == "new"
        && call.receiver().is_some_and(|r| {
            r.as_constant_read_node().is_some_and(|c| constant_id_str(&c.name()) == "Class")
                || r.as_constant_path_node().is_some_and(|p| {
                    p.parent().is_none() && p.name().is_some_and(|n| constant_id_str(&n) == "Class")
                })
        })
        && call.arguments().is_some_and(|a| {
            let args: Vec<_> = a.arguments().iter().collect();
            matches!(args.as_slice(), [one] if one.as_constant_read_node().is_some() || one.as_constant_path_node().is_some())
        })
}

/// Why `value` cannot run apart from the rest of its file, if it cannot.
fn unrunnable_value(value: &Node<'_>) -> Option<String> {
    struct Scan {
        depth: u32,
        reason: Option<String>,
    }
    impl<'pr> ruby_prism::Visit<'pr> for Scan {
        fn visit_local_variable_read_node(&mut self, node: &ruby_prism::LocalVariableReadNode<'pr>) {
            // As many scopes up as the blocks it sits in: a local of the
            // file, which another statement assigns.
            if node.depth() >= self.depth && self.reason.is_none() {
                self.reason = Some(format!(
                    "reads `{}`, a local of the initializer",
                    constant_id_str(&node.name())
                ));
            }
        }
        fn visit_source_file_node(&mut self, _node: &ruby_prism::SourceFileNode<'pr>) {
            self.reason.get_or_insert_with(|| "reads `__FILE__`, the initializer's own path".to_string());
        }
        fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
            if node.receiver().is_none() && constant_id_str(&node.name()) == "__dir__" {
                self.reason.get_or_insert_with(|| "reads `__dir__`, the initializer's directory".to_string());
            }
            ruby_prism::visit_call_node(self, node);
        }
        fn visit_block_node(&mut self, node: &ruby_prism::BlockNode<'pr>) {
            self.depth += 1;
            ruby_prism::visit_block_node(self, node);
            self.depth -= 1;
        }
        fn visit_lambda_node(&mut self, node: &ruby_prism::LambdaNode<'pr>) {
            self.depth += 1;
            ruby_prism::visit_lambda_node(self, node);
            self.depth -= 1;
        }
    }
    let mut scan = Scan { depth: 0, reason: None };
    ruby_prism::Visit::visit(&mut scan, value);
    scan.reason
}

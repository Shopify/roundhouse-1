//! Classes a constant write builds with a body block:
//! `X = Struct.new(:a, :b) do … end`, `X = Struct.new(:a, keyword_init:
//! true) do … end` and `X = Data.define(:a) do … end`.
//!
//! Ruby evaluates the block with `class_eval` on the class the factory
//! returns. Its `def`s are that class's methods, and a visibility
//! marker inside it applies to the methods after it in the block. The
//! block is not part of the enclosing class body, so a `private` written
//! before the constant does not reach into it. (CRuby 4.0:
//! `X.public_method_defined?(:m)` for a `def m` in the block after an
//! outer `private`; `private` inside the block makes the following defs
//! private on `X`.)
//!
//! Each such constant becomes a `LibraryClass` named `Scope::X` with a
//! `LibraryClassOrigin::FactoryClass` origin holding the factory call.
//! Its methods are the block's own definitions; the members, the
//! constructor and value equality stay the runtime's. The Ruby family
//! emits `X = <factory> do … end` around the class body.
//!
//! The body walks that lower a class body skip these constant writes
//! (`is_factory_class_write`); `factory_classes` claims them. Both use
//! `factory_block`, so a block is either a class here or the walk's
//! named refusal, never a constant whose defs were dropped.

use ruby_prism::{BlockNode, CallNode, Node};

use crate::dialect::{LibraryClass, LibraryClassOrigin, MethodReceiver};
use crate::expr::{Expr, ExprNode};
use crate::span::Span;
use crate::{ClassId, Symbol};

use super::expr::ingest_expr;
use super::library_class::{synth_attr_reader, synth_attr_writer};
use super::util::{
    class_name_path, constant_id_str, constant_path_of, flatten_statements, module_name_path,
    symbol_value,
};
use super::{IngestError, IngestResult};

/// `Struct.new(...) do … end` / `Data.define(...) do … end` whose block
/// defines a method: the call and its block. `None` for every other
/// value, including the blockless forms (a constant of the enclosing
/// scope, analysed by `analyze::data`).
pub(super) fn factory_block<'pr>(value: &Node<'pr>) -> Option<(CallNode<'pr>, BlockNode<'pr>)> {
    let call = value.as_call_node()?;
    let generator = match constant_id_str(&call.name()) {
        "new" => "Struct",
        "define" => "Data",
        _ => return None,
    };
    let receiver = call.receiver()?;
    if constant_path_of(&receiver)? != [generator] {
        return None;
    }
    let block = call.block()?.as_block_node()?;
    // A block with no method definition or visibility marker
    // (`Data.define(:a) { "x" }`, an `include`-only body) adds nothing a
    // class needs, and stays the constant `analyze::data` reads, with
    // its block left unmodeled.
    let defines = block.body().is_some_and(|body| {
        flatten_statements(body).iter().any(|stmt| {
            super::visibility::definition(stmt).is_some()
                || stmt.as_singleton_class_node().is_some()
                || stmt
                    .as_call_node()
                    .is_some_and(|c| c.receiver().is_none() && super::visibility::marker(&c))
        })
    });
    defines.then_some((call, block))
}

/// A `NAME = Struct.new(...) do … end` / `NAME = Data.define(...) do …
/// end` statement, which `factory_classes` turns into a class.
pub(super) fn is_factory_class_write(stmt: &Node<'_>) -> bool {
    stmt.as_constant_write_node()
        .is_some_and(|write| factory_block(&write.value()).is_some())
}

/// Every factory class written directly in the file's top level or in a
/// `class`/`module` body, at any nesting, under the qualified name Ruby
/// gives it.
pub(super) fn factory_classes(root: &Node<'_>, file: &str) -> IngestResult<Vec<LibraryClass>> {
    let mut out = Vec::new();
    if let Some(program) = root.as_program_node() {
        collect(Some(program.statements().as_node()), &[], file, &mut out)?;
    }
    Ok(out)
}

fn collect(
    body: Option<Node<'_>>,
    scope: &[String],
    file: &str,
    out: &mut Vec<LibraryClass>,
) -> IngestResult<()> {
    let Some(body) = body else { return Ok(()) };
    for stmt in flatten_statements(body) {
        if let Some(write) = stmt.as_constant_write_node() {
            if let Some((call, block)) = factory_block(&write.value()) {
                let mut path = scope.to_vec();
                path.push(constant_id_str(&write.name()).to_string());
                out.push(factory_class(&path, &call, &block, file)?);
            }
        } else if let Some(class) = stmt.as_class_node() {
            if let Some(name) = class_name_path(&class) {
                let mut inner = scope.to_vec();
                inner.extend(name);
                collect(class.body(), &inner, file, out)?;
            }
        } else if let Some(module) = stmt.as_module_node() {
            if let Some(name) = module_name_path(&module) {
                let mut inner = scope.to_vec();
                inner.extend(name);
                collect(module.body(), &inner, file, out)?;
            }
        }
    }
    Ok(())
}

fn unsupported(file: &str, message: &str) -> IngestError {
    IngestError::Unsupported { file: file.into(), message: message.into() }
}

fn factory_class(
    path: &[String],
    call: &CallNode<'_>,
    block: &BlockNode<'_>,
    file: &str,
) -> IngestResult<LibraryClass> {
    let struct_factory = constant_id_str(&call.name()) == "new";
    let generator = if struct_factory { "Struct" } else { "Data" };
    let receiver = call.receiver().expect("factory_block checked the receiver");
    let mut args = Vec::new();
    if let Some(arguments) = call.arguments() {
        let arguments: Vec<_> = arguments.arguments().iter().collect();
        for (index, arg) in arguments.iter().enumerate() {
            if symbol_value(arg).is_some() {
                args.push(ingest_expr(arg, file)?);
                continue;
            }
            // `keyword_init: true|false|nil`, the one option Struct.new
            // takes, last. Data.define takes none.
            let keyword_init = struct_factory
                && index + 1 == arguments.len()
                && arg.as_keyword_hash_node().is_some_and(|hash| {
                    let elements: Vec<_> = hash.elements().iter().collect();
                    matches!(elements.as_slice(), [only] if only.as_assoc_node().is_some_and(|assoc| {
                        symbol_value(&assoc.key()).as_deref() == Some("keyword_init")
                            && (assoc.value().as_true_node().is_some()
                                || assoc.value().as_false_node().is_some()
                                || assoc.value().as_nil_node().is_some())
                    }))
                });
            if !keyword_init {
                return Err(unsupported(
                    file,
                    &format!(
                        "{generator} factory with a body block needs literal symbol members{}",
                        if struct_factory { " and at most a literal keyword_init:" } else { "" }
                    ),
                ));
            }
            args.push(ingest_expr(arg, file)?);
        }
    }
    if block.parameters().is_some() {
        return Err(unsupported(
            file,
            &format!("block parameters on a {generator} factory body are not modeled"),
        ));
    }
    let owner = ClassId(Symbol::from(path.join("::")));
    let body = super::library_class::factory_class_body(block.body(), &owner, file)?;
    let loc = call.location();
    let span = Span {
        file: super::sources::file_id(file),
        start: loc.start_offset() as u32,
        end: loc.end_offset() as u32,
    };
    let factory = Expr::new(
        span,
        ExprNode::Send {
            recv: Some(ingest_expr(&receiver, file)?),
            method: Symbol::from(constant_id_str(&call.name())),
            args,
            block: None,
            parenthesized: true,
        },
    );
    let origin = LibraryClassOrigin::FactoryClass { factory };
    // The factory's readers (and Struct's writers), so every pass sees
    // `w` in `w * h` as the member it is. Not where the block defines
    // the method itself; the Ruby family emits none of these
    // (`factory_generated_methods`).
    let mut methods = body.methods;
    for generated in origin.factory_generated_methods().unwrap_or_default() {
        if methods.iter().any(|m| m.name == generated && m.receiver == MethodReceiver::Instance) {
            continue;
        }
        methods.push(match generated.as_str().strip_suffix('=') {
            Some(member) => synth_attr_writer(&owner, &Symbol::from(member), MethodReceiver::Instance),
            None => synth_attr_reader(&owner, &generated, MethodReceiver::Instance),
        });
    }
    Ok(LibraryClass {
        name: owner,
        is_module: false,
        parent: Some(ClassId(Symbol::from(generator))),
        includes: body.includes,
        methods,
        nullable_columns: Vec::new(),
        origin: Some(origin),
        constants: Vec::new(),
        unknown_calls: body.unknown_calls,
        class_ivar_initializers: Vec::new(),
    })
}

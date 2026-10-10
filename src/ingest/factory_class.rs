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
//! `X = Class.new(Parent) do … end` evaluates its block the same way
//! and builds nothing the runtime supplies, so it is a plain class with
//! that parent, emitted with a `class` header (`class_new_class`).
//!
//! The body walks that lower a class body skip these constant writes
//! (`is_factory_class_write`); `factory_classes` claims them. Both use
//! `factory_block`, so a block is either a class here or the walk's
//! named refusal, never a constant whose defs were dropped.
//!
//! At FILE level the block-less form is a class too: `Pair =
//! Struct.new(:left, :right)` alone in `app/models/pair.rb` is how an
//! app declares the class Zeitwerk loads for `Pair` (Shopify Core's
//! `ApiClientVisibilityParent`, `SmsLengthParent`). Inside a class or
//! module body the block-less form stays that scope's constant, which
//! `analyze::data` reads.

use ruby_prism::{BlockNode, CallNode, ConstantWriteNode, Node};

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

/// The class a factory call builds: `Struct` (`Struct.new`), `Data`
/// (`Data.define`) or `Class` (`Class.new`).
fn generator(call: &CallNode<'_>) -> Option<&'static str> {
    let receiver = call.receiver()?;
    let receiver = constant_path_of(&receiver)?;
    match (constant_id_str(&call.name()), receiver.as_slice()) {
        ("new", [only]) if only == "Struct" => Some("Struct"),
        ("new", [only]) if only == "Class" => Some("Class"),
        ("define", [only]) if only == "Data" => Some("Data"),
        _ => None,
    }
}

/// `Struct.new(...) do … end` / `Data.define(...) do … end` / `Class.new(...)
/// do … end` whose block defines a method: the call and its block. `None`
/// for every other value, including the blockless forms (a constant of
/// the enclosing scope, analysed by `analyze::data`, or for `Class.new`
/// the declaration `library_class::class_new_declaration` reads).
pub(super) fn factory_block<'pr>(value: &Node<'pr>) -> Option<(CallNode<'pr>, BlockNode<'pr>)> {
    let call = value.as_call_node()?;
    generator(&call)?;
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

/// The constant writes directly in the body block of a factory class
/// write. Ruby evaluates the block with the cref of the scope the write
/// sits in, so `HDR = /x/` in `module M; X = Data.define(:a) do … end`
/// defines `M::HDR`, not `X::HDR` (CRuby 4.0: `M::HDR` is the regexp and
/// `X.const_defined?(:HDR, false)` is false). The enclosing scope's walk
/// carries them, before the class constant, as Ruby assigns them.
pub(super) fn block_constant_writes<'pr>(stmt: &Node<'pr>) -> Vec<ConstantWriteNode<'pr>> {
    let Some(write) = stmt.as_constant_write_node() else { return Vec::new() };
    let Some((_, block)) = factory_block(&write.value()) else { return Vec::new() };
    body_constant_writes(&block)
}

fn body_constant_writes<'pr>(block: &BlockNode<'pr>) -> Vec<ConstantWriteNode<'pr>> {
    let Some(body) = block.body() else { return Vec::new() };
    flatten_statements(body).into_iter().filter_map(|stmt| stmt.as_constant_write_node()).collect()
}

/// The names of the constants a `class << self` directly in `block`
/// writes.
fn singleton_constant_writes(block: &BlockNode<'_>) -> Vec<Symbol> {
    let Some(body) = block.body() else { return Vec::new() };
    flatten_statements(body)
        .into_iter()
        .filter_map(|stmt| stmt.as_singleton_class_node())
        .filter(|sc| sc.expression().as_self_node().is_some())
        .filter_map(|sc| sc.body())
        .flat_map(flatten_statements)
        .filter_map(|stmt| stmt.as_constant_write_node())
        .map(|write| Symbol::from(constant_id_str(&write.name())))
        .collect()
}

/// A file-level statement `factory_classes` turns into a class: a
/// factory with a body block, or a block-less one with literal
/// members ([`blockless_factory`]).
pub(super) fn is_file_factory_class_write(stmt: &Node<'_>) -> bool {
    stmt.as_constant_write_node().is_some_and(|write| {
        factory_block(&write.value()).is_some() || blockless_factory(&write.value()).is_some()
    })
}

/// `Struct.new(:a, :b)`, `Struct.new(:a, keyword_init: true)` or
/// `Data.define(:a)` with no block: the call, when every member is a
/// literal symbol (and Struct's one option, a literal `keyword_init:`,
/// comes last). Any other argument (`Struct.new("Name", :a)`, a
/// splatted member list) stays a constant, as it was.
fn blockless_factory<'pr>(value: &Node<'pr>) -> Option<CallNode<'pr>> {
    let call = value.as_call_node()?;
    let generator = match constant_id_str(&call.name()) {
        "new" => "Struct",
        "define" => "Data",
        _ => return None,
    };
    let receiver = call.receiver()?;
    if constant_path_of(&receiver)? != [generator] || call.block().is_some() {
        return None;
    }
    literal_members(&call, generator == "Struct").then_some(call)
}

/// Every argument a literal symbol member, except a trailing literal
/// `keyword_init:` for Struct.
fn literal_members(call: &CallNode<'_>, struct_factory: bool) -> bool {
    let Some(arguments) = call.arguments() else { return true };
    let arguments: Vec<_> = arguments.arguments().iter().collect();
    arguments.iter().enumerate().all(|(index, arg)| {
        symbol_value(arg).is_some()
            || (struct_factory && index + 1 == arguments.len() && is_keyword_init(arg))
    })
}

/// `keyword_init: true|false|nil`, the one option Struct.new takes.
fn is_keyword_init(arg: &Node<'_>) -> bool {
    arg.as_keyword_hash_node().is_some_and(|hash| {
        let elements: Vec<_> = hash.elements().iter().collect();
        matches!(elements.as_slice(), [only] if only.as_assoc_node().is_some_and(|assoc| {
            symbol_value(&assoc.key()).as_deref() == Some("keyword_init")
                && (assoc.value().as_true_node().is_some()
                    || assoc.value().as_false_node().is_some()
                    || assoc.value().as_nil_node().is_some())
        }))
    })
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

/// [`factory_classes`] for one top-level declaration (`class`/`module`
/// statement) of a file: what a file program carries inline
/// (`library_class::program_items`).
pub(super) fn factory_classes_under(declaration: &Node<'_>, file: &str) -> IngestResult<Vec<LibraryClass>> {
    let mut out = Vec::new();
    collect_stmt(declaration, &[], file, &mut out, false)?;
    Ok(out)
}

/// `singleton`: `body` is a `class << self` directly in the declaration
/// `scope` names. Ruby binds a constant written there on the singleton
/// class (CRuby 4.0: `M.singleton_class.const_defined?(:X, false)`),
/// which no class is emitted for. Every walk that lowers that body
/// carries its constants on the declaration instead
/// (`library_class::walk_decl_body_in`, `singleton_class`), which its
/// methods read by bare name all the same, so the factory class is
/// `Scope::X` too. Only its constant writes are classes here; a class
/// or module nested in it is not visited.
fn collect(
    body: Option<Node<'_>>,
    scope: &[String],
    file: &str,
    out: &mut Vec<LibraryClass>,
) -> IngestResult<()> {
    collect_in(body, scope, file, out, false)
}

fn collect_in(
    body: Option<Node<'_>>,
    scope: &[String],
    file: &str,
    out: &mut Vec<LibraryClass>,
    singleton: bool,
) -> IngestResult<()> {
    let Some(body) = body else { return Ok(()) };
    for stmt in flatten_statements(body) {
        collect_stmt(&stmt, scope, file, out, singleton)?;
    }
    Ok(())
}

fn collect_stmt(
    stmt: &Node<'_>,
    scope: &[String],
    file: &str,
    out: &mut Vec<LibraryClass>,
    singleton: bool,
) -> IngestResult<()> {
    if singleton && stmt.as_constant_write_node().is_none() {
        return Ok(());
    }
    if let Some(write) = stmt.as_constant_write_node() {
        let value = write.value();
        let factory = match factory_block(&value) {
            Some((call, block)) => Some((call, Some(block))),
            // At file level only: see the module doc.
            None if scope.is_empty() => blockless_factory(&value).map(|call| (call, None)),
            None => None,
        };
        if let Some((call, block)) = factory {
            let mut path = scope.to_vec();
            path.push(constant_id_str(&write.name()).to_string());
            out.push(factory_class(&path, &call, block.as_ref(), file)?);
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
    } else if let Some(sc) = stmt.as_singleton_class_node() {
        if !scope.is_empty() && sc.expression().as_self_node().is_some() {
            collect_in(sc.body(), scope, file, out, true)?;
        }
    }
    Ok(())
}

/// `do |klass| … end` whose one required parameter no statement of the
/// block reads.
fn unread_class_parameter(block: &BlockNode<'_>) -> bool {
    let Some(params) = block.parameters().and_then(|p| p.as_block_parameters_node()) else { return false };
    if params.locals().iter().next().is_some() {
        return false;
    }
    let Some(list) = params.parameters() else { return false };
    let required: Vec<_> = list.requireds().iter().collect();
    let only_required = list.optionals().iter().next().is_none()
        && list.rest().is_none()
        && list.posts().iter().next().is_none()
        && list.keywords().iter().next().is_none()
        && list.keyword_rest().is_none()
        && list.block().is_none();
    let [param] = required.as_slice() else { return false };
    let Some(param) = param.as_required_parameter_node() else { return false };
    if !only_required {
        return false;
    }
    let name = constant_id_str(&param.name()).to_string();
    struct Reads<'a>(&'a str, bool);
    impl<'pr> ruby_prism::Visit<'pr> for Reads<'_> {
        fn visit_local_variable_read_node(&mut self, node: &ruby_prism::LocalVariableReadNode<'pr>) {
            self.1 |= constant_id_str(&node.name()) == self.0;
        }
    }
    let mut reads = Reads(&name, false);
    if let Some(body) = block.body() {
        ruby_prism::Visit::visit(&mut reads, &body);
    }
    !reads.1
}

fn unsupported(file: &str, message: &str) -> IngestError {
    IngestError::Unsupported { file: file.into(), message: message.into() }
}

pub(super) fn factory_class(
    path: &[String],
    call: &CallNode<'_>,
    block: Option<&BlockNode<'_>>,
    file: &str,
) -> IngestResult<LibraryClass> {
    let generator = generator(call).expect("factory_block checked the receiver");
    if generator == "Class" {
        return class_new_class(path, call, block, file);
    }
    let struct_factory = generator == "Struct";
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
            let keyword_init = struct_factory && index + 1 == arguments.len() && is_keyword_init(arg);
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
    let (owner, body) = factory_body(path, block, generator, file)?;
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
        conditional_definitions: Vec::new(),
        method_aliases: Vec::new(),
        top_level_constants: Vec::new(),
        name: owner,
        is_module: false,
        parent: Some(ClassId(Symbol::from(generator))),
        parent_span: Default::default(),
        includes: body.includes,
        methods,
        nullable_columns: Vec::new(),
        origin: Some(origin),
        constants: body.constants,
        unknown_calls: body.unknown_calls,
        class_ivar_initializers: Vec::new(),
        visibility_overrides: body.visibility_overrides,
    })
}

/// `NAME = Class.new(Parent) do … end` / `NAME = Class.new do … end`:
/// the class `class NAME < Parent … end` declares. Ruby names the new
/// class on assignment (`Outer::NAME`), `class_exec`s the block on it, so
/// its `def`s, `class << self` and markers are that class's, and the
/// block keeps the enclosing cref, so a constant written directly in it
/// is the enclosing scope's (`block_constant_writes`), as for
/// `Struct.new`. (CRuby 4.0: `Outer::G.name == "Outer::G"`, `Outer::C.
/// superclass == Outer::G`, `Outer.const_defined?(:INNER)` and not
/// `Outer::G`'s, a `private` in the block makes the following `def`
/// private on the class, and `Outer.respond_to?(:update)` is false for a
/// `class << self; def update` in the block.)
///
/// Unlike a `Struct.new` body the block builds nothing the runtime must
/// supply, so it is an ordinary library class with that parent (written,
/// resolved lexically like the block-less form's,
/// `library_class::library_classes`), emitted as a `class` header. That
/// header's one difference from the block, a cref of the class's own,
/// adds no constant lookup: the block's constants are carried out of it.
/// A block that reads a local of the scope around it (a closure, which a
/// class body cannot be) or a parent that is not a constant path is
/// refused by name.
fn class_new_class(
    path: &[String],
    call: &CallNode<'_>,
    block: Option<&BlockNode<'_>>,
    file: &str,
) -> IngestResult<LibraryClass> {
    let arguments: Vec<Node<'_>> = call.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default();
    let parent = match arguments.as_slice() {
        [] => None,
        [parent] => match constant_path_of(parent) {
            Some(written) if !written.is_empty() => Some(ClassId(Symbol::from(written.join("::")))),
            _ => {
                return Err(unsupported(
                    file,
                    "`Class.new(parent) do … end` whose parent is not a constant path is not modeled",
                ));
            }
        },
        _ => return Err(unsupported(file, "`Class.new` with more than one argument is not modeled")),
    };
    if block.is_some_and(reads_enclosing_locals) {
        return Err(unsupported(
            file,
            "a `Class.new do … end` body block that reads a local variable of the enclosing scope is not modeled",
        ));
    }
    let (owner, body) = factory_body(path, block, "Class", file)?;
    Ok(LibraryClass {
        conditional_definitions: Vec::new(),
        method_aliases: Vec::new(),
        top_level_constants: Vec::new(),
        name: owner,
        is_module: false,
        parent,
        parent_span: Default::default(),
        includes: body.includes,
        methods: body.methods,
        nullable_columns: Vec::new(),
        origin: None,
        constants: body.constants,
        unknown_calls: body.unknown_calls,
        class_ivar_initializers: Vec::new(),
        visibility_overrides: body.visibility_overrides,
    })
}

/// Whether a statement of `block` reads or writes a local variable of
/// the scope around the block: a closure, which a class body is not.
/// Prism counts a local's `depth` in scopes from where it is written;
/// `def`, `class`, `module` and `class << self` open a scope that sees no
/// outer local, so they are not entered.
fn reads_enclosing_locals(block: &BlockNode<'_>) -> bool {
    struct Locals {
        blocks: u32,
        outer: bool,
    }
    impl Locals {
        fn depth(&mut self, depth: u32) {
            self.outer |= depth > self.blocks;
        }
    }
    impl<'pr> ruby_prism::Visit<'pr> for Locals {
        fn visit_block_node(&mut self, node: &ruby_prism::BlockNode<'pr>) {
            self.blocks += 1;
            ruby_prism::visit_block_node(self, node);
            self.blocks -= 1;
        }
        fn visit_lambda_node(&mut self, node: &ruby_prism::LambdaNode<'pr>) {
            self.blocks += 1;
            ruby_prism::visit_lambda_node(self, node);
            self.blocks -= 1;
        }
        fn visit_def_node(&mut self, _node: &ruby_prism::DefNode<'pr>) {}
        fn visit_class_node(&mut self, _node: &ruby_prism::ClassNode<'pr>) {}
        fn visit_module_node(&mut self, _node: &ruby_prism::ModuleNode<'pr>) {}
        fn visit_singleton_class_node(&mut self, node: &ruby_prism::SingletonClassNode<'pr>) {
            // `class << expr`: the expression is evaluated outside.
            ruby_prism::Visit::visit(self, &node.expression());
        }
        fn visit_local_variable_read_node(&mut self, node: &ruby_prism::LocalVariableReadNode<'pr>) {
            self.depth(node.depth());
        }
        fn visit_local_variable_write_node(&mut self, node: &ruby_prism::LocalVariableWriteNode<'pr>) {
            self.depth(node.depth());
            ruby_prism::visit_local_variable_write_node(self, node);
        }
        fn visit_local_variable_target_node(&mut self, node: &ruby_prism::LocalVariableTargetNode<'pr>) {
            self.depth(node.depth());
        }
        fn visit_local_variable_operator_write_node(
            &mut self,
            node: &ruby_prism::LocalVariableOperatorWriteNode<'pr>,
        ) {
            self.depth(node.depth());
            ruby_prism::visit_local_variable_operator_write_node(self, node);
        }
        fn visit_local_variable_and_write_node(&mut self, node: &ruby_prism::LocalVariableAndWriteNode<'pr>) {
            self.depth(node.depth());
            ruby_prism::visit_local_variable_and_write_node(self, node);
        }
        fn visit_local_variable_or_write_node(&mut self, node: &ruby_prism::LocalVariableOrWriteNode<'pr>) {
            self.depth(node.depth());
            ruby_prism::visit_local_variable_or_write_node(self, node);
        }
    }
    let mut locals = Locals { blocks: 0, outer: false };
    if let Some(body) = block.body() {
        ruby_prism::Visit::visit(&mut locals, &body);
    }
    locals.outer
}

/// The class a factory body block builds and what the block gives it.
fn factory_body(
    path: &[String],
    block: Option<&BlockNode<'_>>,
    generator: &str,
    file: &str,
) -> IngestResult<(ClassId, super::library_class::FactoryClassBody)> {
    // Ruby passes the new class to the block (`do |klass|`). A single
    // parameter the block never reads (Core's `do |_klass|`) changes
    // nothing; any other parameter list is refused.
    if block.is_some_and(|block| block.parameters().is_some() && !unread_class_parameter(block)) {
        return Err(unsupported(
            file,
            &format!("block parameters on a {generator} factory body are not modeled"),
        ));
    }
    let owner = ClassId(Symbol::from(path.join("::")));
    // The block's own constant writes are the enclosing scope's, which
    // its walk carries (`block_constant_writes`).
    let carried: Vec<Symbol> = block
        .map(body_constant_writes)
        .unwrap_or_default()
        .iter()
        .map(|write| Symbol::from(constant_id_str(&write.name())))
        .collect();
    // The constants a `class << self` in the block writes are the
    // class's own (`factory_class_body`).
    let singleton_written = block.map(singleton_constant_writes).unwrap_or_default();
    // Nested in a class or module body, that `class << self` is indexed
    // as the ENCLOSING scope's singleton (Rubydex reads `self` there as
    // the lexical scope, not the block's class), so a read of such a
    // constant would resolve to `Scope::C`, which nothing defines.
    if path.len() > 1 && !singleton_written.is_empty() {
        return Err(unsupported(
            file,
            &format!(
                "constant written in a `class << self` of a {generator} factory body inside a class or module \
                 body is not modeled"
            ),
        ));
    }
    let mut body = super::library_class::factory_class_body(
        block.and_then(|block| block.body()),
        &owner,
        &carried,
        &singleton_written,
        file,
    )?;
    // Rubydex leaves a file-level factory block's `class << self`
    // unindexed, so no read of its constants is known to reach the
    // singleton class: they stay hoisted onto the class, writes and
    // reads alike (`SINGLETON_CONSTANT_WRITE` unset).
    for (_, value) in &mut body.constants {
        value.decisions &= !crate::expr::SINGLETON_CONSTANT_WRITE;
    }
    Ok((owner, body))
}

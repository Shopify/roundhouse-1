//! `class << self … end` inside a class body: the class-side half of the
//! class, written as a block.
//!
//! Ruby evaluates the body with `self` set to the class's singleton
//! class, so:
//!
//! * a `def` defines a CLASS method (`Room.create_for`);
//! * `attr_accessor` / `attr_reader` / `attr_writer` define class-level
//!   accessors over the class's own instance variables;
//! * `alias_method` aliases a class method;
//! * `include Mod` mixes `Mod` into the singleton class — `Mod`'s
//!   instance methods become class methods, the same thing `extend Mod`
//!   says in the class body proper;
//! * a constant assignment lands in the singleton class's lexical
//!   scope, which the block's own methods read by bare name;
//! * `sig`, `abstract!`, `private_constant`, and the visibility markers
//!   carry no behavior the flattened method list can hold.
//!
//! Models and controllers both walk their class body one statement at a
//! time, and a `class << self` is one statement that stands for many, so
//! both expand it through [`ingest_singleton_body`]. (Library classes
//! keep their own richer walk in `library_class::walk_decl_body`.)
//!
//! ## Recovery
//!
//! A statement this walk cannot read costs itself. Before, the first
//! one aborted the whole block, and with it every `def` the block
//! declared: on Shop, Order and Product a single `RATE = 5` or `sig { … }`
//! erased the class's entire class-side API, and every call into it was
//! reported as a missing method. Strict mode still refuses the first
//! unreadable statement; survey mode records it and keeps reading.

use ruby_prism::Node;

use crate::dialect::{MethodDef, MethodReceiver};
use crate::expr::{Expr, ExprNode};
use crate::ident::{ClassId, Symbol};

use super::expr::ingest_expr;
use super::library_class::{
    alias_source, is_sorbet_annotation_mixin, is_sorbet_type_alias, synth_attr_reader,
    synth_attr_writer, SORBET_ANNOTATIONS,
};
use super::util::{constant_id_str, flatten_statements, symbol_value};
use super::{IngestError, IngestResult};

/// What one `class << self` block declares.
#[derive(Default)]
pub(super) struct SingletonBody {
    /// The block's methods, all `MethodReceiver::Class`, in source order.
    pub methods: Vec<MethodDef>,
    /// Statements to replay in the enclosing class body: the block's
    /// constant assignments, and `extend Mod` for each `include Mod`.
    /// Held as raw expressions, exactly like any other class-body
    /// statement the enclosing walk does not classify.
    pub class_body: Vec<Expr>,
}

const VISIBILITY: &[&str] =
    &["private", "protected", "public", "private_class_method", "public_class_method"];

/// Expand one `class << self` block. `ingest_def` reads a `def` the way
/// the enclosing walk reads its own methods, so a controller's class
/// methods and a model's carry the parameter model of their side.
pub(super) fn ingest_singleton_body(
    sc: &ruby_prism::SingletonClassNode<'_>,
    owner: &ClassId,
    file: &str,
    ingest_def: &dyn Fn(&ruby_prism::DefNode<'_>) -> IngestResult<MethodDef>,
) -> IngestResult<SingletonBody> {
    let mut out = SingletonBody::default();
    // Only `class << self` is the enclosing class's own singleton. The
    // `class << other_object` form opens somebody else's.
    if sc.expression().as_self_node().is_none() {
        return unsupported(file, "unsupported `class << <expr>`: only `class << self` is modeled")
            .map(|()| out);
    }
    let Some(body) = sc.body() else { return Ok(out) };
    for stmt in flatten_statements(body) {
        walk_statement(&stmt, owner, file, ingest_def, &mut out)?;
    }
    Ok(out)
}

fn walk_statement(
    stmt: &Node<'_>,
    owner: &ClassId,
    file: &str,
    ingest_def: &dyn Fn(&ruby_prism::DefNode<'_>) -> IngestResult<MethodDef>,
    out: &mut SingletonBody,
) -> IngestResult<()> {
    if let Some(def) = stmt.as_def_node() {
        return push_def(&def, ingest_def, out);
    }
    if let Some(cw) = stmt.as_constant_write_node() {
        // `Result = T.type_alias { … }` is a type, not a value.
        if is_sorbet_type_alias(&cw.value()) {
            return Ok(());
        }
        // `X = Struct.new(...) do … end` is the class `Owner::X`, which
        // the library ingest of this file builds
        // (`factory_class::collect`), as it does for one in the class
        // body proper. Replayed as a body statement too, it would be
        // defined twice. The block's own constant writes would be the
        // class's, which neither walk carries.
        if super::factory_class::is_factory_class_write(stmt) {
            if !super::factory_class::block_constant_writes(stmt).is_empty() {
                return unsupported(
                    file,
                    "constant assigned inside a Struct.new/Data.define/Class.new body block in a model or controller \
                     `class << self` is not modeled",
                );
            }
            return Ok(());
        }
        return match ingest_expr(stmt, file) {
            Ok(mut expr) => {
                // The singleton class's constant
                // (`SINGLETON_CONSTANT_WRITE`), on the statement and on
                // its value, whichever form the enclosing walk keeps.
                expr.decisions |= crate::expr::SINGLETON_CONSTANT_WRITE;
                if let ExprNode::Assign { value, .. } = &mut *expr.node {
                    value.decisions |= crate::expr::SINGLETON_CONSTANT_WRITE;
                }
                out.class_body.push(expr);
                Ok(())
            }
            Err(err) => recover(err),
        };
    }
    // `alias new old` copies the method as it stands, the same as
    // `alias_method` below and the library-class walk's `alias`.
    if let Some(alias) = stmt.as_alias_method_node() {
        let to = super::library_class::alias_keyword_name(&alias.new_name());
        let from = super::library_class::alias_keyword_name(&alias.old_name());
        let source = to.zip(from).and_then(|(to, from)| {
            out.methods
                .iter()
                .rposition(|m| m.name.as_str() == from && m.receiver == MethodReceiver::Class)
                .map(|i| (to, i))
        });
        let Some((to, source)) = source else {
            return unsupported(
                file,
                "unsupported statement inside `class << self`: `alias` of a method the block does not define",
            );
        };
        let mut copy = out.methods[source].clone();
        copy.name = Symbol::from(to.as_str());
        copy.name_span = crate::span::Span {
            file: super::sources::file_id(file),
            start: alias.location().start_offset() as u32,
            end: alias.location().end_offset() as u32,
        };
        out.methods.push(copy);
        return Ok(());
    }
    let Some(call) = stmt.as_call_node() else {
        return unsupported(file, &format!("unsupported statement inside `class << self`: {stmt:?}"));
    };
    let name = constant_id_str(&call.name()).to_string();
    let receiverless = call.receiver().is_none();

    // `sig { … }` and `T::Sig::WithoutRuntime.sig { … }` — types, read
    // by `sorbet_sig` already.
    if name == "sig" || (receiverless && SORBET_ANNOTATIONS.contains(&name.as_str())) {
        return Ok(());
    }
    if !receiverless {
        return unsupported(
            file,
            &format!("unsupported statement inside `class << self`: call to `{name}` on a receiver"),
        );
    }
    let args: Vec<Node<'_>> = call
        .arguments()
        .map(|a| a.arguments().iter().collect())
        .unwrap_or_default();

    // Visibility: `private` alone changes nothing this list carries;
    // `private def x` and `private_class_method def x` also define x.
    if VISIBILITY.contains(&name.as_str()) {
        for arg in &args {
            if let Some(def) = arg.as_def_node() {
                push_def(&def, ingest_def, out)?;
            }
        }
        return Ok(());
    }
    if matches!(name.as_str(), "private_constant" | "public_constant") {
        return Ok(());
    }
    // Deprecation annotates an existing method without changing the
    // class-side surface, just as it does in the library-class walk.
    if name == "deprecate" && call.block().is_none() {
        return Ok(());
    }

    match name.as_str() {
        "attr_reader" | "attr_writer" | "attr_accessor" => {
            for arg in &args {
                let Some(sym) = symbol_value(arg) else {
                    return unsupported(
                        file,
                        &format!("unsupported statement inside `class << self`: `{name}` with a non-symbol name"),
                    );
                };
                let sym = Symbol::from(sym.as_str());
                if name != "attr_writer" {
                    out.methods.push(synth_attr_reader(owner, &sym, MethodReceiver::Class));
                }
                if name != "attr_reader" {
                    out.methods.push(synth_attr_writer(owner, &sym, MethodReceiver::Class));
                }
            }
            Ok(())
        }
        // Copies the method as it stands, like the library-class walk.
        "alias_method" => match alias_source(&call, &out.methods, true) {
            Some((to, source)) => {
                let mut copy = out.methods[source].clone();
                copy.name = Symbol::from(to.as_str());
                out.methods.push(copy);
                Ok(())
            }
            None => unsupported(
                file,
                "unsupported statement inside `class << self`: `alias_method` of a method the block does not define",
            ),
        },
        "include" | "extend" if is_sorbet_annotation_mixin(&call) => Ok(()),
        // The singleton class including `Mod` is the class extending it.
        "include" => match ingest_expr(stmt, file) {
            Ok(mut expr) => {
                if let ExprNode::Send { method, .. } = &mut *expr.node {
                    *method = Symbol::from("extend");
                }
                out.class_body.push(expr);
                Ok(())
            }
            Err(err) => recover(err),
        },
        _ => unsupported(
            file,
            &format!("unsupported statement inside `class << self`: call to `{name}`"),
        ),
    }
}

fn push_def(
    def: &ruby_prism::DefNode<'_>,
    ingest_def: &dyn Fn(&ruby_prism::DefNode<'_>) -> IngestResult<MethodDef>,
    out: &mut SingletonBody,
) -> IngestResult<()> {
    match ingest_def(def) {
        Ok(mut method) => {
            method.receiver = MethodReceiver::Class;
            out.methods.push(method);
            Ok(())
        }
        Err(err) => recover(err),
    }
}

/// Survey mode records the gap and reads on; strict mode refuses.
fn recover(err: IngestError) -> IngestResult<()> {
    if super::survey::is_active() {
        super::survey::record(&err);
        Ok(())
    } else {
        Err(err)
    }
}

fn unsupported(file: &str, message: &str) -> IngestResult<()> {
    recover(IngestError::Unsupported { file: file.into(), message: message.into() })
}

/// `class << self … end` at expression position: inside a `Class.new do`
/// / `included do` / `class_eval do` block or a method body, where no
/// class-body walk classifies it (Core's `Apps::Enum.new`).
///
/// Ruby runs the body with `self` the singleton class of the enclosing
/// `self`. The IR has no such scope, so each statement is written where
/// the enclosing `self` is still the object, aimed at its singleton
/// class (`singleton_class`):
///
/// * `def x` is written `def self.x` (the same singleton method);
/// * `attr_*`, `alias_method` / `alias`, `define_method`, `include`,
///   `extend`, `prepend`, `private_constant`, and any other
///   implicit-self call without a block, are sent to `singleton_class`
///   by name (`singleton_class.send(:alias_method, :[], :enum_for)`:
///   `send`, as an implicit-self call may reach a private method, and
///   the analyzer proves no class object for `singleton_class`);
/// * `private` / `protected` / `public` with names make those singleton
///   methods so (`singleton_class.send(:private, :new)`); without
///   arguments they set the visibility of the `def`s, `attr_*` and
///   `define_method`s after them, as in the source (an `alias_method`
///   keeps its original's visibility, as Ruby's does);
/// * a call on `self` (`self.x = 1`) is sent to `singleton_class` too;
/// * a bare `self` is `singleton_class`;
/// * `sig` and the other sorbet annotations are dropped, as in the
///   class-body form.
///
/// A statement whose meaning depends on the singleton scope in a way
/// this cannot write is refused by name, one statement at a time: a
/// constant assignment (Ruby binds it on the singleton class, where the
/// block's methods read it lexically), another implicit-self call with a
/// block (the block's `self` would be the object), an argument reading
/// the body's `self` (`attr_reader *names`), a `define_method` block
/// calling a bare name (outside the scope gate it could read an
/// enclosing local), an instance variable, `yield` or `super` in the
/// body, a local of the body's own scope (a block shares the enclosing
/// one), `module_function`, `def self.x`, a `def` nested in another
/// statement, and `class << <expr>`.
pub(super) fn singleton_class_expr(
    sc: &ruby_prism::SingletonClassNode<'_>,
    span: crate::span::Span,
    file: &str,
) -> IngestResult<Expr> {
    let nil = |span| Expr::new(span, ExprNode::Lit { value: crate::expr::Literal::Nil });
    if sc.expression().as_self_node().is_none() {
        return Err(refusal(file, "only `class << self` is modeled, not `class << <expr>`"));
    }
    let Some(body) = sc.body() else { return Ok(nil(span)) };
    let Some(statements) = body.as_statements_node() else {
        return Err(refusal(file, "a body with `rescue` / `ensure` is not modeled"));
    };
    let locals: Vec<String> = sc.locals().iter().map(|l| constant_id_str(&l).to_string()).collect();
    if !locals.is_empty() {
        return Err(refusal(
            file,
            &format!(
                "a local variable of the body's own scope ({}) is not modeled: the statements run in the enclosing scope",
                locals.join(", ")
            ),
        ));
    }
    let mut out = Vec::new();
    super::expr::with_singleton_defs(&body, file, || {
        let mut visibility: Option<&'static str> = None;
        for stmt in statements.body().iter() {
            match singleton_statement(&stmt, &mut visibility, file) {
                Ok(exprs) => out.extend(exprs),
                Err(err) => recover(err)?,
            }
        }
        Ok::<(), IngestError>(())
    })?;
    Ok(match out.len() {
        0 => nil(span),
        1 => out.pop().unwrap(),
        _ => Expr::new(span, ExprNode::Seq { exprs: out }),
    })
}

fn refusal(file: &str, why: &str) -> IngestError {
    IngestError::Unsupported {
        file: file.into(),
        message: format!("`class << self` at expression position: {why}"),
    }
}

/// `singleton_class`, the enclosing `self`'s.
fn singleton_class_ref() -> Expr {
    Expr::new(
        crate::span::Span::synthetic(),
        ExprNode::Send {
            recv: None,
            method: Symbol::from("singleton_class"),
            args: Vec::new(),
            block: None,
            parenthesized: false,
        },
    )
}

fn sym(name: &str) -> Expr {
    Expr::new(
        crate::span::Span::synthetic(),
        ExprNode::Lit { value: crate::expr::Literal::Sym { value: Symbol::from(name) } },
    )
}

/// `singleton_class.send(:method, *args)`.
fn singleton_send(span: crate::span::Span, method: &str, args: Vec<Expr>, block: Option<Expr>) -> Expr {
    let mut all = vec![sym(method)];
    all.extend(args);
    Expr::new(
        span,
        ExprNode::Send {
            recv: Some(singleton_class_ref()),
            method: Symbol::from("send"),
            args: all,
            block,
            parenthesized: true,
        },
    )
}

/// What defines a method that a bare `private` / `protected` before it
/// applies to (`alias_method` keeps its original's visibility).
const TAKES_DEFAULT_VISIBILITY: &[&str] = &["attr_reader", "attr_writer", "attr_accessor", "attr", "define_method"];

fn singleton_statement(
    stmt: &Node<'_>,
    visibility: &mut Option<&'static str>,
    file: &str,
) -> IngestResult<Vec<Expr>> {
    let span = super::util::node_span(stmt, file);
    if let Some(def) = stmt.as_def_node() {
        let expr = ingest_expr(stmt, file)?;
        let name = constant_id_str(&def.name()).to_string();
        let mut out = vec![expr];
        if let Some(v) = visibility {
            out.push(singleton_send(span, v, vec![sym(&name)], None));
        }
        return Ok(out);
    }
    if stmt.as_self_node().is_some() {
        return Ok(vec![singleton_class_ref()]);
    }
    if let Some(alias) = stmt.as_alias_method_node() {
        let (Some(to), Some(from)) = (
            super::library_class::alias_keyword_name(&alias.new_name()),
            super::library_class::alias_keyword_name(&alias.old_name()),
        ) else {
            return Err(refusal(file, "an `alias` of interpolated names is not modeled"));
        };
        return Ok(vec![singleton_send(span, "alias_method", vec![sym(&to), sym(&from)], None)]);
    }
    if stmt.as_constant_write_node().is_some()
        || stmt.as_constant_path_write_node().is_some()
        || stmt.as_constant_or_write_node().is_some()
        || stmt.as_constant_operator_write_node().is_some()
    {
        return Err(refusal(
            file,
            "a constant assigned in the body is not modeled: Ruby binds it on the singleton class, where the body's methods read it lexically",
        ));
    }
    if let Some(call) = stmt.as_call_node() {
        let name = constant_id_str(&call.name()).to_string();
        let receiver = call.receiver();
        if receiver.is_none() {
            if name == "sig" || SORBET_ANNOTATIONS.contains(&name.as_str()) || is_sorbet_annotation_mixin(&call) {
                return Ok(Vec::new());
            }
            if name == "module_function" {
                return Err(refusal(file, "`module_function` (undefined on a singleton class) is not modeled"));
            }
            if matches!(name.as_str(), "private" | "protected" | "public") {
                let args: Vec<Node<'_>> = call.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default();
                if args.is_empty() {
                    *visibility = (name != "public").then(|| match name.as_str() {
                        "private" => "private",
                        _ => "protected",
                    });
                    return Ok(Vec::new());
                }
                let mut out = Vec::new();
                let mut names = Vec::new();
                for arg in &args {
                    if let Some(def) = arg.as_def_node() {
                        out.push(ingest_expr(arg, file)?);
                        names.push(sym(constant_id_str(&def.name())));
                    } else if arg.as_call_node().is_some_and(|c| {
                        c.receiver().is_none() && TAKES_DEFAULT_VISIBILITY.contains(&constant_id_str(&c.name()))
                    }) {
                        // `private attr_reader :x`: its names, defined on
                        // the singleton class.
                        let mut none = None;
                        names.extend(singleton_statement(arg, &mut none, file)?);
                    } else if reaches_self(arg) != (false, false) {
                        return Err(refusal(file, &format!("`{name}` of a computed name is not modeled")));
                    } else {
                        names.push(ingest_expr(arg, file)?);
                    }
                }
                out.push(singleton_send(span, &name, names, None));
                return Ok(out);
            }
            let has_block = call.block().is_some_and(|b| b.as_block_node().is_some());
            if has_block && name != "define_method" {
                return Err(refusal(
                    file,
                    &format!("`{name}` with a block is not modeled: the block's `self` would be the object, not its singleton class"),
                ));
            }
            refuse_scope_reading_arguments(&call, &name, file)?;
            if let Some(bare) = call.block().as_ref().and_then(bare_name_call) {
                // `class << self` is a scope gate: Ruby parses `x` in the
                // block as a call, which written in the enclosing scope
                // would read a local `x` of that scope instead.
                return Err(refusal(
                    file,
                    &format!("`{name}` whose block calls the bare name `{bare}` is not modeled: written outside the `class << self` scope gate it could read a local of the enclosing scope"),
                ));
            }
            let expr = ingest_expr(stmt, file)?;
            let ExprNode::Send { recv: None, method, args, block, .. } = *expr.node else {
                return Err(refusal(file, &format!("`{name}` is not modeled")));
            };
            let call = singleton_send(span, method.as_str(), args, block);
            return Ok(vec![match visibility {
                Some(v) if TAKES_DEFAULT_VISIBILITY.contains(&name.as_str()) => singleton_send(span, v, vec![call], None),
                _ => call,
            }]);
        }
        if receiver.as_ref().is_some_and(|r| r.as_self_node().is_some()) {
            if call.block().is_some() {
                return Err(refusal(file, &format!("`self.{name}` with a block is not modeled")));
            }
            refuse_scope_reading_arguments(&call, &name, file)?;
            let expr = ingest_expr(stmt, file)?;
            return match *expr.node {
                ExprNode::Send { recv: Some(recv), method, args, block: None, .. }
                    if matches!(&*recv.node, ExprNode::SelfRef) =>
                {
                    Ok(vec![singleton_send(span, method.as_str(), args, None)])
                }
                ExprNode::Assign { target: crate::expr::LValue::Attr { recv, name: attr }, value }
                    if matches!(&*recv.node, ExprNode::SelfRef) =>
                {
                    Ok(vec![singleton_send(span, &format!("{}=", attr.as_str()), vec![value], None)])
                }
                _ => Err(refusal(file, &format!("`self.{name}` is not modeled"))),
            };
        }
    }
    let (reaches, defines) = reaches_self(stmt);
    // A `def` nested in a block or conditional is refused by name where it
    // is ingested (`method_def_expr` reads the marks of the enclosing
    // `with_singleton_defs`), so that refusal is the one reported.
    let expr = if defines { Some(ingest_expr(stmt, file)?) } else { None };
    if reaches {
        return Err(refusal(
            file,
            "a statement reading `self`, an instance variable, `yield` or `super` of the singleton scope is not modeled",
        ));
    }
    Ok(vec![match expr {
        Some(expr) => expr,
        None => ingest_expr(stmt, file)?,
    }])
}

/// Refuses a call sent to `singleton_class` whose arguments read the
/// body's `self` (`attr_reader *names`, `x(self)`): written in the
/// enclosing scope they would read the object instead.
fn refuse_scope_reading_arguments(call: &ruby_prism::CallNode<'_>, name: &str, file: &str) -> IngestResult<()> {
    let reads = call
        .arguments()
        .is_some_and(|args| args.arguments().iter().any(|arg| reaches_self(&arg) != (false, false)));
    if reads {
        return Err(refusal(
            file,
            &format!("`{name}` with an argument that reads the singleton scope's `self` is not modeled: written outside it the argument would read the object"),
        ));
    }
    Ok(())
}

/// The first bare name (`x`, parsed as a call because no local `x` is in
/// scope) under `node`.
fn bare_name_call(node: &Node<'_>) -> Option<String> {
    use ruby_prism::Visit;
    struct Bare(Option<String>);
    impl<'pr> Visit<'pr> for Bare {
        fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
            if self.0.is_none() && node.is_variable_call() {
                self.0 = Some(constant_id_str(&node.name()).to_string());
            }
            ruby_prism::visit_call_node(self, node);
        }
    }
    let mut bare = Bare(None);
    bare.visit(node);
    bare.0
}

/// Whether `node` depends on the scope's `self` (`self`, an implicit-self
/// call, an instance or class variable, `yield`, `super` or a constant
/// write), and whether it holds a `def` (whose body is not visited).
fn reaches_self(node: &Node<'_>) -> (bool, bool) {
    use ruby_prism::Visit;
    struct Reach(bool, bool);
    impl<'pr> Visit<'pr> for Reach {
        fn visit_self_node(&mut self, _: &ruby_prism::SelfNode<'pr>) {
            self.0 = true;
        }
        fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
            if node.receiver().is_none() {
                self.0 = true;
            }
            ruby_prism::visit_call_node(self, node);
        }
        fn visit_instance_variable_read_node(&mut self, _: &ruby_prism::InstanceVariableReadNode<'pr>) {
            self.0 = true;
        }
        fn visit_instance_variable_write_node(&mut self, _: &ruby_prism::InstanceVariableWriteNode<'pr>) {
            self.0 = true;
        }
        fn visit_instance_variable_or_write_node(&mut self, _: &ruby_prism::InstanceVariableOrWriteNode<'pr>) {
            self.0 = true;
        }
        fn visit_instance_variable_and_write_node(&mut self, _: &ruby_prism::InstanceVariableAndWriteNode<'pr>) {
            self.0 = true;
        }
        fn visit_instance_variable_operator_write_node(
            &mut self,
            _: &ruby_prism::InstanceVariableOperatorWriteNode<'pr>,
        ) {
            self.0 = true;
        }
        fn visit_instance_variable_target_node(&mut self, _: &ruby_prism::InstanceVariableTargetNode<'pr>) {
            self.0 = true;
        }
        fn visit_class_variable_read_node(&mut self, _: &ruby_prism::ClassVariableReadNode<'pr>) {
            self.0 = true;
        }
        fn visit_class_variable_write_node(&mut self, _: &ruby_prism::ClassVariableWriteNode<'pr>) {
            self.0 = true;
        }
        fn visit_yield_node(&mut self, _: &ruby_prism::YieldNode<'pr>) {
            self.0 = true;
        }
        fn visit_super_node(&mut self, _: &ruby_prism::SuperNode<'pr>) {
            self.0 = true;
        }
        fn visit_forwarding_super_node(&mut self, _: &ruby_prism::ForwardingSuperNode<'pr>) {
            self.0 = true;
        }
        fn visit_def_node(&mut self, _: &ruby_prism::DefNode<'pr>) {
            self.1 = true;
        }
        fn visit_constant_write_node(&mut self, _: &ruby_prism::ConstantWriteNode<'pr>) {
            self.0 = true;
        }
        fn visit_constant_path_write_node(&mut self, _: &ruby_prism::ConstantPathWriteNode<'pr>) {
            self.0 = true;
        }
    }
    let mut reach = Reach(false, false);
    reach.visit(node);
    (reach.0, reach.1)
}

//! ActiveSupport's `Class#class_attribute`, as 8.2.0.alpha defines it
//! (activesupport `core_ext/class/attribute.rb`, `class_attribute.rb`):
//!
//! ```ruby
//! class_attribute :name, instance_accessor: true, instance_reader: …,
//!   instance_writer: …, instance_predicate: true, default: nil
//! ```
//!
//! Rails stores the value in `@__class_attr_<name>` on the class that
//! last wrote it (the owner) and reads it through the owner, so a
//! subclass reads its parent's value, live, until it writes its own.
//! The declaration also sets that ivar on the declaring class to
//! `default`. The expansion here is the same storage written as plain
//! defs, which the analyzer can see and every lane can run:
//!
//! - `self.name`: the `@__class_attr_name` of the nearest class at or
//!   above this one that has it (the owner; the declaring class always
//!   has one). Read off the owner directly, as Rails reads it, so a
//!   protected or private `name` on an ancestor does not get in the way;
//! - `self.name=(value)`: writes this class's own;
//! - `self.name?` (`instance_predicate`): `!!self.name`;
//! - `name` (`instance_reader`): the instance's own `@name` once written,
//!   else `self.class.name`;
//! - `name=` (`instance_writer`): `attr_writer :name`;
//! - `name?` (`instance_predicate` and `instance_reader`): `!!name`.
//!
//! They are public whatever visibility the surrounding body is in:
//! Rails defines them through a string `class_eval`, which starts public.
//! The declaration's seed, `@__class_attr_name = default`, is a statement
//! that runs in the class body where the declaration stood (the
//! caller's: `ModelBodyItem::ClassBody` or a library class initializer).
//!
//! Only the documented options with literal `true`/`false`/`nil` flags
//! are expanded; anything else (a splat, a computed flag, a non-literal
//! name) leaves the declaration to the caller's unsupported path.

use crate::dialect::MethodDef;
use crate::expr::{Expr, ExprNode, LValue, Literal};
use crate::ident::{ClassId, Symbol};
use crate::span::Span;

use super::expr::ingest_expr;
use super::util::{constant_id_str, symbol_value};

/// One `class_attribute` declaration.
pub(crate) struct Decl {
    pub names: Vec<Symbol>,
    pub instance_reader: bool,
    pub instance_writer: bool,
    pub instance_predicate: bool,
    /// `default:`; Rails' default is nil.
    pub default: Option<Expr>,
    /// The declaration's own span: its seed runs where it stood.
    pub span: Span,
}

/// The class-side storage ivar Rails uses (`@__class_attr_<name>`),
/// without the sigil, as `ExprNode::Ivar` spells names.
pub(crate) fn storage_ivar(name: &Symbol) -> Symbol {
    Symbol::from(format!("__class_attr_{}", name.as_str()))
}

/// The attribute a seed (`@__class_attr_<name> = …`) is for.
pub(crate) fn seeded_name(expr: &Expr) -> Option<&str> {
    let ExprNode::Assign { target: LValue::Ivar { name }, .. } = &*expr.node else { return None };
    name.as_str().strip_prefix("__class_attr_")
}

/// Whether `call` runs on the class itself: no receiver, `self`, or
/// `T.unsafe(self)` (Sorbet's cast, which a typed concern writes where
/// `self` is the module: hedwig's `included do
/// T.unsafe(self).class_attribute(:rate_limiting_config, …) end`).
fn on_self(call: &ruby_prism::CallNode<'_>) -> bool {
    let Some(recv) = call.receiver() else { return true };
    if recv.as_self_node().is_some() {
        return true;
    }
    let Some(cast) = recv.as_call_node() else { return false };
    constant_id_str(&cast.name()) == "unsafe"
        && cast.receiver().is_some_and(|r| r.as_constant_read_node().is_some_and(|c| constant_id_str(&c.name()) == "T"))
        && cast.arguments().is_some_and(|args| {
            let mut args = args.arguments().iter();
            args.next().is_some_and(|a| a.as_self_node().is_some()) && args.next().is_none()
        })
}

/// Read a `class_attribute` call on the class (no receiver, `self`, or
/// `T.unsafe(self)`). `None` when it is some other call or a shape the
/// expansion does not model.
pub(crate) fn parse(call: &ruby_prism::CallNode<'_>, file: &str) -> Option<Decl> {
    if !on_self(call) || constant_id_str(&call.name()) != "class_attribute" || call.block().is_some() {
        return None;
    }
    let mut names: Vec<Symbol> = Vec::new();
    let mut instance_accessor: Option<bool> = None;
    let mut instance_reader: Option<bool> = None;
    let mut instance_writer: Option<bool> = None;
    let mut instance_predicate: Option<bool> = None;
    let mut default: Option<Expr> = None;
    let flag = |node: &ruby_prism::Node<'_>| -> Option<bool> {
        if node.as_true_node().is_some() {
            Some(true)
        } else if node.as_false_node().is_some() || node.as_nil_node().is_some() {
            Some(false)
        } else {
            None
        }
    };
    for arg in call.arguments()?.arguments().iter() {
        if let Some(s) = symbol_value(&arg) {
            names.push(Symbol::from(s));
            continue;
        }
        if let Some(s) = arg.as_string_node() {
            names.push(Symbol::from(String::from_utf8_lossy(s.unescaped()).into_owned()));
            continue;
        }
        let hash = arg.as_keyword_hash_node()?;
        for element in hash.elements().iter() {
            let assoc = element.as_assoc_node()?;
            let value = assoc.value();
            let slot = match symbol_value(&assoc.key())?.as_str() {
                "instance_accessor" => &mut instance_accessor,
                "instance_reader" => &mut instance_reader,
                "instance_writer" => &mut instance_writer,
                "instance_predicate" => &mut instance_predicate,
                "default" => {
                    if default.is_some() {
                        return None;
                    }
                    default = Some(ingest_expr(&value, file).ok()?);
                    continue;
                }
                _ => return None,
            };
            if slot.is_some() {
                return None;
            }
            *slot = Some(flag(&value)?);
        }
    }
    if names.is_empty() || names.iter().any(|n| !is_method_name(n.as_str())) {
        return None;
    }
    let accessor = instance_accessor.unwrap_or(true);
    Some(Decl {
        names,
        instance_reader: instance_reader.unwrap_or(accessor),
        instance_writer: instance_writer.unwrap_or(accessor),
        instance_predicate: instance_predicate.unwrap_or(true),
        default,
        span: Span {
            file: super::sources::file_id(file),
            start: call.location().start_offset() as u32,
            end: call.location().end_offset() as u32,
        },
    })
}

pub(super) fn is_method_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c == '_' || c.is_ascii_lowercase())
        && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

/// The defs the declaration makes, owned by `owner`.
pub(crate) fn methods(owner: &ClassId, decl: &Decl) -> Vec<MethodDef> {
    let mut src = String::from("class RoundhouseClassAttribute\n");
    for name in &decl.names {
        let n = name.as_str();
        src.push_str(&format!(
            "  def self.{n}\n    owner = self\n    owner = owner.superclass until owner.instance_variable_defined?(:@__class_attr_{n})\n    owner.instance_variable_get(:@__class_attr_{n})\n  end\n\n"
        ));
        src.push_str(&format!("  def self.{n}=(value)\n    @__class_attr_{n} = value\n  end\n\n"));
        if decl.instance_predicate {
            src.push_str(&format!("  def self.{n}?\n    !!self.{n}\n  end\n\n"));
        }
        if decl.instance_reader {
            src.push_str(&format!("  def {n}\n    defined?(@{n}) ? @{n} : self.class.{n}\n  end\n\n"));
        }
        if decl.instance_writer {
            src.push_str(&format!("  def {n}=(value)\n    @{n} = value\n  end\n\n"));
        }
        if decl.instance_predicate && decl.instance_reader {
            src.push_str(&format!("  def {n}?\n    !!{n}\n  end\n\n"));
        }
    }
    src.push_str("end\n");
    let classes = match super::ingest_library_classes(src.as_bytes(), "<class_attribute>") {
        Ok(classes) => classes,
        Err(err) => {
            super::survey::record(&err);
            return Vec::new();
        }
    };
    let mut out: Vec<MethodDef> = classes.into_iter().flat_map(|c| c.methods).collect();
    for m in &mut out {
        m.enclosing_class = Some(owner.0.clone());
        m.name_span = Span::synthetic();
    }
    out
}

/// `@__class_attr_<name> = default` for each name, at the declaration.
pub(crate) fn seeds(decl: &Decl) -> Vec<Expr> {
    // ActiveSupport evaluates `default:` once and stores that one object
    // under every name: the first name's seed evaluates it and each later
    // one reads the first's ivar back (`@__class_attr_b =
    // @__class_attr_a`), so a mutable default is shared and a default
    // with a side effect runs once.
    let mut first: Option<Symbol> = None;
    decl.names
        .iter()
        .map(|name| {
            let value = match (&first, &decl.default) {
                (Some(shared), Some(_)) => Expr::new(decl.span, ExprNode::Ivar { name: storage_ivar(shared) }),
                (_, Some(default)) => default.clone(),
                (_, None) => Expr::new(Span::synthetic(), ExprNode::Lit { value: Literal::Nil }),
            };
            first.get_or_insert_with(|| name.clone());
            Expr::new(
                decl.span,
                ExprNode::Assign { target: LValue::Ivar { name: storage_ivar(name) }, value },
            )
        })
        .collect()
}

/// `self.<name> = value` (`Send` of `<name>=` on `self`) or
/// `self.<name> += value` / `||=` / … (`OpAssign` on a `self` attribute):
/// a class-body write through a class-side writer. Answers the attribute
/// name.
pub(crate) fn self_write(expr: &Expr) -> Option<&str> {
    match &*expr.node {
        ExprNode::Send { recv: Some(recv), method, args, block: None, .. }
            if matches!(&*recv.node, ExprNode::SelfRef) && args.len() == 1 =>
        {
            method.as_str().strip_suffix('=').filter(|n| is_method_name(n))
        }
        ExprNode::Assign { target: LValue::Attr { recv, name }, .. }
        | ExprNode::OpAssign { target: LValue::Attr { recv, name }, .. }
            if matches!(&*recv.node, ExprNode::SelfRef) =>
        {
            let n = name.as_str();
            Some(n.strip_suffix('=').unwrap_or(n)).filter(|n| is_method_name(n))
        }
        _ => None,
    }
}

/// The attributes a model body's `class_attribute` declarations seed
/// (its own and the ones carried in from included concerns). Their
/// ivars, the class side's `@__class_attr_x` and an instance's own `@x`,
/// hold whatever the app writes: gradual, like an `attr_accessor`
/// virtual.
pub(crate) fn declared_names(body: &[crate::dialect::ModelBodyItem]) -> Vec<Symbol> {
    body.iter()
        .filter_map(|item| match item {
            crate::dialect::ModelBodyItem::ClassBody { expr, .. } => seeded_name(expr).map(Symbol::from),
            _ => None,
        })
        .collect()
}

/// The parameter of a writer this expansion generated for one of
/// `names` (`self.x=(value)` writing `@__class_attr_x`, or `x=(value)`
/// writing `@x`), or `None` for any other method — an app's own
/// redefinition included. Rails' writers take any value: the caller
/// seeds the parameter gradual.
pub(crate) fn generated_writer_param<'a>(method: &'a MethodDef, names: &[Symbol]) -> Option<&'a Symbol> {
    let base = method.name.as_str().strip_suffix('=')?;
    let name = names.iter().find(|n| n.as_str() == base)?;
    let [param] = method.params.as_slice() else { return None };
    let ExprNode::Assign { target: LValue::Ivar { name: ivar }, value } = &*method.body.node else { return None };
    let expected = match method.receiver {
        crate::dialect::MethodReceiver::Class => storage_ivar(name),
        _ => name.clone(),
    };
    let reads_param = matches!(&*value.node, ExprNode::Var { name, .. } if *name == param.name);
    (*ivar == expected && reads_param).then_some(&param.name)
}

/// Class-side visibility a concern's `included do` block gives the
/// methods its own `class_attribute` declarations generate, in the two
/// shapes Rails code uses (Core's `MutationGuard`):
///
/// ```ruby
/// class_attribute :gate
/// wrapped_method = method(:gate=)
/// redefine_singleton_method(:gate=) do |value|
///   wrapped_method.call(value)
///   singleton_class.class_eval { protected :gate }
/// end
/// class << self
///   protected :gate=
/// end
/// ```
///
/// - `class << self; protected :a, …; end` makes those class-side
///   methods protected on the includer (`protected`).
/// - The `method` capture and `redefine_singleton_method` replace the
///   writer with one that runs the captured writer (bound to the
///   includer, so it writes the includer's storage whatever class it is
///   called on) and then makes the reader protected on the receiving
///   class (`wrapped`). The capture's local is read nowhere else.
///
/// The statements are consumed (`consumed`, by start offset). `None` when
/// the block has none of these, or has one the expansion cannot make
/// faithful: a name no earlier `class_attribute` of the block generated,
/// a writer wrapped twice or re-declared after wrapping, a `protected`
/// that a later redefinition would make public again, or any other
/// statement in a singleton body or the redefined block.
pub(crate) struct ClassSideVisibility {
    pub consumed: Vec<usize>,
    pub protected: Vec<String>,
    /// (attribute, the redefined block's parameter, the reader it protects).
    pub wrapped: Vec<(String, String, String)>,
    /// `private :name` / `protected :name` naming an instance method an
    /// earlier `class_attribute` of the block generated.
    pub instance_visibility: Vec<(String, crate::dialect::MethodVisibility)>,
}

pub(crate) fn class_side_visibility(direct: &[ruby_prism::Node<'_>], file: &str) -> Option<ClassSideVisibility> {
    use super::util::flatten_statements;
    let mut declared: Vec<String> = Vec::new();
    let mut captures: Vec<(String, String, usize)> = Vec::new();
    let mut out = ClassSideVisibility {
        consumed: Vec::new(),
        protected: Vec::new(),
        wrapped: Vec::new(),
        instance_visibility: Vec::new(),
    };
    let mut instance_declared: Vec<String> = Vec::new();
    let mut capture_reads: Vec<(String, usize)> = Vec::new();
    let sym_args = |call: &ruby_prism::CallNode<'_>| -> Option<Vec<String>> {
        let args: Vec<String> = call.arguments()?.arguments().iter().map(|a| symbol_value(&a)).collect::<Option<_>>()?;
        (!args.is_empty()).then_some(args)
    };
    for stmt in direct {
        let offset = stmt.location().start_offset();
        if let Some(call) = stmt.as_call_node()
            && let Some(decl) = parse(&call, file)
        {
            for name in &decl.names {
                let n = name.as_str();
                if declared.iter().any(|d| d == n) {
                    return None;
                }
                declared.push(n.to_string());
                declared.push(format!("{n}="));
                if decl.instance_predicate {
                    declared.push(format!("{n}?"));
                }
                if decl.instance_reader {
                    instance_declared.push(n.to_string());
                }
                if decl.instance_writer {
                    instance_declared.push(format!("{n}="));
                }
                if decl.instance_predicate && decl.instance_reader {
                    instance_declared.push(format!("{n}?"));
                }
            }
            continue;
        }
        // `private :name` on the includer's instance reader the block's
        // `class_attribute` generated.
        if let Some(call) = stmt.as_call_node()
            && call.receiver().is_none()
            && call.block().is_none()
            && let visibility = match constant_id_str(&call.name()) {
                "private" => Some(crate::dialect::MethodVisibility::Private),
                "protected" => Some(crate::dialect::MethodVisibility::Protected),
                _ => None,
            }
            && let Some(visibility) = visibility
            && let Some(names) = sym_args(&call)
            && names.iter().all(|n| instance_declared.contains(n))
        {
            for name in names {
                out.instance_visibility.push((name, visibility));
            }
            out.consumed.push(offset);
            continue;
        }
        // `wrapped_method = method(:gate=)`
        if let Some(write) = stmt.as_local_variable_write_node()
            && let Some(call) = write.value().as_call_node()
            && call.receiver().is_none()
            && call.block().is_none()
            && constant_id_str(&call.name()) == "method"
            && let Some(args) = sym_args(&call)
            && let [writer] = args.as_slice()
            && writer.ends_with('=')
            && declared.iter().any(|d| d == writer)
        {
            let local = constant_id_str(&write.name()).to_string();
            if captures.iter().any(|(l, _, _)| *l == local) {
                return None;
            }
            captures.push((local, writer.clone(), offset));
            continue;
        }
        // `redefine_singleton_method(:gate=) do |value| … end`
        if let Some(call) = stmt.as_call_node()
            && call.receiver().is_none()
            && constant_id_str(&call.name()) == "redefine_singleton_method"
        {
            let args = sym_args(&call)?;
            let [writer] = args.as_slice() else { return None };
            let attr = writer.strip_suffix('=')?;
            if out.wrapped.iter().any(|(a, _, _)| a == attr) || out.protected.iter().any(|p| p == writer) {
                return None;
            }
            let block = call.block()?.as_block_node()?;
            let params = block.parameters()?.as_block_parameters_node()?.parameters()?;
            let requireds: Vec<_> = params.requireds().iter().collect();
            let [param] = requireds.as_slice() else { return None };
            if params.optionals().iter().next().is_some()
                || params.rest().is_some()
                || params.posts().iter().next().is_some()
                || params.keywords().iter().next().is_some()
                || params.keyword_rest().is_some()
                || params.block().is_some()
            {
                return None;
            }
            let param = constant_id_str(&param.as_required_parameter_node()?.name()).to_string();
            let body = flatten_statements(block.body()?);
            let [first, second] = body.as_slice() else { return None };
            // `wrapped_method.call(value)`
            let forward = first.as_call_node()?;
            let local = constant_id_str(&forward.receiver()?.as_local_variable_read_node()?.name()).to_string();
            let forwarded: Vec<_> = forward.arguments()?.arguments().iter().collect();
            let [forwarded] = forwarded.as_slice() else { return None };
            if constant_id_str(&forward.name()) != "call"
                || forward.block().is_some()
                || forwarded.as_local_variable_read_node().is_none_or(|l| constant_id_str(&l.name()) != param)
            {
                return None;
            }
            let (_, captured, capture_at) = captures.iter().find(|(l, _, _)| *l == local)?;
            if captured != writer {
                return None;
            }
            // `singleton_class.class_eval { protected :gate }`
            let eval = second.as_call_node()?;
            let recv = eval.receiver()?.as_call_node()?;
            if constant_id_str(&eval.name()) != "class_eval"
                || eval.arguments().is_some()
                || constant_id_str(&recv.name()) != "singleton_class"
                || recv.receiver().is_some()
                || recv.arguments().is_some()
                || recv.block().is_some()
            {
                return None;
            }
            let inner = flatten_statements(eval.block()?.as_block_node()?.body()?);
            let [mark] = inner.as_slice() else { return None };
            let mark = mark.as_call_node()?;
            if mark.receiver().is_some() || mark.block().is_some() || constant_id_str(&mark.name()) != "protected" {
                return None;
            }
            let protects = sym_args(&mark)?;
            let [reader] = protects.as_slice() else { return None };
            if !declared.iter().any(|d| d == reader) || reader.ends_with('=') {
                return None;
            }
            capture_reads.push((local, offset));
            out.consumed.push(*capture_at);
            out.consumed.push(offset);
            out.wrapped.push((attr.to_string(), param, reader.clone()));
            continue;
        }
        // `class << self; protected :gate=; end`
        if let Some(singleton) = stmt.as_singleton_class_node() {
            if singleton.expression().as_self_node().is_none() {
                return None;
            }
            for inner in flatten_statements(singleton.body()?) {
                let mark = inner.as_call_node()?;
                if mark.receiver().is_some() || mark.block().is_some() || constant_id_str(&mark.name()) != "protected" {
                    return None;
                }
                for name in sym_args(&mark)? {
                    if !declared.iter().any(|d| *d == name) {
                        return None;
                    }
                    out.protected.push(name);
                }
            }
            out.consumed.push(offset);
            continue;
        }
    }
    if out.consumed.is_empty() {
        return None;
    }
    // A captured writer read anywhere but its own redefinition is a
    // Method object the expansion does not keep.
    for stmt in direct {
        let offset = stmt.location().start_offset();
        for (local, _, _) in &captures {
            let consumer = capture_reads.iter().any(|(l, at)| l == local && *at == offset);
            if !consumer && reads_local(stmt, local) {
                return None;
            }
        }
    }
    Some(out)
}

fn reads_local(node: &ruby_prism::Node<'_>, local: &str) -> bool {
    struct Reads<'a>(&'a str, bool);
    impl<'pr> ruby_prism::Visit<'pr> for Reads<'_> {
        fn visit_local_variable_read_node(&mut self, node: &ruby_prism::LocalVariableReadNode<'pr>) {
            self.1 |= constant_id_str(&node.name()) == self.0;
        }
    }
    let mut reads = Reads(local, false);
    ruby_prism::Visit::visit(&mut reads, node);
    reads.1
}

/// The writer `class_side_visibility`'s redefinition leaves: the captured
/// writer's effect, bound to `owner` (the includer, once the carried item
/// is rehomed), then the reader made protected on the receiving class.
/// The method returns what the block returned (`class_eval`'s value).
pub(crate) fn wrapped_writer(owner: &ClassId, attr: &str, param: &str, reader: &str) -> Option<MethodDef> {
    let src = format!(
        "class RoundhouseClassAttribute\n  def self.{attr}=({param})\n    {owner}.instance_variable_set(:@__class_attr_{attr}, {param})\n    self.singleton_class.class_eval {{ protected :{reader} }}\n  end\nend\n",
        owner = owner.0.as_str(),
    );
    let classes = match super::ingest_library_classes(src.as_bytes(), "<class_attribute>") {
        Ok(classes) => classes,
        Err(err) => {
            super::survey::record(&err);
            return None;
        }
    };
    let mut method = classes.into_iter().flat_map(|c| c.methods).next()?;
    method.enclosing_class = Some(owner.0.clone());
    method.name_span = Span::synthetic();
    Some(method)
}

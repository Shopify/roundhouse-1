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

/// Read a receiverless `class_attribute` call. `None` when it is some
/// other call or a shape the expansion does not model.
pub(crate) fn parse(call: &ruby_prism::CallNode<'_>, file: &str) -> Option<Decl> {
    if call.receiver().is_some() || constant_id_str(&call.name()) != "class_attribute" || call.block().is_some() {
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

fn is_method_name(name: &str) -> bool {
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
    decl.names
        .iter()
        .map(|name| {
            let value = decl
                .default
                .clone()
                .unwrap_or_else(|| Expr::new(Span::synthetic(), ExprNode::Lit { value: Literal::Nil }));
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

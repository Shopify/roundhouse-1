//! `cattr_*` / `mattr_*` (ActiveSupport core_ext/module/attribute_accessors.rb):
//! one class variable, `@@name`, per attribute.
//!
//! ActiveSupport 8.1 defines, for `mattr_reader :x` in a class or module,
//!
//! ```ruby
//! def self.x; @@x; end
//! def x; @@x; end                 # unless instance_reader/instance_accessor: false
//! class_variable_set("@@x", default) unless default.nil? && class_variable_defined?("@@x")
//! ```
//!
//! and `mattr_writer` the `x=` pair the same way. `mattr_accessor` is both;
//! `cattr_*` are aliases. A class variable is shared by the class, every
//! subclass and their instances. The lowering stored it in the declaring
//! class's own ivar instead, so a subclass read nil and a subclass write
//! went to the subclass alone (activemerchant 1.137.0's `Gateway.inherited`
//! appends to `@@implementations`, which raised for every gateway of a
//! gateway subclass). Authored `@@x` reads and writes beside the
//! declaration now name the same storage the accessors do, with no
//! rewriting.
//!
//! The seed runs where the declaration is, in the class body: `@@x =
//! default`, or `@@x = nil unless defined?(@@x)` without one. A
//! multi-name declaration evaluates its default once and every name holds
//! that one object.

use crate::dialect::{MethodDef, MethodReceiver, MethodVisibility};
use crate::expr::{Expr, ExprNode, LValue, Literal};
use crate::ident::{ClassId, Symbol, VarId};
use crate::span::Span;

use super::IngestError;
use super::IngestResult;
use super::util::symbol_value;

/// A literal-shaped `cattr_*` / `mattr_*` declaration.
pub(crate) struct Declaration {
    pub(crate) names: Vec<Symbol>,
    reader: bool,
    writer: bool,
    instance_reader: bool,
    instance_writer: bool,
    default: Option<Expr>,
}

/// `@@name`.
pub(crate) fn storage(name: &Symbol) -> Symbol {
    Symbol::from(format!("@@{}", name.as_str()))
}

/// Read `call` (`kw` is its method name). Anything ActiveSupport would
/// read another way (a computed name, a computed flag, an unknown option,
/// a block with parameters) is refused rather than approximated.
pub(crate) fn declaration(call: &ruby_prism::CallNode<'_>, kw: &str, file: &str) -> IngestResult<Declaration> {
    let unsupported = |what: &str| IngestError::Unsupported {
        file: file.into(),
        message: format!("`{kw}` with {what} is not modeled"),
    };
    let mut names = Vec::new();
    let mut flags: [Option<bool>; 3] = [None; 3];
    let mut default = None;
    if let Some(args) = call.arguments() {
        for arg in args.arguments().iter() {
            if let Some(name) = symbol_value(&arg) {
                if !is_attribute_name(&name) {
                    return Err(unsupported("an invalid attribute name"));
                }
                names.push(Symbol::from(name));
                continue;
            }
            let Some(hash) = arg.as_keyword_hash_node() else {
                return Err(unsupported("a computed attribute name"));
            };
            for element in hash.elements().iter() {
                let Some(assoc) = element.as_assoc_node() else {
                    return Err(unsupported("a keyword splat"));
                };
                let key = symbol_value(&assoc.key());
                let slot = match key.as_deref() {
                    Some("instance_reader") => 0,
                    Some("instance_writer") => 1,
                    Some("instance_accessor") => 2,
                    Some("default") => {
                        default = Some(seed_value(&assoc.value(), kw, file)?);
                        continue;
                    }
                    Some("location") => continue,
                    _ => return Err(unsupported("an unknown option")),
                };
                let value = assoc.value();
                flags[slot] = Some(if value.as_true_node().is_some() {
                    true
                } else if value.as_false_node().is_some() || value.as_nil_node().is_some() {
                    false
                } else {
                    return Err(unsupported("a computed option"));
                });
            }
        }
    }
    if let Some(block) = call.block() {
        // `(block_given? && default.nil?) ? yield : default`.
        let Some(block) = block.as_block_node() else { return Err(unsupported("a block argument")) };
        if block.parameters().is_some() {
            return Err(unsupported("a block with parameters"));
        }
        let body = match block.body() {
            Some(body) => seed_value(&body, kw, file)?,
            None => Expr::new(Span::synthetic(), ExprNode::Lit { value: Literal::Nil }),
        };
        if default.as_ref().is_none_or(is_nil) {
            default = Some(body);
        }
    }
    if names.is_empty() {
        return Err(unsupported("no attribute name"));
    }
    let accessor = flags[2].unwrap_or(true);
    Ok(Declaration {
        names,
        reader: kw.ends_with("_reader") || kw.ends_with("_accessor"),
        writer: kw.ends_with("_writer") || kw.ends_with("_accessor"),
        instance_reader: flags[0].unwrap_or(true) && accessor,
        instance_writer: flags[1].unwrap_or(true) && accessor,
        default,
    })
}

/// A default (or default block body) as the seed stores it. Survey
/// ingest stands `nil` in for a gap inside an expression and records it;
/// a seed holding that `nil` would store a value the source never
/// computes, so the declaration is refused instead (the gap stays
/// recorded). Strict ingest, so a trailing `#:` ascription is the value's.
fn seed_value(node: &ruby_prism::Node<'_>, kw: &str, file: &str) -> IngestResult<Expr> {
    let before = super::survey::recorded_len();
    let value = super::expr::ingest_expr_strict(node, file)?;
    if super::survey::is_active() && super::survey::recorded_len() != before {
        return Err(IngestError::Unsupported {
            file: file.into(),
            message: format!("`{kw}` with a default ingest could not read whole is not modeled"),
        });
    }
    Ok(value)
}

fn is_attribute_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c == '_' || c.is_ascii_alphabetic()) && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

fn is_nil(e: &Expr) -> bool {
    matches!(&*e.node, ExprNode::Lit { value: Literal::Nil })
}

impl Declaration {
    /// A declaration read some other way than from source (an included
    /// hook's `base.cattr_accessor`, `hook_class_attribute`).
    pub(crate) fn new(kw: &str, names: Vec<Symbol>, instance_reader: bool, instance_writer: bool, default: Option<Expr>) -> Self {
        Declaration {
            names,
            reader: kw.ends_with("_reader") || kw.ends_with("_accessor"),
            writer: kw.ends_with("_writer") || kw.ends_with("_accessor"),
            instance_reader,
            instance_writer,
            default,
        }
    }

    /// The default, unless absent or a literal nil.
    pub(crate) fn default(&self) -> Option<&Expr> {
        self.default.as_ref().filter(|d| !is_nil(d))
    }

    /// The accessors, class side first, as ActiveSupport defines them
    /// (public, whatever visibility is in effect around the call: its
    /// `module_eval` string starts its own scope).
    pub(crate) fn methods(&self, owner: &ClassId) -> Vec<MethodDef> {
        let mut out = Vec::new();
        for name in &self.names {
            if self.reader {
                out.push(reader(owner, name, MethodReceiver::Class));
                if self.instance_reader {
                    out.push(reader(owner, name, MethodReceiver::Instance));
                }
            }
            if self.writer {
                out.push(writer(owner, name, MethodReceiver::Class));
                if self.instance_writer {
                    out.push(writer(owner, name, MethodReceiver::Instance));
                }
            }
        }
        out
    }

    /// The class-body statements the declaration runs, at `span`.
    pub(crate) fn seeds(&self, span: Span) -> Vec<Expr> {
        let first = &self.names[0];
        self.names
            .iter()
            .enumerate()
            .map(|(i, name)| match &self.default {
                Some(default) if !is_nil(default) => {
                    let value = if i == 0 { default.clone() } else { var(span, first) };
                    assign(span, name, value)
                }
                _ => unset_seed(span, name),
            })
            .collect()
    }
}

/// `@@name = nil unless defined?(@@name)`.
pub(crate) fn unset_seed(span: Span, name: &Symbol) -> Expr {
    Expr::new(
        span,
        ExprNode::If {
            cond: Expr::new(span, ExprNode::Defined { operand: var(span, name) }),
            then_branch: Expr::new(span, ExprNode::Lit { value: Literal::Nil }),
            else_branch: assign(span, name, Expr::new(span, ExprNode::Lit { value: Literal::Nil })),
        },
    )
}

/// `@@name = value`.
pub(crate) fn assign(span: Span, name: &Symbol, value: Expr) -> Expr {
    Expr::new(span, ExprNode::Assign { target: LValue::Var { id: VarId(0), name: storage(name) }, value })
}

fn var(span: Span, name: &Symbol) -> Expr {
    Expr::new(span, ExprNode::Var { id: VarId(0), name: storage(name) })
}

/// `def (self.)name; @@name; end`.
pub(crate) fn reader(owner: &ClassId, name: &Symbol, receiver: MethodReceiver) -> MethodDef {
    let mut method = super::library_class::synth_attr_reader(owner, name, receiver);
    method.body = var(Span::synthetic(), name);
    method.visibility = MethodVisibility::Public;
    method
}

/// `def (self.)name=(value); @@name = value; end`.
pub(crate) fn writer(owner: &ClassId, name: &Symbol, receiver: MethodReceiver) -> MethodDef {
    let mut method = super::library_class::synth_attr_writer(owner, name, receiver);
    if let ExprNode::Assign { target, .. } = &mut *method.body.node {
        *target = LValue::Var { id: VarId(0), name: storage(name) };
    }
    method.visibility = MethodVisibility::Public;
    method
}

/// `name` when `method` is a generated class-side writer of `@@name`
/// (or of the `@name` other expansions store in).
pub(crate) fn written_storage(method: &MethodDef) -> Option<&str> {
    let base = method.name.as_str().strip_suffix('=')?;
    let [param] = method.params.as_slice() else { return None };
    let ExprNode::Assign { target, value } = &*method.body.node else { return None };
    let stored = match target {
        LValue::Ivar { name } => name.as_str(),
        LValue::Var { name, .. } => name.as_str().strip_prefix("@@")?,
        _ => return None,
    };
    let reads_param = matches!(&*value.node, ExprNode::Var { name, .. } if *name == param.name);
    (stored == base && reads_param).then_some(base)
}

/// `method` is an accessor [`reader`] or [`writer`] generated, whose
/// declaration its seed (a class-body statement at the declaration's own
/// span) stands for.
pub(crate) fn generated(method: &MethodDef) -> bool {
    method.name_span.is_synthetic()
        && match &*method.body.node {
            ExprNode::Var { name, .. } => name.as_str().strip_prefix("@@") == Some(method.name.as_str()),
            ExprNode::Assign { target: LValue::Var { name, .. }, .. } => {
                name.as_str().strip_prefix("@@").is_some_and(|bare| method.name.as_str().strip_suffix('=') == Some(bare))
            }
            _ => false,
        }
}

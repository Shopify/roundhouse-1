//! Class identities for literal Data.define constants on source library classes.

use crate::det::{HashMap, HashSet};

use crate::App;
use crate::expr::{Expr, ExprNode, Literal};
use crate::ident::{ClassId, Symbol};
use crate::span::Span;
use crate::ty::Ty;

use super::ClassInfo;
use super::body::ConstResolver;

pub(super) fn register(
    app: &App,
    resolver: &ConstResolver,
    classes: &mut HashMap<ClassId, ClassInfo>,
) -> HashMap<Span, Ty> {
    let mut factories = HashMap::default();
    register_factory_classes(app, resolver, classes);
    // Source overrides can replace `.define`; only the built-in factory is modeled here.
    if resolver.has_source_namespace("Data") {
        return factories;
    }
    let mut register = |owner: &ClassId, name: &Symbol, value: &Expr| {
        let ExprNode::Send {
            recv: Some(recv),
            method,
            args,
            block: None,
            ..
        } = &*value.node
        else {
            return;
        };
        let ExprNode::Const { path } = &*recv.node else {
            return;
        };
        if method.as_str() != "define" || !resolver.is_runtime_class(recv.span, path, "Data") {
            return;
        }
        let mut members = HashSet::default();
        for arg in args {
            let ExprNode::Lit {
                value: Literal::Sym { value: member },
            } = &*arg.node
            else {
                return;
            };
            if !reader_name(member.as_str()) || !members.insert(member.clone()) {
                return;
            }
        }
        let Some(id) = resolver.constant_class(value.span, name.as_str()) else {
            return;
        };
        // Rehomed constants are not emitted in their original source scope.
        if id.0.as_str() != format!("{}::{}", owner.0.as_str(), name.as_str())
            || classes.contains_key(&id)
        {
            return;
        }
        let instance = Ty::Class {
            id: id.clone(),
            args: vec![],
        };
        let mut info = ClassInfo::default();
        info.class_methods
            .insert(Symbol::from("new"), instance.clone());
        // A member declaration establishes a reader, not its value type.
        // Data has no generated writers.
        for member in members {
            info.instance_methods.insert(member, Ty::Untyped);
        }
        classes.insert(id, info);
        factories.insert(value.span, instance);
    };
    for class in &app.library_classes {
        for (name, value) in &class.constants {
            register(&class.name, name, value);
        }
    }
    factories
}

/// The members a `Struct.new` / `Data.define` factory with a body block
/// generates (`LibraryClassOrigin::FactoryClass`). The class itself is
/// a library class, registered with the block's own methods; this adds
/// what the factory defines around them: `new`, a reader per member,
/// and for Struct a writer per member. A method the block defines
/// itself keeps its own entry. Member values are untyped, as for the
/// blockless form.
fn register_factory_classes(
    app: &App,
    resolver: &ConstResolver,
    classes: &mut HashMap<ClassId, ClassInfo>,
) {
    for class in &app.library_classes {
        let Some(crate::dialect::LibraryClassOrigin::FactoryClass { factory }) = &class.origin else {
            continue;
        };
        let ExprNode::Send { recv: Some(recv), method, args, .. } = &*factory.node else {
            continue;
        };
        let ExprNode::Const { path } = &*recv.node else { continue };
        let generator = if method.as_str() == "new" { "Struct" } else { "Data" };
        // An app's own `Struct`/`Data` is not the factory modeled here.
        if resolver.has_source_namespace(generator)
            || !resolver.is_runtime_class(recv.span, path, generator)
        {
            continue;
        }
        let Some(info) = classes.get_mut(&class.name) else { continue };
        info.parent = Some(ClassId(Symbol::from(generator)));
        info.class_methods
            .entry(Symbol::from("new"))
            .or_insert_with(|| Ty::Class { id: class.name.clone(), args: vec![] });
        for arg in args {
            let ExprNode::Lit { value: Literal::Sym { value: member } } = &*arg.node else {
                continue;
            };
            if !reader_name(member.as_str()) {
                continue;
            }
            info.instance_methods.entry(member.clone()).or_insert(Ty::Untyped);
            if generator == "Struct" {
                info.instance_methods
                    .entry(Symbol::from(format!("{}=", member.as_str())))
                    .or_insert(Ty::Untyped);
            }
        }
    }
}

fn reader_name(member: &str) -> bool {
    let bare = member
        .strip_suffix('?')
        .or_else(|| member.strip_suffix('!'))
        .unwrap_or(member);
    let mut chars = bare.chars();
    chars
        .next()
        .is_some_and(|first| first == '_' || first.is_ascii_alphabetic())
        && chars.all(|character| character == '_' || character.is_ascii_alphanumeric())
}

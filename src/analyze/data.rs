//! Class identities for literal Data.define constants on source library classes.

use crate::det::{HashMap, HashSet};

use crate::App;
use crate::diagnostic::Diagnostic;
use crate::dialect::LibraryClassOrigin;
use crate::expr::{Expr, ExprNode, Literal, RESOLVED_DATA_FACTORY};
use crate::ident::{ClassId, Symbol};
use crate::span::Span;
use crate::ty::Ty;

use super::ClassInfo;
use super::body::ConstResolver;

/// Register class identities for built-in `Data.define` constants with distinct
/// literal members, retaining any lifted custom methods. Generated readers stay
/// untyped; rejected declarations are absent from the returned span-to-type map.
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
        let custom = app.library_classes.iter().any(|class| {
            class.name == id && matches!(class.origin,
                Some(LibraryClassOrigin::DataFactory { declaration_span }) if declaration_span == value.span)
        });
        // The main branch also ingests Struct block classes into the same
        // registry. Preserve those generated declarations when a factory
        // adds its constructor and readers.
        let reopened = app.library_classes.iter().any(|class| {
            class.name == id && !class.is_module && class.parent.is_none()
        });
        if id.0.as_str() != format!("{}::{}", owner.0.as_str(), name.as_str())
            || (classes.contains_key(&id) && !custom && !reopened)
            || (custom && app.library_classes.iter().filter(|class| class.name == id).count() != 1)
        {
            return;
        }
        let instance = Ty::Class {
            id: id.clone(),
            args: vec![].into(),
        };
        let mut info = classes.remove(&id).unwrap_or_default();
        info.class_methods
            .insert(Symbol::from("new"), instance.clone());
        info.declares_constructor = true;
        // A member declaration establishes a reader, not its value type.
        // Data has no generated writers. A block method of the same name
        // overrides the reader, as it does in Ruby.
        for member in members {
            info.instance_methods.entry(member).or_insert(Ty::Untyped);
        }
        info.instance_methods.entry(Symbol::from("with")).or_insert(instance.clone());
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
            .or_insert_with(|| Ty::Class { id: class.name.clone(), args: Default::default() });
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

/// `X = Struct.new(:a, :b)` and `X = Struct.new(:a, keyword_init: true)`
/// without a block, as a constant of a library class or a model
/// (Core's `IdentityAccount::IdentityAccountFields`). The class the
/// factory returns gets what `register_factory_classes` gives the block
/// form: `Struct` as parent, `new`, an untyped reader and writer per
/// member. The value stays the constructor's source expression; only
/// Data constants are typed through `factories`. Runs before signatures
/// resolve, so a signature in the owner can name the class bare.
pub(super) fn register_blockless_structs(
    app: &App,
    resolver: &ConstResolver,
    classes: &mut HashMap<ClassId, ClassInfo>,
) {
    if resolver.has_source_namespace("Struct") {
        return;
    }
    let mut register = |owner: &ClassId, name: &Symbol, value: &Expr| {
        let ExprNode::Send { recv: Some(recv), method, args, block: None, .. } = &*value.node else {
            return;
        };
        let ExprNode::Const { path } = &*recv.node else { return };
        if method.as_str() != "new" || !resolver.is_runtime_class(recv.span, path, "Struct") {
            return;
        }
        let (options, members) = match args.split_last() {
            Some((last, rest)) if matches!(&*last.node, ExprNode::Hash { .. }) => (Some(last), rest),
            _ => (None, args.as_slice()),
        };
        if let Some(options) = options {
            let ExprNode::Hash { entries, .. } = &*options.node else { return };
            let keyword_init = |(key, value): &(Expr, Expr)| {
                matches!(&*key.node, ExprNode::Lit { value: Literal::Sym { value } } if value.as_str() == "keyword_init")
                    && matches!(&*value.node, ExprNode::Lit { value: Literal::Bool { .. } | Literal::Nil })
            };
            if !entries.iter().all(keyword_init) {
                return;
            }
        }
        let mut seen = HashSet::default();
        let mut names = Vec::new();
        for arg in members {
            let ExprNode::Lit { value: Literal::Sym { value: member } } = &*arg.node else {
                return;
            };
            if !reader_name(member.as_str()) || !seen.insert(member.clone()) {
                return;
            }
            names.push(member.clone());
        }
        let Some(id) = resolver.constant_class(value.span, name.as_str()) else {
            return;
        };
        if id.0.as_str() != format!("{}::{}", owner.0.as_str(), name.as_str()) || classes.contains_key(&id) {
            return;
        }
        let mut info = ClassInfo::default();
        info.app_declared = true;
        info.parent = Some(ClassId(Symbol::from("Struct")));
        info.class_methods.insert(Symbol::from("new"), Ty::Class { id: id.clone(), args: Default::default() });
        for member in names {
            info.instance_methods.insert(Symbol::from(format!("{}=", member.as_str())), Ty::Untyped);
            info.instance_methods.insert(member, Ty::Untyped);
        }
        classes.insert(id, info);
    };
    for class in &app.library_classes {
        for (name, value) in &class.constants {
            register(&class.name, name, value);
        }
    }
    for model in &app.models {
        for item in &model.body {
            let crate::dialect::ModelBodyItem::Unknown { expr, .. } = item else { continue };
            if let ExprNode::Assign { target: crate::expr::LValue::Const { path }, value } = &*expr.node
                && let [name] = path.as_slice()
            {
                register(&model.name, name, value);
            }
        }
    }
}

/// Report lifted custom factories whose declaration was not admitted by analysis
/// or whose generated class is subclassed. Lifting methods alone does not establish
/// that the original factory belongs to the supported subset.
pub(super) fn diagnose(app: &App) -> Vec<Diagnostic> {
    if !app.library_classes.iter().any(|class| matches!(class.origin, Some(LibraryClassOrigin::DataFactory { .. }))) {
        return Vec::new();
    }
    let resolver = app.const_resolver.for_app(app);
    let mut diagnostics = Vec::new();
    for factory in &app.library_classes {
        let Some(LibraryClassOrigin::DataFactory { declaration_span }) = factory.origin else {
            continue;
        };
        let admitted = app.library_classes.iter().flat_map(|class| &class.constants)
            .any(|(_, value)| value.span == declaration_span && value.decisions & RESOLVED_DATA_FACTORY != 0);
        let subclassed = app.library_classes.iter().any(|class| {
            class.parent.as_ref().is_some_and(|parent| {
                let path: Vec<_> = parent.0.as_str().split("::").map(Symbol::from).collect();
                resolver.declaration_name(class.parent_span, &path) == Some(factory.name.0.as_str())
            })
        });
        if !admitted || subclassed {
            diagnostics.push(Diagnostic::unsupported(declaration_span, None, "Data.define",
                "custom Data factories require the built-in Data, distinct literal member names, and no class reopening or subclassing"));
        }
    }
    diagnostics
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

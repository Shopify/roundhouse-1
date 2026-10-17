//! The gem boundary: classes a locked gem's RBI declares, registered
//! so dispatch can answer for them ([`crate::gem_boundary`]).
//!
//! Purely additive. A class the registry already has -- the app's own,
//! the framework catalog's, the stdlib's -- is left exactly as it is:
//! an RBI describes what a gem defines, and a name the app or the
//! catalog also defines is a different (or a better-known) thing that
//! this table has no business overriding. Core classes an RBI merely
//! reopens (`class String`, `module Kernel`) are never registered for
//! the same reason: registering `Object` would turn every "ancestor we
//! don't model" gradual escape into a hard miss.
//!
//! Soundness is by pruning toward `untyped`:
//!
//! * a class name in a signature that resolves to no registered class
//!   becomes `untyped`, never a class nobody defined;
//! * a mixin that resolves to nothing marks the class `open`, so a
//!   method the declared surface lacks is unknown rather than an error;
//! * an app class that inherits a gem class keeps the gradual escape it
//!   always had (see `ClassInfo::gem_boundary`).

use crate::det::{HashMap, HashSet};

use crate::app::App;
use crate::ident::{ClassId, Symbol};
use crate::ty::Ty;

use super::super::body::ClassInfo;

pub(in crate::analyze) use crate::gem_boundary::{RUBY_CORE, RUBY_STDLIB};

/// Mixins that add no callable surface a caller could miss: sorbet's
/// own `extend T::Sig`, and the ActiveSupport::Concern marker.
fn is_marker(name: &str) -> bool {
    name.starts_with("T::") || name == "T" || name == "ActiveSupport::Concern"
}

pub(in crate::analyze) fn register(classes: &mut HashMap<ClassId, ClassInfo>, app: &App) {
    if app.gem_boundary.is_empty() {
        return;
    }
    let declared = &app.gem_boundary.classes;

    // Which declarations become classes: not core or stdlib, not already
    // known.
    let fresh: HashSet<&ClassId> = declared
        .keys()
        .filter(|id| {
            let name = id.0.as_str();
            let top = name.split("::").next().unwrap_or(name);
            !RUBY_CORE.contains(&top) && !RUBY_STDLIB.contains(&top) && !classes.contains_key(*id)
        })
        .collect();

    // Names resolve against the union of what was already known and
    // what is being added, so gem classes may refer to each other.
    let known = |id: &ClassId| classes.contains_key(id) || fresh.contains(id);
    let resolve = |written: &ClassId, scope: &str| -> Option<ClassId> {
        // Tapioca writes absolute names (`::Money::Currency`), so the
        // name as written is tried first; a relative name (hand-written
        // shims) then resolves lexically from the class it appears in.
        if known(written) {
            return Some(written.clone());
        }
        let mut parts: Vec<&str> = scope.split("::").filter(|s| !s.is_empty()).collect();
        while !parts.is_empty() {
            let candidate = ClassId(Symbol::new(&format!("{}::{}", parts.join("::"), written.0.as_str())));
            if known(&candidate) {
                return Some(candidate);
            }
            parts.pop();
        }
        None
    };
    let prune = |ty: &Ty, scope: &str| -> Ty { prune_ty(ty, &|id| resolve(id, scope)) };

    // A namespace the catalog already nests a class in (`ActiveRecord`,
    // which activemodel-serializers-xml reopens) exists without the gem;
    // only a name the gem alone introduces is the gem's to load.
    let framework_namespaces: HashSet<&str> = classes
        .iter()
        .filter(|(_, info)| !info.app_declared && !info.gem_boundary)
        .flat_map(|(id, _)| id.0.as_str().match_indices("::").map(|(i, _)| &id.0.as_str()[..i]))
        .collect();

    // First pass: build every ClassInfo with resolved types and the
    // instance-method surface of each declared mixin.
    let mut built: HashMap<ClassId, ClassInfo> = HashMap::default();
    let mut open: HashSet<ClassId> = HashSet::default();
    for id in &fresh {
        let gem = &declared[*id];
        let scope = id.0.as_str();
        let owner = (!framework_namespaces.contains(scope)).then(|| gem.gem.clone());
        let gem_loaded = owner.as_deref().is_some_and(|g| app.gem_boundary.loads(g));
        let mut info = ClassInfo { gem_boundary: true, gem: owner, gem_loaded, ..ClassInfo::default() };
        for (name, ty) in &gem.instance_methods {
            info.instance_methods.insert(name.clone(), prune(ty, scope));
        }
        for (name, ty) in &gem.class_methods {
            info.class_methods.insert(name.clone(), prune(ty, scope));
        }
        for name in &gem.constants {
            info.constants.insert(name.clone(), Ty::Untyped);
        }
        let mut is_open = gem.dynamic;
        if let Some(parent) = &gem.parent {
            // Unresolved: left as written. Dispatch already reads a
            // named ancestor the registry lacks as "inherited surface we
            // don't model", which is exactly what it is.
            info.parent = Some(resolve(parent, scope).unwrap_or_else(|| parent.clone()));
        }
        for include in &gem.includes {
            match resolve(include, scope) {
                Some(found) => info.includes.push(found),
                None if is_marker(include.0.as_str()) => {}
                None => is_open = true,
            }
        }
        if is_open {
            open.insert((*id).clone());
        }
        built.insert((*id).clone(), info);
    }

    // `extend M` makes M's instance methods this class's class-side
    // surface. Copied from the declared table (a gem module) or the
    // registry (a catalog module); an `M` neither knows leaves the
    // class-side surface unenumerable.
    for id in &fresh {
        let gem = &declared[*id];
        let scope = id.0.as_str();
        for extend in &gem.extends {
            let Some(target) = resolve(extend, scope) else {
                if !is_marker(extend.0.as_str()) {
                    open.insert((*id).clone());
                }
                continue;
            };
            let methods: Vec<(Symbol, Ty)> = match (built.get(&target), classes.get(&target)) {
                (Some(info), _) => info.instance_methods.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
                (None, Some(info)) => info.instance_methods.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
                (None, None) => Vec::new(),
            };
            if let Some(info) = built.get_mut(*id) {
                for (name, ty) in methods {
                    info.class_methods.entry(name).or_insert(ty);
                }
            }
        }
    }

    // A class that includes an open module is open, transitively.
    loop {
        let mut grew = false;
        for (id, info) in &built {
            if open.contains(id) {
                continue;
            }
            if info.includes.iter().any(|m| open.contains(m)) {
                open.insert(id.clone());
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }

    // An app declaration of a class or module a loaded gem defines is a
    // REOPEN: Ruby runs its body on the gem's own class object (Core's
    // essentials liquid_ext.rb reopens `Liquid::Render` to prepend a
    // patch and rebuild `SYNTAX` from the gem's `Liquid::Render::FOR`).
    // So the app's source is that body, not the whole class: the gem's
    // constants, methods, superclass and mixins are the class's too,
    // wherever the app's declaration does not speak, and the class is
    // the gem's, so a tree that does not load the gem refuses it. A
    // declaration that names another superclass is not a reopen (Ruby
    // raises a superclass mismatch) and stays the app's alone.
    struct GemReopen {
        id: ClassId,
        gem: String,
        parent: Option<ClassId>,
        includes: Vec<ClassId>,
        instance_methods: Vec<(Symbol, Ty)>,
        class_methods: Vec<(Symbol, Ty)>,
        constants: Vec<Symbol>,
        open: bool,
    }
    let mut gem_reopens: Vec<GemReopen> = Vec::new();
    let app_names: HashSet<&ClassId> = app
        .models
        .iter()
        .map(|m| &m.name)
        .chain(app.controllers.iter().map(|c| &c.name))
        .chain(app.library_classes.iter().map(|lc| &lc.name))
        .collect();
    // A namespace the catalog nests a class in exists without the gem
    // (`framework_namespaces` above also counts the app's own nesting,
    // which is no evidence: Core nests `Liquid::Render`'s patch modules
    // in the gem's `Liquid`).
    let catalog_namespace = |scope: &str| {
        let inside = format!("{scope}::");
        classes.iter().any(|(id, info)| !info.gem_boundary && !app_names.contains(id) && id.0.as_str().starts_with(&inside))
    };
    for lc in &app.library_classes {
        if lc.origin.is_some() || gem_reopens.iter().any(|r| r.id == lc.name) {
            continue;
        }
        let Some(gem) = app.gem_boundary.loaded_reopen(&lc.name).filter(|gem| gem.is_module == lc.is_module) else { continue };
        let scope = lc.name.0.as_str();
        if !classes.contains_key(&lc.name) || catalog_namespace(scope) {
            continue;
        }
        let parent = gem.parent.as_ref().map(|p| resolve(p, scope).unwrap_or_else(|| p.clone()));
        if lc.parent.is_some() && lc.parent != parent {
            continue;
        }
        let mut open = gem.dynamic;
        let mut includes = Vec::new();
        for include in &gem.includes {
            match resolve(include, scope) {
                Some(found) => includes.push(found),
                None if is_marker(include.0.as_str()) => {}
                None => open = true,
            }
        }
        gem_reopens.push(GemReopen {
            id: lc.name.clone(),
            gem: gem.gem.clone(),
            parent,
            includes,
            instance_methods: gem.instance_methods.iter().map(|(n, ty)| (n.clone(), prune(ty, scope))).collect(),
            class_methods: gem.class_methods.iter().map(|(n, ty)| (n.clone(), prune(ty, scope))).collect(),
            constants: gem.constants.clone(),
            open,
        });
    }

    for (id, mut info) in built {
        info.open = open.contains(&id);
        classes.insert(id, info);
    }

    // A class the catalog or stdlib registers and an RBI also describes
    // (`Rack::Utils`, `Rack::Mime`) gains the RBI's methods it lacks. The
    // registered entry wins wherever both speak, and a class the app
    // itself declares is never touched: its source is the whole truth.
    let app_owned: HashSet<&ClassId> = app
        .models
        .iter()
        .map(|m| &m.name)
        .chain(app.controllers.iter().map(|c| &c.name))
        .chain(app.library_classes.iter().map(|lc| &lc.name))
        .collect();
    let reopened: Vec<&ClassId> = declared
        .keys()
        .filter(|id| !fresh.contains(*id) && !app_owned.contains(*id) && classes.contains_key(*id))
        .filter(|id| !app.extensions.is_rails_dependent(&declared[*id].gem))
        .filter(|id| {
            let name = id.0.as_str();
            !RUBY_CORE.contains(&name.split("::").next().unwrap_or(name))
        })
        .collect();
    for id in reopened {
        let gem = &declared[id];
        let scope = id.0.as_str();
        let known = |c: &ClassId| classes.contains_key(c);
        let mut add_instance = Vec::new();
        let mut add_class = Vec::new();
        for (name, ty) in &gem.instance_methods {
            add_instance.push((name.clone(), prune_ty(ty, &|c| resolve_in(&known, c, scope))));
        }
        for (name, ty) in &gem.class_methods {
            add_class.push((name.clone(), prune_ty(ty, &|c| resolve_in(&known, c, scope))));
        }
        if let Some(info) = classes.get_mut(id) {
            for (name, ty) in add_instance {
                info.instance_methods.entry(name).or_insert(ty);
            }
            for (name, ty) in add_class {
                info.class_methods.entry(name).or_insert(ty);
            }
        }
    }

    for reopen in gem_reopens {
        let Some(info) = classes.get_mut(&reopen.id) else { continue };
        let is_open = reopen.open || info.includes.iter().chain(&reopen.includes).any(|m| open.contains(m));
        for (name, ty) in reopen.instance_methods {
            info.instance_methods.entry(name).or_insert(ty);
        }
        for (name, ty) in reopen.class_methods {
            info.class_methods.entry(name).or_insert(ty);
        }
        for name in reopen.constants {
            info.constants.entry(name).or_insert(Ty::Untyped);
        }
        if info.parent.is_none() {
            info.parent = reopen.parent;
        }
        for include in reopen.includes {
            if !info.includes.contains(&include) {
                info.includes.push(include);
            }
        }
        info.open |= is_open;
        info.gem = Some(reopen.gem);
        info.gem_loaded = true;
    }
}

/// `written` as a registered class, from `scope` outward.
fn resolve_in(known: &dyn Fn(&ClassId) -> bool, written: &ClassId, scope: &str) -> Option<ClassId> {
    if known(written) {
        return Some(written.clone());
    }
    let mut parts: Vec<&str> = scope.split("::").filter(|s| !s.is_empty()).collect();
    while !parts.is_empty() {
        let candidate = ClassId(Symbol::new(&format!("{}::{}", parts.join("::"), written.0.as_str())));
        if known(&candidate) {
            return Some(candidate);
        }
        parts.pop();
    }
    None
}

/// `ty` with every class name resolved through `resolve`; a name that
/// resolves to nothing becomes `untyped`.
fn prune_ty(ty: &Ty, resolve: &dyn Fn(&ClassId) -> Option<ClassId>) -> Ty {
    match ty {
        Ty::Class { id, args } => match resolve(id) {
            Some(id) => Ty::Class {
                id,
                args: args.iter().map(|a| prune_ty(a, resolve)).collect(),
            },
            None => Ty::Untyped,
        },
        Ty::Array { elem } => Ty::Array { elem: std::sync::Arc::new(prune_ty(elem, resolve)) },
        Ty::Hash { key, value } => Ty::Hash {
            key: std::sync::Arc::new(prune_ty(key, resolve)),
            value: std::sync::Arc::new(prune_ty(value, resolve)),
        },
        Ty::Tuple { elems } => Ty::Tuple { elems: elems.iter().map(|t| prune_ty(t, resolve)).collect() },
        Ty::Union { variants } => {
            Ty::Union { variants: variants.iter().map(|t| prune_ty(t, resolve)).collect() }
        }
        Ty::Fn { params, block, ret, effects } => Ty::Fn {
            params: params
                .iter()
                .map(|p| crate::ty::Param { name: p.name.clone(), ty: prune_ty(&p.ty, resolve).into(), kind: p.kind.clone() })
                .collect(),
            block: block.as_ref().map(|b| std::sync::Arc::new(prune_ty(b, resolve))),
            ret: std::sync::Arc::new(prune_ty(ret, resolve)),
            effects: effects.clone(),
        },
        other => other.clone(),
    }
}

//! `alias to from` / `alias_method :to, :from` naming a method an app
//! ancestor defines rather than the class body itself.
//!
//! graphql-client 0.25.0's `OperationDefinition < Definition` does
//! `alias operation_name definition_name`, with `definition_name`
//! defined in definition.rb. A class body only knows its own methods
//! as it is walked, so the support walk has the ingest capture such an
//! alias as the call it is ([`capturing`];
//! `library_class::walk_decl_body_in`) and holds the file back. Once
//! the walk is done, [`decide`] looks each original up the way Ruby
//! does when the alias runs (the class's own includes, latest first,
//! then its superclass's chain, each class's methods before its
//! includes): a file whose every such alias resolves is carried, one
//! with an original no app ancestor defines (a gem method:
//! `alias role? has_role?` on a model) is refused as the ingest refuses
//! it elsewhere.
//!
//! After the merges and the passes that make methods, [`resolve`] turns
//! each resolved alias into the copy an alias of a method of the body's
//! own is (`MethodAlias`), marked `inherited`: analysis sees an ordinary
//! method, and the Ruby family writes the native `alias_method`, which
//! Ruby resolves through the ancestors as the source's alias did. The
//! copy keeps the original's visibility, as Ruby's alias does. A source
//! `alias_method` naming a method no app ancestor defines stays the
//! captured call it always was.

use std::cell::{Cell, RefCell};

use crate::app::App;
use crate::dialect::{LibraryClass, MethodAlias, MethodDef, MethodReceiver};
use crate::expr::{Expr, ExprNode, Literal};
use crate::ident::ClassId;
use crate::span::Span;

use super::IngestError;

thread_local! {
    static CAPTURING: Cell<bool> = const { Cell::new(false) };
    static DEFERRED: RefCell<Vec<Span>> = const { RefCell::new(Vec::new()) };
}

/// The error a keyword `alias` naming a method nothing defines refuses
/// its file with.
pub(super) fn refusal(file: &str) -> IngestError {
    IngestError::Unsupported { file: file.into(), message: "alias names a method this body has not defined".into() }
}

/// Runs `ingest` with keyword `alias`es of methods the body does not
/// define captured (and deferred) rather than refused. Returns its
/// result with the deferred aliases' spans.
pub(super) fn capturing<T>(ingest: impl FnOnce() -> T) -> (T, Vec<Span>) {
    struct Reset(bool);
    impl Drop for Reset {
        fn drop(&mut self) {
            CAPTURING.with(|c| c.set(self.0));
        }
    }
    let before = DEFERRED.with(|d| d.borrow().len());
    let reset = Reset(CAPTURING.with(|c| c.replace(true)));
    let out = ingest();
    drop(reset);
    let deferred = DEFERRED.with(|d| d.borrow_mut().split_off(before));
    (out, deferred)
}

pub(super) fn is_capturing() -> bool {
    CAPTURING.with(Cell::get)
}

/// Note a captured alias (`span`, its statement) for [`capturing`].
pub(super) fn defer(span: Span) {
    DEFERRED.with(|d| d.borrow_mut().push(span));
}

/// Of the held-back files (each its classes and its deferred aliases'
/// spans), the ones whose every deferred alias names a method an app
/// ancestor defines: among `app`'s classes, the file's own, and those
/// of the files already found carried. In the files' order; the rest
/// are refused.
pub(super) fn decide(app: &[LibraryClass], held: &[(Vec<LibraryClass>, Vec<Span>)]) -> Vec<bool> {
    let mut carried = vec![false; held.len()];
    loop {
        let mut changed = false;
        for (i, (classes, spans)) in held.iter().enumerate() {
            if carried[i] {
                continue;
            }
            let mut candidates: Vec<&LibraryClass> = app.iter().collect();
            for (j, (others, _)) in held.iter().enumerate() {
                if carried[j] || j == i {
                    candidates.extend(others.iter());
                }
            }
            let resolves = spans.iter().all(|span| {
                classes.iter().any(|lc| {
                    lc.unknown_calls.iter().any(|call| {
                        call.span == *span
                            && alias_call(call).is_some_and(|(receiver, _, from)| {
                                ancestor_method(&candidates, lc, from, receiver).is_some()
                                    || core_ancestor_defines(&candidates, lc, from, receiver)
                            })
                    })
                })
            });
            if resolves {
                carried[i] = true;
                changed = true;
            }
        }
        if !changed {
            return carried;
        }
    }
}

pub(super) fn resolve(app: &mut App) {
    // (class index, index of the captured call, the copy, the original's
    // name, whether the original is an ancestor's)
    let mut resolved: Vec<(usize, usize, MethodDef, crate::Symbol, bool)> = Vec::new();
    {
        let candidates: Vec<&LibraryClass> = app.library_classes.iter().collect();
        for (i, lc) in app.library_classes.iter().enumerate() {
            for (j, call) in lc.unknown_calls.iter().enumerate() {
                let Some((receiver, to, from)) = alias_call(call) else { continue };
                let own: Vec<&MethodDef> =
                    lc.methods.iter().filter(|m| m.name.as_str() == from && m.receiver == receiver).collect();
                if !own.is_empty() {
                    // The body's own def, where the alias runs after it:
                    // one a later pass made from a body macro written
                    // above the alias (frozen_record 0.27.4's Scope
                    // `delegate ..., :each, to: :to_a` then `alias_method
                    // :find_each, :each`), or one the walk met in another
                    // file of the class. The copy is that def, as for an
                    // alias of a def the walk had already met; the Ruby
                    // family writes the native alias after it. A def only
                    // below the alias in its file is not the one it names:
                    // Ruby aliases the method as it stands there, an
                    // ancestor's (frozen_record's `alias_method :load,
                    // :new` above its own `def new`, inside `class <<
                    // self`), and the call stays captured, which the Ruby
                    // family replays above the defs
                    // (`emit::ruby::library::aliases_ancestor_method`).
                    if let Some(original) = own.iter().rev().find(|m| defined_before(m, call.span)) {
                        let mut copy = (*original).clone();
                        copy.name = crate::Symbol::from(to);
                        resolved.push((i, j, copy, original.name.clone(), false));
                    }
                    continue;
                }
                if let Some(original) = ancestor_method(&candidates, lc, from, receiver) {
                    let mut copy = original.clone();
                    copy.name = crate::Symbol::from(to);
                    resolved.push((i, j, copy, original.name.clone(), true));
                }
            }
        }
    }
    for i in 0..app.library_classes.len() {
        let mine: Vec<&(usize, usize, MethodDef, crate::Symbol, bool)> = resolved.iter().filter(|r| r.0 == i).collect();
        if mine.is_empty() {
            continue;
        }
        let spans: Vec<crate::span::Span> = mine.iter().map(|r| app.library_classes[i].unknown_calls[r.1].span).collect();
        let lc = &mut app.library_classes[i];
        let mut at = 0;
        lc.unknown_calls.retain(|_| {
            at += 1;
            !mine.iter().any(|r| r.1 == at - 1)
        });
        for ((_, _, copy, original, inherited), span) in mine.into_iter().zip(spans) {
            lc.method_aliases.push(MethodAlias {
                name: copy.name.clone(),
                receiver: copy.receiver,
                original: original.clone(),
                inherited: *inherited,
                span,
            });
            lc.methods.push(copy.clone());
        }
    }
}

/// Whether the body's def `m` runs before the statement at `at`: one a
/// pass made (no source position: a body macro's), or one written in
/// another file of the class, or above `at` in its file.
fn defined_before(m: &MethodDef, at: Span) -> bool {
    m.name_span.is_synthetic() || at.is_synthetic() || m.name_span.file != at.file || m.name_span.start < at.start
}

/// `alias_method :to, :from` as the ingest captured it: bare for an
/// instance method, on `singleton_class` for a class method.
fn alias_call(call: &Expr) -> Option<(MethodReceiver, &str, &str)> {
    let ExprNode::Send { recv, method, args, block: None, .. } = &*call.node else { return None };
    if method.as_str() != "alias_method" {
        return None;
    }
    let receiver = match recv {
        None => MethodReceiver::Instance,
        Some(recv) => match &*recv.node {
            ExprNode::Send { recv: None, method, args, block: None, .. }
                if method.as_str() == "singleton_class" && args.is_empty() =>
            {
                MethodReceiver::Class
            }
            _ => return None,
        },
    };
    let [to, from] = args.as_slice() else { return None };
    Some((receiver, symbol(to)?, symbol(from)?))
}

fn symbol(arg: &Expr) -> Option<&str> {
    match &*arg.node {
        ExprNode::Lit { value: Literal::Sym { value } } => Some(value.as_str()),
        _ => None,
    }
}

fn named<'a>(classes: &[&'a LibraryClass], id: &str) -> Vec<&'a LibraryClass> {
    classes.iter().copied().filter(|c| c.name.0.as_str() == id).collect()
}

/// The first definition of `name` on `receiver`'s side along `lc`'s
/// app ancestors, in Ruby's method resolution order.
fn ancestor_method<'a>(app: &[&'a LibraryClass], lc: &LibraryClass, name: &str, receiver: MethodReceiver) -> Option<&'a MethodDef> {
    let own = |id: &str| -> Option<&'a MethodDef> {
        named(app, id).into_iter().flat_map(|c| c.methods.iter()).filter(|m| m.name.as_str() == name && m.receiver == receiver).last()
    };
    let included = |id: &str| -> Option<&'a MethodDef> {
        if receiver != MethodReceiver::Instance {
            return None;
        }
        let includes: Vec<&ClassId> = named(app, id).into_iter().flat_map(|c| c.includes.iter()).collect();
        includes.into_iter().rev().find_map(|module| {
            app.iter()
                .filter(|c| c.is_module && c.name == *module)
                .flat_map(|c| c.methods.iter())
                .filter(|m| m.name.as_str() == name && m.receiver == MethodReceiver::Instance)
                .last()
        })
    };
    if let Some(found) = included(lc.name.0.as_str()) {
        return Some(found);
    }
    let mut seen: Vec<String> = vec![lc.name.0.as_str().to_string()];
    let mut class = lc.name.0.as_str().to_string();
    let mut parent = named(app, &class).into_iter().find_map(|c| c.parent.clone());
    while let Some(written) = parent {
        let resolved = superclass(app, &class, written.0.as_str())?;
        if seen.contains(&resolved) || seen.len() > 32 {
            return None;
        }
        if let Some(found) = own(&resolved).or_else(|| included(&resolved)) {
            return Some(found);
        }
        seen.push(resolved.clone());
        parent = named(app, &resolved).into_iter().filter(|c| !c.is_module).find_map(|c| c.parent.clone());
        class = resolved;
    }
    None
}

/// Whether `lc` (any of its reopenings) or an app superclass of it
/// defines `name` on `receiver`'s side.
pub(crate) fn answers(app: &[LibraryClass], lc: &LibraryClass, name: &str, receiver: MethodReceiver) -> bool {
    let candidates: Vec<&LibraryClass> = app.iter().collect();
    named(&candidates, lc.name.0.as_str())
        .into_iter()
        .chain(std::iter::once(lc))
        .any(|c| c.methods.iter().any(|m| m.name.as_str() == name && m.receiver == receiver))
        || ancestor_method(&candidates, lc, name, receiver).is_some()
}

/// A captured `alias_method` of `lc` that [`decide`] carried for a core
/// ancestor's method: `lc` defines no method of the original's name on
/// that side, and its superclass chain reaches a core class that does
/// (`core_ancestor_defines`). The Ruby family replays it as written
/// (`emit::ruby::library::modelled_class_body_calls`); Ruby aliases the
/// core method there.
pub(crate) fn aliases_core_method(app: &[LibraryClass], lc: &LibraryClass, call: &Expr) -> bool {
    let Some((receiver, _, from)) = alias_call(call) else { return false };
    if lc.methods.iter().any(|m| m.name.as_str() == from && m.receiver == receiver) {
        return false;
    }
    let candidates: Vec<&LibraryClass> = app.iter().collect();
    ancestor_method(&candidates, lc, from, receiver).is_none() && core_ancestor_defines(&candidates, lc, from, receiver)
}

/// No app ancestor defines `name`, but a core class `lc` inherits from
/// does, by the vendored signatures (`stdlib_rbs::core_instance_method`):
/// activemerchant 1.137.0's `class Errors < Hash; alias count size`. The
/// first class along `lc`'s superclasses the app does not declare, or
/// one it reopens that core declares (`Object` when the chain ends
/// unwritten), answers for the rest of the chain. The captured
/// `alias_method` stays the call it is, and Ruby resolves it there.
/// Instance methods only.
fn core_ancestor_defines(app: &[&LibraryClass], lc: &LibraryClass, name: &str, receiver: MethodReceiver) -> bool {
    if receiver != MethodReceiver::Instance {
        return false;
    }
    let mut seen: Vec<String> = vec![lc.name.0.as_str().to_string()];
    let mut class = lc.name.0.as_str().to_string();
    let mut parent = named(app, &class).into_iter().filter(|c| !c.is_module).find_map(|c| c.parent.clone());
    if parent.is_none() && named(app, &class).iter().all(|c| !c.is_module) {
        return crate::stdlib_rbs::core_instance_method("Object", name);
    }
    while let Some(written) = parent {
        let Some(resolved) = superclass(app, &class, written.0.as_str()) else {
            return crate::stdlib_rbs::core_instance_method(written.0.as_str(), name);
        };
        if crate::stdlib_rbs::declares_core_namespace(&resolved) {
            return crate::stdlib_rbs::core_instance_method(&resolved, name);
        }
        if seen.contains(&resolved) || seen.len() > 32 {
            return false;
        }
        seen.push(resolved.clone());
        parent = named(app, &resolved).into_iter().filter(|c| !c.is_module).find_map(|c| c.parent.clone());
        if parent.is_none() {
            return crate::stdlib_rbs::core_instance_method("Object", name);
        }
        class = resolved;
    }
    false
}

/// The library class `written` names as `class`'s superclass: looked
/// up from the namespace the `class` statement is in outwards, then at
/// the top level.
fn superclass(app: &[&LibraryClass], class: &str, written: &str) -> Option<String> {
    let written = written.trim_start_matches("::");
    let scope: Vec<&str> = class.split("::").collect();
    let is_class = |name: &str| app.iter().any(|c| !c.is_module && c.name.0.as_str() == name);
    (0..scope.len())
        .rev()
        .map(|depth| if depth == 0 { written.to_string() } else { format!("{}::{written}", scope[..depth].join("::")) })
        .find(|candidate| is_class(candidate))
}

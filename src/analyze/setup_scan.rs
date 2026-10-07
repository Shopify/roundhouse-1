//! The two app-wide tree scans the library pass's setup runs every
//! round, kept per library class across rounds.
//!
//! `current_attribute_writes` (`Current.user = user` value types) and
//! `harvest_mailer_with_params` (`SomeMailer.with(k: v)` value types)
//! read the typed trees of every model, controller, view and library
//! class. In the late rounds of a large app only a few hundred library
//! classes are re-typed, so each round walked forty thousand unchanged
//! classes for the same hits. Both scans are split into an ordered list
//! of hits per tree and a merge that replays them; a library class's
//! hits are kept with the trees print they were taken at and reused
//! while its trees are unchanged. Every other source is walked again.
//! Replaying the same hits in the same order builds the same maps, so
//! the result is the full scan's.
//!
//! The hit and merge steps restate `harvest_mailer_with_params` and
//! `collect_const_attr_writes`, and the walk order restates theirs
//! (`for_each_hook_body_ref`, then views). `ROUNDHOUSE_INCREMENTAL=0`
//! runs those originals; `ROUNDHOUSE_INCREMENTAL=check` runs them too
//! and fails the analysis if either result differs.

use crate::det::{HashMap, HashSet};
use crate::dialect::LibraryClass;
use crate::expr::{Expr, ExprNode};
use crate::ident::{ClassId, Symbol};
use crate::ty::Ty;
use crate::App;

/// A `.with` call on a mailer: the mailer, then each typed `key: value`.
pub(super) type MailerHit = (ClassId, Option<(Symbol, Ty)>);
/// A `Target.attr = value` write with a closed value type.
pub(super) type AttrHit = (ClassId, Symbol, Ty);

/// One library class's hits, as its trees were when they were taken.
struct LibraryHits {
    name: ClassId,
    /// `None`: taken without a print, walked again next round.
    print: Option<u64>,
    mailer: Vec<MailerHit>,
    writes: Vec<AttrHit>,
}

#[derive(Default)]
pub(super) struct SetupScan {
    library: Vec<Option<LibraryHits>>,
}

fn const_id(path: &[Symbol]) -> ClassId {
    ClassId(Symbol::from(path.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("::").as_str()))
}

/// `harvest_mailer_with_params`'s per-node test, as hits.
pub(super) fn mailer_hits(e: &Expr, mailers: &HashSet<ClassId>, out: &mut Vec<MailerHit>) {
    use crate::expr::Literal;
    if let ExprNode::Send { recv: Some(recv), method, args, .. } = &*e.node {
        if method.as_str() == "with" {
            if let ExprNode::Const { path } = &*recv.node {
                let id = const_id(path);
                if mailers.contains(&id) {
                    out.push((id.clone(), None));
                    for arg in args {
                        let ExprNode::Hash { entries, .. } = &*arg.node else { continue };
                        for (k, v) in entries {
                            let ExprNode::Lit { value: Literal::Sym { value: key } } = &*k.node else { continue };
                            let Some(ty) = v.ty.clone() else { continue };
                            if matches!(ty, Ty::Var { .. }) {
                                continue;
                            }
                            out.push((id.clone(), Some((key.clone(), ty))));
                        }
                    }
                }
            }
        }
    }
    e.node.for_each_child(&mut |c| mailer_hits(c, mailers, out));
}

pub(super) fn merge_mailer_hits(out: &mut HashMap<ClassId, crate::ty::Row>, hits: &[MailerHit]) {
    for (id, field) in hits {
        let row = out.entry(id.clone()).or_insert_with(crate::ty::Row::closed);
        if let Some((key, ty)) = field {
            let merged = match row.fields.get(key) {
                Some(prev) => crate::analyze::body::union_of(prev.clone(), ty.clone()),
                None => ty.clone(),
            };
            row.fields.insert(key.clone(), merged);
        }
    }
}

/// `collect_const_attr_writes`'s per-node test, as hits.
pub(super) fn attr_hits(e: &Expr, targets: &HashSet<&ClassId>, out: &mut Vec<AttrHit>) {
    // `Current.user = bot` arrives as a SEND of `user=`, not as an
    // `Assign` — prism spells a receiver-ful attribute write as a call,
    // and only the compound forms (`||=`) become `LValue::Attr`.
    if let ExprNode::Send { recv: Some(recv), method, args, block: None, .. } = &*e.node {
        if let (Some(attr), 1, ExprNode::Const { path }) =
            (method.as_str().strip_suffix('='), args.len(), &*recv.node)
        {
            if let Some(target) = targets.get(&const_id(path)) {
                if let Some(ty) = args[0].ty.as_ref().filter(|t| !t.is_open()) {
                    out.push(((*target).clone(), Symbol::from(attr), ty.clone()));
                }
            }
        }
    }
    e.node.for_each_child(&mut |c| attr_hits(c, targets, out));
}

pub(super) fn merge_attr_hits(out: &mut HashMap<ClassId, HashMap<Symbol, Ty>>, hits: &[AttrHit]) {
    for (target, name, ty) in hits {
        let entry = out.entry(target.clone()).or_default();
        let merged = match entry.remove(name) {
            Some(prev) if prev == *ty => prev,
            Some(prev) => crate::analyze::body::union_of(prev, ty.clone()),
            None => ty.clone(),
        };
        entry.insert(name.clone(), merged);
    }
}

/// The trees `for_each_hook_body_ref` visits for one library class, in
/// its order.
fn library_hook_bodies(lc: &LibraryClass, f: &mut impl FnMut(&Expr)) {
    for method in &lc.methods {
        for p in &method.params {
            if let Some(default) = &p.default {
                f(default);
            }
        }
        f(&method.body);
    }
    for (_name, value) in &lc.constants {
        f(value);
    }
    for call in &lc.unknown_calls {
        f(call);
    }
    for initializer in &lc.class_ivar_initializers {
        f(initializer);
    }
}

/// The trees `for_each_hook_body_ref` visits before the library
/// classes (`models`) or after them (`!models`), in its order.
fn other_hook_bodies(app: &App, models: bool, f: &mut impl FnMut(&Expr)) {
    fn defaults(params: &[crate::dialect::Param], f: &mut impl FnMut(&Expr)) {
        for p in params {
            if let Some(default) = &p.default {
                f(default);
            }
        }
    }
    if models {
        for model in &app.models {
            for item in &model.body {
                match item {
                    crate::dialect::ModelBodyItem::Method { method, .. } => {
                        defaults(&method.params, f);
                        f(&method.body)
                    }
                    crate::dialect::ModelBodyItem::Scope { scope, .. } => {
                        defaults(&scope.params, f);
                        f(&scope.body)
                    }
                    crate::dialect::ModelBodyItem::Callback { callback, .. } => {
                        if let Some(cond) = &callback.condition {
                            f(cond);
                        }
                    }
                    crate::dialect::ModelBodyItem::Unknown { expr, .. } => f(expr),
                    crate::dialect::ModelBodyItem::Association {
                        assoc: crate::dialect::Association::HasMany { extension, .. },
                        ..
                    } => {
                        for m in extension {
                            defaults(&m.params, f);
                            f(&m.body);
                        }
                    }
                    _ => {}
                }
            }
            for default in model.class_attr_defaults.values() {
                f(default);
            }
        }
        return;
    }
    if let Some(lc) = &app.rails_application {
        library_hook_bodies(lc, f);
    }
    for controller in &app.controllers {
        for item in &controller.body {
            match item {
                crate::dialect::ControllerBodyItem::Action { action, .. } => {
                    for (_name, default) in &action.opt_params {
                        f(default);
                    }
                    f(&action.body)
                }
                crate::dialect::ControllerBodyItem::ClassMethod { method, .. } => {
                    defaults(&method.params, f);
                    f(&method.body)
                }
                crate::dialect::ControllerBodyItem::Unknown { expr, .. } => f(expr),
                crate::dialect::ControllerBodyItem::Filter { filter, .. } => {
                    if let Some(c) = &filter.if_cond_expr {
                        f(c);
                    }
                    if let Some(c) = &filter.unless_cond_expr {
                        f(c);
                    }
                }
                _ => {}
            }
        }
    }
    if let Some(seeds) = &app.seeds {
        f(seeds);
    }
}

impl SetupScan {
    /// Both scans, reusing the hits of each library class whose trees
    /// print `prints[i]` is the one they were taken at (`None`: walk).
    pub(super) fn scan(
        &mut self,
        app: &App,
        prints: &[Option<u64>],
        mailers: &HashSet<ClassId>,
        targets: &HashSet<&ClassId>,
    ) -> (HashMap<ClassId, crate::ty::Row>, HashMap<ClassId, HashMap<Symbol, Ty>>, usize) {
        if self.library.len() != app.library_classes.len() {
            self.library = (0..app.library_classes.len()).map(|_| None).collect();
        }
        let mut walked = 0usize;
        for ((lc, slot), print) in app.library_classes.iter().zip(self.library.iter_mut()).zip(prints) {
            let kept =
                slot.as_ref().is_some_and(|hits| hits.name == lc.name && hits.print.is_some() && hits.print == *print);
            if kept {
                continue;
            }
            walked += 1;
            let mut mailer = Vec::new();
            for method in &lc.methods {
                mailer_hits(&method.body, mailers, &mut mailer);
            }
            let mut writes = Vec::new();
            if !targets.is_empty() {
                library_hook_bodies(lc, &mut |body| attr_hits(body, targets, &mut writes));
            }
            *slot = Some(LibraryHits { name: lc.name.clone(), print: *print, mailer, writes });
        }

        let mut mailer_hits_all = Vec::new();
        for model in &app.models {
            for method in model.methods() {
                mailer_hits(&method.body, mailers, &mut mailer_hits_all);
            }
        }
        for controller in &app.controllers {
            for action in controller.actions() {
                mailer_hits(&action.body, mailers, &mut mailer_hits_all);
            }
        }
        for hits in self.library.iter().flatten() {
            mailer_hits_all.extend(hits.mailer.iter().cloned());
        }
        let mut mailer_out = HashMap::default();
        merge_mailer_hits(&mut mailer_out, &mailer_hits_all);

        let mut attr_out = HashMap::default();
        if !targets.is_empty() {
            let mut writes = Vec::new();
            other_hook_bodies(app, true, &mut |body| attr_hits(body, targets, &mut writes));
            for hits in self.library.iter().flatten() {
                writes.extend(hits.writes.iter().cloned());
            }
            other_hook_bodies(app, false, &mut |body| attr_hits(body, targets, &mut writes));
            for view in &app.views {
                attr_hits(&view.body, targets, &mut writes);
            }
            merge_attr_hits(&mut attr_out, &writes);
        }
        (mailer_out, attr_out, walked)
    }
}

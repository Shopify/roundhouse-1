//! Call-site parameter observations kept across fixpoint rounds.
//!
//! `unify_params_from_call_sites` rebuilds the parameter table every
//! round from the call sites in every typed tree. In the late rounds of
//! a large app only a few hundred classes are re-typed, yet every site
//! in the app was collected, placed on its `def`'s slots and unified
//! again (seconds per round on Shopify Core). This module keeps each
//! source tree's placed sites, sorted by the key they are recorded
//! under, and recomputes only the keys a changed source contributes to.
//!
//! The result is the table a full rebuild gives. A key's slots are a
//! fold of its sites in collection order, and keys are independent, so
//! refolding a key from its contributing sources in source order (each
//! source's sites for that key in their own order) reproduces it. A
//! library class whose trees print is unchanged since its sites were
//! collected is not walked again: its sites are a function of its
//! typed trees, its name and the static helper index. Every other
//! source is walked and compared. Where a site is placed reads the
//! registry's class set and each class's parent and includes; the
//! cache starts over when those differ from the round it was built in.
//!
//! `ROUNDHOUSE_INCREMENTAL=0` uses the full rebuild every round;
//! `ROUNDHOUSE_INCREMENTAL=check` also runs the full rebuild and fails
//! the analysis if the tables differ.

use std::collections::BTreeSet;

use crate::det::HashMap;

use super::{incremental, observe_into, Analyzer, ParamSite, ParamTables, UnifyScope};
use crate::app::ParamKey;
use crate::dialect::MethodReceiver;
use crate::ident::{ClassId, Symbol};
use crate::ty::Ty;
use crate::App;

/// One tree group `unify_params_from_call_sites` walks, in its order.
#[derive(Clone, PartialEq, Eq, Debug)]
enum Unit {
    Model(usize, ClassId),
    Library(usize, ClassId),
    Controller(usize, ClassId),
    View(usize),
    Seeds,
}

struct Source {
    unit: Unit,
    /// For a library class: the trees print its sites were collected at.
    print: Option<u64>,
    /// Placed sites, stably sorted by key.
    sites: Vec<(ParamKey, Vec<Ty>)>,
    /// The `(receiver class, method)` pairs this source's class calls,
    /// for `Analyzer::callers_by_target` (see [`call_targets`]).
    callers: Vec<(ClassId, Symbol)>,
}

#[derive(Default)]
pub(super) struct SiteCache {
    sources: Vec<Source>,
    /// The table the sites fold to (before the concern fold).
    table: HashMap<ParamKey, Vec<Ty>>,
    /// Key → indices of the sources with a site for it, ascending.
    contributors: HashMap<ParamKey, Vec<u32>>,
    /// [`placement_print`] of the registry the sites were placed against.
    placement: Option<(usize, u64)>,
}

/// What site placement (`inherited_param_owner`, `lexical_parent`) reads
/// of the registry: which classes exist and each one's parent and
/// includes. Classes are never removed, so the count and an
/// order-independent sum of per-class hashes identify it.
fn placement_print(classes: &HashMap<ClassId, crate::analyze::body::ClassInfo>) -> (usize, u64) {
    use std::hash::{Hash, Hasher};
    let mut sum = 0u64;
    for (id, info) in classes {
        let mut h = std::hash::DefaultHasher::new();
        id.hash(&mut h);
        info.parent.hash(&mut h);
        info.includes.hash(&mut h);
        sum = sum.wrapping_add(h.finish());
    }
    (classes.len(), sum)
}

fn units(app: &App) -> Vec<Unit> {
    let mut units = Vec::with_capacity(
        app.models.len() + app.library_classes.len() + app.controllers.len() + app.views.len() + 1,
    );
    units.extend(app.models.iter().enumerate().map(|(i, m)| Unit::Model(i, m.name.clone())));
    units.extend(app.library_classes.iter().enumerate().map(|(i, lc)| Unit::Library(i, lc.name.clone())));
    units.extend(app.controllers.iter().enumerate().map(|(i, c)| Unit::Controller(i, c.name.clone())));
    units.extend((0..app.views.len()).map(Unit::View));
    units.push(Unit::Seeds);
    units
}

/// The class a unit's sends are made from: `None` for views and seeds,
/// whose sends have no owning production class.
fn unit_class(unit: &Unit) -> Option<&ClassId> {
    match unit {
        Unit::Model(_, name) | Unit::Library(_, name) | Unit::Controller(_, name) => Some(name),
        Unit::View(_) | Unit::Seeds => None,
    }
}

/// The distinct `(receiver class, method)` pairs of a unit's raw sites,
/// in first-seen order. A class's calls to ITSELF are dropped: when its
/// own answer moves it is already dirty, and implicit-self sends are
/// most of what a body contains.
fn call_targets(caller: &ClassId, raw: &[ParamSite]) -> Vec<(ClassId, Symbol)> {
    let mut seen: crate::det::HashSet<(&ClassId, &Symbol)> = crate::det::HashSet::default();
    let mut out = Vec::new();
    for (class_id, method, _, _, _) in raw {
        if class_id != caller && seen.insert((class_id, method)) {
            out.push((class_id.clone(), method.clone()));
        }
    }
    out
}

/// The distinct keys of a key-sorted site list.
fn keys(sites: &[(ParamKey, Vec<Ty>)]) -> Vec<&ParamKey> {
    let mut out: Vec<&ParamKey> = Vec::new();
    for (key, _) in sites {
        if out.last() != Some(&key) {
            out.push(key);
        }
    }
    out
}

/// A key-sorted site list's run for `key`.
fn run<'s>(sites: &'s [(ParamKey, Vec<Ty>)], key: &ParamKey) -> &'s [(ParamKey, Vec<Ty>)] {
    let start = sites.partition_point(|(k, _)| k < key);
    let len = sites[start..].partition_point(|(k, _)| k == key);
    &sites[start..start + len]
}

impl Analyzer {
    /// Collect one unit's call sites, as the full walk does.
    fn collect_unit_sites(&self, app: &App, unit: &Unit, scope: UnifyScope, out: &mut Vec<ParamSite>) {
        let helpers = &app.helper_method_index;
        match unit {
            Unit::Model(i, _) => {
                let model = &app.models[*i];
                for method in model.methods() {
                    self.collect_send_sites(&method.body, Some(&model.name), method.receiver, helpers, out);
                }
                for scope_item in model.scopes() {
                    self.collect_send_sites(&scope_item.body, Some(&model.name), MethodReceiver::Class, helpers, out);
                }
            }
            Unit::Library(i, _) => {
                let lc = &app.library_classes[*i];
                for method in &lc.methods {
                    self.collect_send_sites(&method.body, Some(&lc.name), method.receiver, helpers, out);
                }
            }
            Unit::Controller(i, _) => {
                let controller = &app.controllers[*i];
                for action in controller.actions() {
                    self.collect_send_sites(&action.body, Some(&controller.name), MethodReceiver::Instance, helpers, out);
                }
                for method in controller.class_methods() {
                    self.collect_send_sites(&method.body, Some(&controller.name), MethodReceiver::Class, helpers, out);
                }
                // A class-body macro call (`preload_site_configs %w[a], only:
                // :show`) is the call site that types the class method.
                for item in &controller.body {
                    if let crate::dialect::ControllerBodyItem::ClassIvarInit { expr, .. } = item {
                        self.collect_send_sites(expr, Some(&controller.name), MethodReceiver::Class, helpers, out);
                    }
                }
            }
            Unit::View(i) => {
                if matches!(scope, UnifyScope::WithViews) {
                    self.collect_send_sites(&app.views[*i].body, None, MethodReceiver::Instance, helpers, out);
                }
            }
            Unit::Seeds => {
                if let (UnifyScope::WithViews, Some(seeds)) = (scope, &app.seeds) {
                    self.collect_send_sites(seeds, None, MethodReceiver::Instance, helpers, out);
                }
            }
        }
    }

    /// Every call site `scope` covers, in walk order.
    pub(super) fn collect_param_sites(&self, app: &App, scope: UnifyScope) -> Vec<ParamSite> {
        self.collect_param_sites_and_callers(app, scope).0
    }

    /// [`Self::collect_param_sites`], plus each class unit's
    /// [`call_targets`].
    pub(super) fn collect_param_sites_and_callers(
        &self,
        app: &App,
        scope: UnifyScope,
    ) -> (Vec<ParamSite>, Vec<(ClassId, Vec<(ClassId, Symbol)>)>) {
        let mut sites = Vec::new();
        let mut callers = Vec::new();
        for unit in units(app) {
            let from = sites.len();
            self.collect_unit_sites(app, &unit, scope, &mut sites);
            if let Some(caller) = unit_class(&unit) {
                callers.push((caller.clone(), call_targets(caller, &sites[from..])));
            }
        }
        (sites, callers)
    }

    /// Bring the cached site table up to date with the current trees and
    /// set `inferred_params` to it: the table a full collect-and-apply of
    /// `scope` builds.
    pub(super) fn apply_param_sites_incrementally(&mut self, app: &App, scope: UnifyScope, tables: &ParamTables) {
        let mut cache = std::mem::take(&mut self.site_cache);
        let units = units(app);
        let placement = Some(placement_print(&self.classes));
        let restart = cache.placement != placement
            || cache.sources.len() != units.len()
            || cache.sources.iter().zip(&units).any(|(s, u)| s.unit != *u);
        if restart {
            cache = SiteCache {
                sources: units.into_iter().map(|unit| Source { unit, print: None, sites: Vec::new(), callers: Vec::new() }).collect(),
                placement,
                ..SiteCache::default()
            };
        }
        let collect = crate::timings::begin("unify: collect");
        let mut placed = HashMap::default();
        let mut affected: BTreeSet<ParamKey> = BTreeSet::new();
        let (mut walked, mut changed) = (0usize, 0usize);
        for (index, source) in cache.sources.iter_mut().enumerate() {
            let print = match &source.unit {
                Unit::Library(i, name) => self.incremental.trees_of(*i, name),
                _ => None,
            };
            if print.is_some() && source.print == print {
                continue;
            }
            walked += 1;
            let mut raw = Vec::new();
            self.collect_unit_sites(app, &source.unit, scope, &mut raw);
            source.callers = unit_class(&source.unit).map(|caller| call_targets(caller, &raw)).unwrap_or_default();
            let mut sites = self.place_param_sites(raw, &tables.shapes, &tables.defined, &mut placed);
            sites.sort_by(|a, b| a.0.cmp(&b.0));
            source.print = print;
            if sites == source.sites {
                continue;
            }
            changed += 1;
            let old = std::mem::replace(&mut source.sites, sites);
            let index = index as u32;
            let (old_keys, new_keys) = (keys(&old), keys(&source.sites));
            for key in &old_keys {
                if new_keys.binary_search(key).is_err() {
                    if let Some(list) = cache.contributors.get_mut(*key) {
                        if let Ok(at) = list.binary_search(&index) {
                            list.remove(at);
                        }
                    }
                    affected.insert((*key).clone());
                } else if run(&old, key) != run(&source.sites, key) {
                    affected.insert((*key).clone());
                }
            }
            for key in &new_keys {
                if old_keys.binary_search(key).is_err() {
                    let list = cache.contributors.entry((*key).clone()).or_default();
                    if let Err(at) = list.binary_search(&index) {
                        list.insert(at, index);
                    }
                    affected.insert((*key).clone());
                }
            }
        }
        drop(collect);
        for source in &cache.sources {
            if let Some(caller) = unit_class(&source.unit) {
                self.record_callers(caller, &source.callers);
            }
        }
        let _apply = crate::timings::begin("unify: apply");
        for key in &affected {
            let mut entry: Option<Vec<Ty>> = None;
            for &source in cache.contributors.get(key).map(|l| l.as_slice()).unwrap_or(&[]) {
                for (_, arg_tys) in run(&cache.sources[source as usize].sites, key) {
                    let slots = entry.get_or_insert_with(|| {
                        (0..arg_tys.len()).map(|_| Ty::Var { var: crate::ident::TyVar(0) }).collect()
                    });
                    observe_into(slots, arg_tys.clone());
                }
            }
            match entry {
                Some(slots) => {
                    cache.table.insert(key.clone(), slots);
                }
                None => {
                    cache.table.remove(key);
                    cache.contributors.remove(key);
                }
            }
        }
        crate::timings::note(format_args!(
            "unify sites: walked {walked} of {} sources, {changed} changed, {} keys refolded, restarted {restart}",
            cache.sources.len(),
            affected.len()
        ));
        if self.incremental.mode == incremental::Mode::Check {
            let mut fresh: HashMap<ParamKey, Vec<Ty>> = HashMap::default();
            let sites = self.collect_param_sites(app, scope);
            let mut memo = HashMap::default();
            for (key, arg_tys) in self.place_param_sites(sites, &tables.shapes, &tables.defined, &mut memo) {
                super::observe_param_site(&mut fresh, key, arg_tys);
            }
            if fresh != cache.table {
                let differing = fresh
                    .iter()
                    .filter(|(k, v)| cache.table.get(*k) != Some(*v))
                    .map(|(k, _)| k)
                    .chain(cache.table.keys().filter(|k| !fresh.contains_key(*k)))
                    .min()
                    .cloned();
                panic!(
                    "ROUNDHOUSE_INCREMENTAL=check: the cached call-site table differs from a full rebuild (first key {:?}: full {:?}, cached {:?})",
                    differing,
                    differing.as_ref().and_then(|k| fresh.get(k)),
                    differing.as_ref().and_then(|k| cache.table.get(k)),
                );
            }
        }
        self.site_cache = cache;
        self.inferred_params.clone_from(&self.site_cache.table);
    }
}

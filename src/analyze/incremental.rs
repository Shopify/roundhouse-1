//! Incremental re-typing for the library pass of the whole-program
//! fixpoint.
//!
//! Each round of the fixpoint used to re-type every library class
//! against the registry the round before refined. On Shopify Core the
//! late rounds change a few thousand signature entries, and a library
//! class none of whose registry reads changed is typed to the same
//! trees again. This module keeps, per library class, what its
//! typing read ([`super::body::DepRecorder`]) and the round it was
//! typed, and a change stamp for each registry entry. The library pass
//! skips a class whose reads are all older than its typing, provided
//! that typing left the class's own trees as it found them (typing
//! reads them too; see [`trees_print`]).
//!
//! The registry changes between passes are found by comparing a 64-bit
//! structural hash of each entry with the one the previous pass saw
//! (cloning the registry each pass cost more than the typing it saved).
//! Inside a pass the only registry write is the mailer `params` row,
//! which is stamped where it happens.
//!
//! `ROUNDHOUSE_INCREMENTAL=0` turns the skip off (every class is typed
//! every pass, the previous behavior). `ROUNDHOUSE_INCREMENTAL=check`
//! types every class but compares each class the skip would have kept
//! against its re-typed trees, reports each difference, and fails the
//! analysis if there was one: the oracle for the skip rule.

use std::collections::{HashMap, HashSet};

use rubydex::model::identity_maps::IdentityHashMap;
use rubydex::model::ids::DeclarationId;

use crate::dialect::LibraryClass;
use crate::expr::Expr;
use crate::ident::{ClassId, Symbol};
use crate::ty::Ty;

use std::hash::Hasher;

use super::body::{ClassInfo, DepKey, Mix};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Mode {
    /// Type every class every pass.
    Off,
    /// Skip classes whose reads did not change.
    On,
    /// Type every class; check that the skipped ones would be unchanged.
    Check,
}

impl Mode {
    pub(super) fn from_env() -> Self {
        match std::env::var("ROUNDHOUSE_INCREMENTAL").as_deref() {
            Ok("0") | Ok("off") | Ok("false") => Mode::Off,
            Ok("check") => Mode::Check,
            _ => Mode::On,
        }
    }
}

/// Hashes as the [`super::body::DepKey`] hashes are, with the same mix.
fn mix() -> Mix {
    Mix::default()
}

fn hash_symbol(h: &mut Mix, s: &Symbol) {
    h.write(s.as_str().as_bytes());
}

/// A structural hash of `ty` that agrees with `Ty`'s `PartialEq`: equal
/// types hash equal (a record's fields in any order).
fn hash_ty(h: &mut Mix, ty: &Ty) {
    match ty {
        Ty::Int => h.write_u8(1),
        Ty::Float => h.write_u8(2),
        Ty::Bool => h.write_u8(3),
        Ty::Str => h.write_u8(4),
        Ty::Sym => h.write_u8(5),
        Ty::Date => h.write_u8(6),
        Ty::Time => h.write_u8(7),
        Ty::Nil => h.write_u8(8),
        Ty::Relation { of } => {
            h.write_u8(9);
            hash_symbol(h, &of.0);
        }
        Ty::Array { elem } => {
            h.write_u8(10);
            hash_ty(h, elem);
        }
        Ty::Hash { key, value } => {
            h.write_u8(11);
            hash_ty(h, key);
            hash_ty(h, value);
        }
        Ty::Tuple { elems } => {
            h.write_u8(12);
            h.write_u64(elems.len() as u64);
            for t in elems {
                hash_ty(h, t);
            }
        }
        Ty::Record { row } => {
            h.write_u8(13);
            let mut fields = 0u64;
            for (name, t) in &row.fields {
                let mut f = mix();
                hash_symbol(&mut f, name);
                hash_ty(&mut f, t);
                fields = fields.wrapping_add(f.finish());
            }
            h.write_u64(fields);
            h.write_u64(row.fields.len() as u64);
            h.write_u64(row.rest.map_or(u64::MAX, |v| u64::from(v.0)));
        }
        Ty::Union { variants } => {
            h.write_u8(14);
            h.write_u64(variants.len() as u64);
            for t in variants {
                hash_ty(h, t);
            }
        }
        Ty::SelfInstance => h.write_u8(15),
        Ty::Class { id, args } => {
            h.write_u8(16);
            hash_symbol(h, &id.0);
            h.write_u64(args.len() as u64);
            for t in args {
                hash_ty(h, t);
            }
        }
        Ty::Fn { params, block, ret, effects } => {
            h.write_u8(17);
            h.write_u64(params.len() as u64);
            for p in params {
                hash_symbol(h, &p.name);
                hash_ty(h, &p.ty);
                h.write(format!("{:?}", p.kind).as_bytes());
            }
            match block {
                Some(b) => hash_ty(h, b),
                None => h.write_u8(0),
            }
            hash_ty(h, ret);
            if !effects.is_pure() {
                h.write(format!("{effects:?}").as_bytes());
            }
        }
        Ty::Var { var } => {
            h.write_u8(18);
            h.write_u64(u64::from(var.0));
        }
        Ty::Untyped => h.write_u8(19),
        Ty::Bottom => h.write_u8(20),
    }
}

fn ty_print(ty: &Ty) -> u64 {
    let mut h = mix();
    hash_ty(&mut h, ty);
    h.finish()
}

fn tys_print(tys: &[Ty]) -> u64 {
    let mut h = mix();
    h.write_u64(tys.len() as u64);
    for t in tys {
        hash_ty(&mut h, t);
    }
    h.finish()
}

fn entry_print(name: &Symbol, ty: &Ty) -> u64 {
    let mut h = mix();
    hash_symbol(&mut h, name);
    hash_ty(&mut h, ty);
    h.finish()
}

/// Order-independent hash of a method table.
fn table_print(table: &HashMap<Symbol, Ty>) -> u64 {
    table
        .iter()
        .fold(table.len() as u64, |acc, (m, t)| acc.wrapping_add(entry_print(m, t)))
}

fn set_print(set: &HashSet<Symbol>) -> u64 {
    set.iter().fold(set.len() as u64, |acc, m| {
        let mut h = mix();
        hash_symbol(&mut h, m);
        acc.wrapping_add(h.finish())
    })
}

/// The fields of a class entry the fixpoint rewrites besides its method
/// tables (see `harvest_block_value_methods`, `harvest_one_model`).
fn structure_print(info: &ClassInfo) -> u64 {
    let mut h = mix();
    h.write_u64(set_print(&info.block_value_methods));
    h.write_u64(set_print(&info.relation_derived));
    h.write_u64(set_print(&info.materializing_scopes));
    h.finish()
}

/// What typing wrote into a library class's trees: every expression's
/// type, decision bits and diagnostic.
pub(super) fn trees_print(lc: &LibraryClass) -> u64 {
    fn walk(h: &mut Mix, e: &Expr) {
        match &e.ty {
            Some(t) => hash_ty(h, t),
            None => h.write_u8(0),
        }
        h.write_u64(e.decisions);
        if let Some(d) = &e.diagnostic {
            h.write(format!("{d:?}").as_bytes());
        }
        e.node.for_each_child(&mut |c| walk(h, c));
    }
    let mut h = mix();
    for e in &lc.class_ivar_initializers {
        walk(&mut h, e);
    }
    for (_, e) in &lc.constants {
        walk(&mut h, e);
    }
    for m in &lc.methods {
        for p in &m.params {
            if let Some(d) = &p.default {
                walk(&mut h, d);
            }
        }
        walk(&mut h, &m.body);
    }
    h.finish()
}

/// One method table as the last pass saw it: the whole-table hash, and
/// the per-method hashes once the table has changed (to name what did).
#[derive(Default)]
struct TablePrint {
    whole: u64,
    methods: Option<HashMap<Symbol, u64>>,
}

impl TablePrint {
    fn per_method(table: &HashMap<Symbol, Ty>) -> HashMap<Symbol, u64> {
        table.iter().map(|(m, t)| (m.clone(), ty_print(t))).collect()
    }
}

/// The registry as the last pass saw it, as hashes.
#[derive(Default)]
struct Prints {
    /// (instance table, class-method table, structure) per class.
    classes: HashMap<ClassId, (TablePrint, TablePrint, u64)>,
    params: HashMap<(ClassId, Symbol), u64>,
    constants: IdentityHashMap<DeclarationId, u64>,
}

/// What one library class's last typing read and was seeded with.
struct Memo {
    name: ClassId,
    typed_at: u64,
    /// [`trees_print`] after that typing.
    trees: u64,
    /// Whether that typing left the trees as it found them. Typing
    /// reads the class's own earlier trees (a local Hash's value type
    /// from the values its `[]=` writes were last typed to, a sibling
    /// method's last return), so a class whose typing still moved its
    /// trees types differently once more even when no registry read
    /// changed.
    settled: bool,
    deps: Vec<u64>,
    self_id: ClassId,
    concern_env: Option<HashMap<Symbol, Ty>>,
    attribute_writes: Option<HashMap<Symbol, Ty>>,
}

/// Counts for one library pass.
#[derive(Default, Clone, Copy)]
pub(super) struct PassStats {
    pub(super) typed: usize,
    /// Classes whose reads did not change: skipped, or under `check`
    /// typed and compared.
    pub(super) clean: usize,
    pub(super) changed_keys: usize,
    pub(super) mismatched: usize,
    /// Typed classes whose trees the typing moved (typed again next pass).
    pub(super) unsettled: usize,
}

pub(super) struct Incremental {
    pub(super) mode: Mode,
    /// Advanced once per pass and once per in-pass registry write.
    tick: u64,
    /// Registry read ([`DepKey::hash64`]) → tick of its last change.
    changed: IdentityHashMap<u64, u64>,
    prints: Option<Prints>,
    memo: Vec<Option<Memo>>,
    pub(super) pass: PassStats,
    passes: usize,
    pub(super) mismatches_total: usize,
}

impl Incremental {
    pub(super) fn new(mode: Mode) -> Self {
        Self {
            mode,
            tick: 0,
            changed: IdentityHashMap::default(),
            prints: None,
            memo: Vec::new(),
            pass: PassStats::default(),
            passes: 0,
            mismatches_total: 0,
        }
    }

    fn stamp(&mut self, key: DepKey<'_>) {
        self.changed.insert(key.hash64(), self.tick);
        self.pass.changed_keys += 1;
    }

    /// Stamp every registry entry that differs from the snapshot the last
    /// pass left. Call before the pass types anything.
    pub(super) fn begin_pass(
        &mut self,
        classes: &HashMap<ClassId, ClassInfo>,
        params: &HashMap<(ClassId, Symbol), Vec<Ty>>,
        constants: &IdentityHashMap<DeclarationId, Ty>,
        classes_in_app: usize,
    ) {
        self.pass = PassStats::default();
        self.tick += 1;
        if self.memo.len() != classes_in_app {
            self.memo.resize_with(classes_in_app, || None);
        }
        if self.mode == Mode::Off {
            return;
        }
        let first = self.prints.is_none();
        let mut prints = self.prints.take().unwrap_or_default();
        let mut stamps: Vec<u64> = Vec::new();
        let mut stamp = |key: DepKey<'_>| stamps.push(key.hash64());
        for (id, info) in classes {
            let (inst, cls, structure) = match prints.classes.get_mut(id) {
                Some(entry) => entry,
                None => {
                    // A class new since the last pass: its entry, and every
                    // method an earlier read found missing.
                    if !first {
                        stamp(DepKey::Class(id));
                        stamp(DepKey::AllInstance(id));
                        for m in info.instance_methods.keys() {
                            stamp(DepKey::Instance(id, m));
                        }
                        for m in info.class_methods.keys() {
                            stamp(DepKey::ClassMethod(id, m));
                        }
                    }
                    prints.classes.insert(
                        id.clone(),
                        (
                            TablePrint { whole: table_print(&info.instance_methods), methods: None },
                            TablePrint { whole: table_print(&info.class_methods), methods: None },
                            structure_print(info),
                        ),
                    );
                    continue;
                }
            };
            for (print, table, instance_side) in
                [(inst, &info.instance_methods, true), (cls, &info.class_methods, false)]
            {
                let whole = table_print(table);
                if whole == print.whole {
                    continue;
                }
                let key = |m| if instance_side { DepKey::Instance(id, m) } else { DepKey::ClassMethod(id, m) };
                if instance_side {
                    stamp(DepKey::AllInstance(id));
                }
                let now = TablePrint::per_method(table);
                match &print.methods {
                    Some(before) => {
                        for (m, t) in &now {
                            if before.get(m) != Some(t) {
                                stamp(key(m));
                            }
                        }
                        for m in before.keys() {
                            if !now.contains_key(m) {
                                stamp(key(m));
                            }
                        }
                    }
                    // The table's first change: which methods it had is not
                    // kept, so stamp the class entry. Every method read
                    // goes through the class entry, so this covers them.
                    None => stamp(DepKey::Class(id)),
                }
                print.whole = whole;
                print.methods = Some(now);
            }
            let now = structure_print(info);
            if now != *structure {
                stamp(DepKey::Class(id));
                *structure = now;
            }
        }
        if prints.classes.len() != classes.len() {
            prints.classes.retain(|id, _| {
                let kept = classes.contains_key(id);
                if !kept {
                    stamp(DepKey::Class(id));
                    stamp(DepKey::AllInstance(id));
                }
                kept
            });
        }
        let mut param_prints = HashMap::with_capacity(params.len());
        for ((c, m), tys) in params {
            let key = (c.clone(), m.clone());
            let now = tys_print(tys);
            if !first && prints.params.get(&key) != Some(&now) {
                stamp(DepKey::Params(c, m));
            }
            param_prints.insert(key, now);
        }
        for (c, m) in prints.params.keys() {
            if !param_prints.contains_key(&(c.clone(), m.clone())) {
                stamp(DepKey::Params(c, m));
            }
        }
        prints.params = param_prints;
        let mut constant_prints = IdentityHashMap::default();
        for (id, ty) in constants {
            let now = ty_print(ty);
            if !first && prints.constants.get(id) != Some(&now) {
                stamp(DepKey::Constant(id));
            }
            constant_prints.insert(*id, now);
        }
        for id in prints.constants.keys() {
            if !constant_prints.contains_key(id) {
                stamp(DepKey::Constant(id));
            }
        }
        prints.constants = constant_prints;
        self.prints = Some(prints);
        self.pass.changed_keys += stamps.len();
        for key in stamps {
            self.changed.insert(key, self.tick);
        }
    }

    /// A registry write inside the pass (a mailer's `params` row): later
    /// classes in this pass see it, earlier ones next pass.
    pub(super) fn note_write(&mut self, class: &ClassId, method: &Symbol, created: bool) {
        if self.mode == Mode::Off {
            return;
        }
        self.tick += 1;
        if created {
            self.stamp(DepKey::Class(class));
        }
        self.stamp(DepKey::Instance(class, method));
        self.stamp(DepKey::AllInstance(class));
    }

    /// `ROUNDHOUSE_INCREMENTAL=check`: a class the skip rule called clean
    /// typed to different trees. Names the first differing member.
    pub(super) fn report_mismatch(&mut self, kept: &LibraryClass, now: &LibraryClass) {
        self.pass.mismatched += 1;
        if self.mismatches_total + self.pass.mismatched > 40 {
            return;
        }
        let member = kept
            .methods
            .iter()
            .zip(&now.methods)
            .find(|(a, b)| a != b)
            .map(|(a, _)| format!("#{}", a.name.as_str()))
            .or_else(|| (kept.class_ivar_initializers != now.class_ivar_initializers).then(|| " class ivar initializers".to_string()))
            .or_else(|| (kept.constants != now.constants).then(|| " constants".to_string()))
            .unwrap_or_else(|| " (other field)".to_string());
        eprintln!(
            "roundhouse-incremental-check: library pass {}: {}{member} typed differently than its kept trees",
            self.passes + 1,
            now.name.0.as_str()
        );
        if let Some((a, b)) = kept.methods.iter().zip(&now.methods).find(|(a, b)| a != b) {
            let (a, b) = (format!("{a:?}"), format!("{b:?}"));
            let at = a.bytes().zip(b.bytes()).position(|(x, y)| x != y).unwrap_or(a.len().min(b.len()));
            let from = a[..at].char_indices().rev().nth(600).map_or(0, |(i, _)| i);
            let clip = |s: &str| s.get(from..).map(|s| s.chars().take(1400).collect::<String>()).unwrap_or_default();
            eprintln!("roundhouse-incremental-check:   kept: ...{}", clip(&a));
            eprintln!("roundhouse-incremental-check:   now:  ...{}", clip(&b));
        }
    }

    /// Whether class `index` would type to the trees it already has.
    pub(super) fn is_clean(
        &self,
        index: usize,
        name: &ClassId,
        self_id: &ClassId,
        trees: u64,
        concern_env: Option<&HashMap<Symbol, Ty>>,
        attribute_writes: Option<&HashMap<Symbol, Ty>>,
    ) -> bool {
        if self.mode == Mode::Off {
            return false;
        }
        let Some(Some(memo)) = self.memo.get(index) else { return false };
        memo.name == *name
            && memo.settled
            && memo.trees == trees
            && memo.self_id == *self_id
            && memo.concern_env.as_ref() == concern_env
            && memo.attribute_writes.as_ref() == attribute_writes
            && memo.deps.iter().all(|d| self.changed.get(d).is_none_or(|t| *t <= memo.typed_at))
    }

    pub(super) fn record(
        &mut self,
        index: usize,
        name: &ClassId,
        self_id: &ClassId,
        deps: Vec<u64>,
        trees_before: u64,
        trees_after: u64,
        concern_env: Option<&HashMap<Symbol, Ty>>,
        attribute_writes: Option<&HashMap<Symbol, Ty>>,
    ) {
        self.pass.typed += 1;
        if trees_before != trees_after {
            self.pass.unsettled += 1;
        }
        if self.mode == Mode::Off {
            return;
        }
        if let Some(slot) = self.memo.get_mut(index) {
            *slot = Some(Memo {
                name: name.clone(),
                typed_at: self.tick,
                trees: trees_after,
                settled: trees_before == trees_after,
                deps,
                self_id: self_id.clone(),
                concern_env: concern_env.cloned(),
                attribute_writes: attribute_writes.cloned(),
            });
        }
    }

    /// Report the pass.
    pub(super) fn end_pass(&mut self) {
        self.passes += 1;
        self.mismatches_total += self.pass.mismatched;
        let PassStats { typed, clean, changed_keys, mismatched, unsettled } = self.pass;
        crate::timings::note(format_args!(
            "library pass {} ({:?}): typed {typed} ({unsettled} moved their own trees), clean {clean}, changed registry reads {changed_keys}, check mismatches {mismatched}",
            self.passes, self.mode
        ));

    }
}

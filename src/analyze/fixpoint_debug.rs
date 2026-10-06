//! `ROUNDHOUSE_FIXPOINT_DEBUG=1`: per-round report of what moved in the
//! inference signature. A fixpoint that does not settle is either an
//! oscillation (same sizes, different shape) or growth (some type gains
//! nodes every round); this names the entries and their sizes so the
//! growing quantity can be read off the log instead of guessed.

use std::collections::HashMap;

use crate::ident::{ClassId, Symbol};
use crate::ty::Ty;

use super::InferenceSig;

pub(super) fn enabled() -> bool {
    std::env::var_os("ROUNDHOUSE_FIXPOINT_DEBUG").is_some()
}

/// Structural node count of a type.
pub(crate) fn ty_size(ty: &Ty) -> usize {
    1 + match ty {
        Ty::Array { elem } => ty_size(elem),
        Ty::Hash { key, value } => ty_size(key) + ty_size(value),
        Ty::Tuple { elems } => elems.iter().map(ty_size).sum(),
        Ty::Record { row } => row.fields.values().map(ty_size).sum(),
        Ty::Union { variants } => variants.iter().map(ty_size).sum(),
        Ty::Class { args, .. } => args.iter().map(ty_size).sum(),
        Ty::Fn { params, block, ret, .. } => {
            params.iter().map(|p| ty_size(&p.ty)).sum::<usize>()
                + block.as_deref().map(ty_size).unwrap_or(0)
                + ty_size(ret)
        }
        _ => 0,
    }
}

fn short(ty: &Ty) -> String {
    let s = format!("{ty:?}");
    let max = if std::env::var("ROUNDHOUSE_FIXPOINT_DEBUG").as_deref() == Ok("all") { 4000 } else { 400 };
    if s.len() > max {
        format!("{}…", &s[..s.char_indices().nth(max).map(|(i, _)| i).unwrap_or(s.len())])
    } else {
        s
    }
}

struct Change {
    kind: &'static str,
    key: String,
    before: usize,
    after: usize,
    ty: String,
}

fn diff_methods(
    kind: &'static str,
    prev: &HashMap<ClassId, HashMap<Symbol, Ty>>,
    now: &HashMap<ClassId, HashMap<Symbol, Ty>>,
    out: &mut Vec<Change>,
) {
    for (cid, methods) in now {
        let old = prev.get(cid);
        for (name, ty) in methods {
            let before = old.and_then(|m| m.get(name));
            if before != Some(ty) {
                out.push(Change {
                    kind,
                    key: format!("{}#{}", cid.0.as_str(), name.as_str()),
                    before: before.map(ty_size).unwrap_or(0),
                    after: ty_size(ty),
                    ty: short(ty),
                });
            }
        }
    }
}

pub(super) fn report(label: &str, prev: &InferenceSig, now: &InferenceSig) {
    let mut changes = Vec::new();
    diff_methods("instance", &prev.instance, &now.instance, &mut changes);
    diff_methods("class", &prev.class_methods, &now.class_methods, &mut changes);
    for ((cid, name), tys) in &now.params {
        let old = prev.params.get(&(cid.clone(), name.clone()));
        if old != Some(tys) {
            let before: usize = old.map(|v| v.iter().map(ty_size).sum()).unwrap_or(0);
            let after: usize = tys.iter().map(ty_size).sum();
            out_param(&mut changes, cid, name, before, after, tys);
        }
    }
    let total: usize = now
        .instance
        .values()
        .chain(now.class_methods.values())
        .flat_map(|m| m.values())
        .map(ty_size)
        .sum::<usize>()
        + now.params.values().flatten().map(ty_size).sum::<usize>();
    let mut per_kind: HashMap<&str, (usize, isize)> = HashMap::new();
    for c in &changes {
        let e = per_kind.entry(c.kind).or_default();
        e.0 += 1;
        e.1 += c.after as isize - c.before as isize;
    }
    let mut kinds: Vec<_> = per_kind.into_iter().collect();
    kinds.sort();
    eprintln!(
        "roundhouse-fixpoint: {label}: {} changed entr(ies), total sig nodes {total}; per kind (count, node delta): {kinds:?}",
        changes.len()
    );
    if std::env::var("ROUNDHOUSE_FIXPOINT_DEBUG").as_deref() == Ok("all") {
        for c in &changes {
            eprintln!(
                "roundhouse-fixpoint-entry\t{label}\t{}\t{}\t{}\t{}\t{}",
                c.kind, c.key, c.before, c.after, c.ty
            );
        }
    }
    changes.sort_by_key(|c| std::cmp::Reverse(c.after));
    for c in changes.iter().take(15) {
        eprintln!(
            "roundhouse-fixpoint:   {} {} {} -> {} nodes: {}",
            c.kind, c.key, c.before, c.after, c.ty
        );
    }
    let mut grown: Vec<_> = changes.iter().filter(|c| c.after > c.before && c.before > 0).collect();
    grown.sort_by_key(|c| std::cmp::Reverse(c.after as isize - c.before as isize));
    for c in grown.iter().take(10) {
        eprintln!(
            "roundhouse-fixpoint:   grew {} {} {} -> {}",
            c.kind, c.key, c.before, c.after
        );
    }
    let same_size: Vec<_> = changes.iter().filter(|c| c.after == c.before).collect();
    for c in same_size.iter().take(10) {
        eprintln!(
            "roundhouse-fixpoint:   reshaped {} {} ({} nodes): {}",
            c.kind, c.key, c.after, c.ty
        );
    }
}

fn out_param(
    changes: &mut Vec<Change>,
    cid: &ClassId,
    name: &Symbol,
    before: usize,
    after: usize,
    tys: &[Ty],
) {
    changes.push(Change {
        kind: "param",
        key: format!("{}#{}", cid.0.as_str(), name.as_str()),
        before,
        after,
        ty: short(&Ty::Tuple { elems: tys.to_vec() }),
    });
}

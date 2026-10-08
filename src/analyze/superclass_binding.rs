//! A library class's superclass, bound the way Ruby binds it.
//!
//! Ingest records the superclass as written, without a leading `::`, and
//! dispatch walks `ClassInfo::parent` as a registry key. Rubydex rewrites
//! the written name at ingest (`resolve_library_parents`), but only when
//! the target is a source class or model. A superclass only a gem's RBI
//! defines, or a constant (`DeliveryChangeParent = Struct.new(...)` in
//! the enclosing class), gets no rewrite. Core's `Impertinent::LiquidColor
//! < Script::Color`, inside `module Impertinent`, then parented a
//! `Script::Color` that no class is, so it inherited none of
//! `Impertinent::Script::Color`'s methods.
//!
//! Ruby reads the written path at the `class` keyword. A `::` path names
//! the top level. Otherwise the path's first segment is looked up through
//! the lexical nesting, innermost first, then through the ancestors of
//! the innermost nesting body, then on Object; the rest of the path is
//! read inside the constant that lookup finds. A compact header (`class
//! A::B < X`) does not enter `A`, so the nesting is read from the source
//! (`ingest::util::superclass_nestings`), not from the class's name.
//! The class itself is not a candidate: its `class` keyword is still
//! being evaluated, so `class FeatureSetExtension < FeatureSetExtension`
//! inside `module Admin` names the top-level class.

use std::collections::BTreeMap;
use std::rc::Rc;

use super::body::ClassInfo;
use crate::App;
use super::body::ConstResolver;
use crate::det::HashMap;
use crate::ident::{ClassId, Symbol};

/// Rebind each library class's registry parent to the class Ruby's
/// lookup names, where that class is in the registry and the written
/// name is not it.
pub(super) fn bind(app: &App, resolver: &ConstResolver, classes: &mut HashMap<ClassId, ClassInfo>) {
    let mut nestings: BTreeMap<u32, Rc<Vec<(usize, Vec<String>)>>> = BTreeMap::new();
    let mut rebound: Vec<(ClassId, ClassId)> = Vec::new();
    let mut seen: std::collections::BTreeSet<&ClassId> = std::collections::BTreeSet::new();
    for lc in &app.library_classes {
        let (Some(parent), false) = (&lc.parent, lc.parent_span.is_synthetic()) else { continue };
        if lc.origin.is_some() || !seen.insert(&lc.name) {
            continue;
        }
        if classes.get(&lc.name).and_then(|info| info.parent.as_ref()) != Some(parent) {
            continue;
        }
        let span = lc.parent_span;
        let Some(source) = (span.file.0 as usize).checked_sub(1).and_then(|i| app.sources.get(i)) else { continue };
        let Some(written) = source.text.get(span.start as usize..span.end as usize).map(str::trim) else { continue };
        // A `::` path is the top level's, which the registry key already
        // names; a different key is rubydex's rewrite; anything but a
        // constant path is an expression.
        let path = written;
        if path != parent.0.as_str() || !path.split("::").all(is_constant_segment) {
            continue;
        }
        let head = path.split("::").next().unwrap_or(path);
        let defined = |name: &str| {
            name != lc.name.0.as_str()
                && (classes.contains_key(&ClassId(Symbol::from(name))) || resolver.has_source_namespace(name))
        };
        // Cheap screen before the source is parsed: a nesting body is an
        // enclosing namespace of the class's name, so a head none of them
        // (nor the innermost one's ancestors) defines binds at the top.
        let segments: Vec<&str> = lc.name.0.as_str().split("::").collect();
        let enclosing: Vec<String> = (1..segments.len()).rev().map(|i| segments[..i].join("::")).collect();
        let screened = enclosing.iter().any(|scope| defined(&format!("{scope}::{head}")))
            || enclosing.first().is_some_and(|scope| {
                ancestors(scope, classes).iter().any(|a| defined(&format!("{a}::{head}")))
            });
        if !screened {
            continue;
        }
        let parsed = nestings
            .entry(span.file.0)
            .or_insert_with(|| Rc::new(crate::ingest::util::superclass_nestings(source.text.as_bytes(), &source.path)))
            .clone();
        let Some((_, nesting)) = parsed.iter().find(|(start, _)| *start == span.start as usize) else { continue };
        let scope = nesting
            .iter()
            .find(|scope| defined(&format!("{scope}::{head}")))
            .cloned()
            .or_else(|| {
                let innermost = nesting.first()?;
                ancestors(innermost, classes).into_iter().find(|a| defined(&format!("{a}::{head}")))
            });
        let Some(scope) = scope else { continue };
        let bound = ClassId(Symbol::from(format!("{scope}::{path}").as_str()));
        if &bound != parent && classes.contains_key(&bound) {
            rebound.push((lc.name.clone(), bound));
        }
    }
    for (name, bound) in rebound {
        if let Some(info) = classes.get_mut(&name) {
            info.parent = Some(bound);
        }
    }
}

fn is_constant_segment(segment: &str) -> bool {
    segment.chars().next().is_some_and(|c| c.is_ascii_uppercase())
        && segment.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The registry's ancestors of `name`, excluding itself: its includes,
/// the last one first, then its superclass chain, as recorded.
fn ancestors(name: &str, classes: &HashMap<ClassId, ClassInfo>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut stack: Vec<ClassId> = vec![ClassId(Symbol::from(name))];
    let mut first = true;
    while let Some(id) = stack.pop() {
        if out.len() > 64 {
            break;
        }
        if !first {
            if out.iter().any(|seen| seen == id.0.as_str()) {
                continue;
            }
            out.push(id.0.as_str().to_string());
        }
        first = false;
        let Some(info) = classes.get(&id) else { continue };
        // Pushed in reverse so the superclass is visited last and the
        // last include first.
        stack.extend(info.parent.iter().cloned());
        stack.extend(info.includes.iter().cloned());
    }
    out
}

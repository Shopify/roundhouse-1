//! A splat multi-write destructures `Array.try_convert(value) || [value]`
//! (see `ingest_multi_write`). The analyzer types that coercion so the
//! targets keep the value's own types: an `Array[String]` destructures into
//! `String?` targets and an `Array[String]` rest, and a String or a Hash,
//! which answer no `to_ary`, binds whole. Without the coercion typed, the
//! wrap arm (`[value]`) would union an `Array[Array[String]]` into every
//! target. Core's
//! in_progress_capabilities.rb `*parents, leaf = path.split(".")` is the
//! first shape.

use roundhouse::det::HashMap;

use roundhouse::analyze::{BodyTyper, ClassInfo, Ctx};
use roundhouse::expr::{Expr, ExprNode};
use roundhouse::ident::{ClassId, Symbol};
use roundhouse::ty::Ty;

/// Every user local read in `source`, with its type, after typing the
/// statements with `path` bound to a String.
fn local_types(source: &str) -> Vec<(String, Option<Ty>)> {
    let result = ruby_prism::parse(source.as_bytes());
    assert_eq!(result.errors().count(), 0, "legal Ruby control: {source}");
    let program = result.node();
    let mut expr = roundhouse::ingest::ingest_expr(
        &program.as_program_node().unwrap().statements().as_node(),
        "<snippet>",
    )
    .expect("ingest");
    let classes: HashMap<ClassId, ClassInfo> = HashMap::default();
    let mut ctx = Ctx::default();
    ctx.local_bindings.insert(Symbol::from("path"), Ty::Str);
    BodyTyper::new(&classes).analyze_expr(&mut expr, &ctx);
    fn collect(expr: &Expr, out: &mut Vec<(String, Option<Ty>)>) {
        if let ExprNode::Var { name, .. } = &*expr.node {
            if !name.as_str().starts_with("__mw_") {
                out.push((name.as_str().to_string(), expr.ty.clone()));
            }
        }
        expr.node.for_each_child(&mut |child| collect(child, out));
    }
    let mut out = Vec::new();
    collect(&expr, &mut out);
    out
}

fn ty_of(types: &[(String, Option<Ty>)], name: &str) -> Option<Ty> {
    types.iter().find(|(n, _)| n == name).and_then(|(_, ty)| ty.clone())
}

#[test]
fn splat_targets_keep_the_value_types_through_the_coercion() {
    let types = local_types(
        r#"*parents, leaf = path.split(".")
first, *middle, last = path.split(".")
*none, whole = path
*empty, h = { k: 1 }
[parents, leaf, first, middle, last, none, whole, empty, h]
"#,
    );
    // A target reads one element, which is nil past the end of a short
    // array (`*parents, leaf = "".split(".")` binds `leaf = nil`); a rest
    // is always an Array of the elements.
    let array_of_str = Ty::Array { elem: Box::new(Ty::Str) };
    let str_or_nil = Ty::Union { variants: vec![Ty::Str, Ty::Nil] };
    for name in ["leaf", "first", "last", "whole"] {
        assert_eq!(ty_of(&types, name), Some(str_or_nil.clone()), "{name}: {types:?}");
    }
    for name in ["parents", "middle", "none"] {
        assert_eq!(ty_of(&types, name), Some(array_of_str.clone()), "{name}: {types:?}");
    }
    let hash = Ty::Hash { key: Box::new(Ty::Sym), value: Box::new(Ty::Int) };
    assert_eq!(ty_of(&types, "h"), Some(Ty::Union { variants: vec![hash.clone(), Ty::Nil] }), "h: {types:?}");
    assert_eq!(ty_of(&types, "empty"), Some(Ty::Array { elem: Box::new(hash) }), "empty: {types:?}");
}

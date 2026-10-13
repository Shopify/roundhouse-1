//! What a method body sees for its parameters before any caller or
//! signature says more. `*names` is an Array and `**options` a Hash with
//! Symbol keys on every call, so the analyzer types them so; before, the
//! slot stayed a bare type variable and every read was an untyped site.
//! A local a default assigns (`default: (no_default = true)`) is in scope
//! for the whole body, and nil when the caller passed the argument.

use roundhouse::analyze::Analyzer;
use roundhouse::expr::{Expr, ExprNode};
use roundhouse::ident::TyVar;
use roundhouse::ingest::ingest_library_classes;
use roundhouse::ty::Ty;
use roundhouse::App;

const SOURCE: &str = r#"
class Registry
  def self.names(*names)
    names
  end

  def self.count(*names)
    names.length
  end

  def self.options(**options)
    options
  end

  def self.option_keys(**options)
    options.keys
  end

  def self.mixed(first, *rest, **opts)
    [first, rest, opts]
  end

  def self.given?(name, default: (no_default = true))
    no_default
  end
end
"#;

fn analyzed() -> App {
    let mut app = App::new();
    app.library_classes = ingest_library_classes(SOURCE.as_bytes(), "registry.rb").expect("ingest");
    Analyzer::new(&app).analyze(&mut app);
    app
}

fn body_ty(app: &App, method: &str) -> Ty {
    let lc = app.library_classes.iter().find(|c| c.name.0.as_str() == "Registry").expect("Registry");
    let m = lc.methods.iter().find(|m| m.name.as_str() == method).unwrap_or_else(|| panic!("{method}"));
    last_expr(&m.body).ty.clone().unwrap_or_else(|| panic!("{method} body untyped"))
}

fn last_expr(e: &Expr) -> &Expr {
    match &*e.node {
        ExprNode::Seq { exprs } if !exprs.is_empty() => last_expr(exprs.last().unwrap()),
        _ => e,
    }
}

fn unknown() -> std::sync::Arc<Ty> {
    std::sync::Arc::new(Ty::Var { var: TyVar(0) })
}

#[test]
fn a_rest_param_is_an_array() {
    let app = analyzed();
    assert_eq!(body_ty(&app, "names"), Ty::Array { elem: unknown() });
    assert_eq!(body_ty(&app, "count"), Ty::Int);
}

#[test]
fn a_keyword_rest_param_is_a_symbol_keyed_hash() {
    let app = analyzed();
    assert!(
        matches!(body_ty(&app, "options"), Ty::Hash { key, .. } if *key == Ty::Sym),
        "{:?}",
        body_ty(&app, "options")
    );
    assert_eq!(body_ty(&app, "option_keys"), Ty::Array { elem: std::sync::Arc::new(Ty::Sym) });
}

#[test]
fn rest_params_beside_a_positional() {
    let app = analyzed();
    let lc = app.library_classes.iter().find(|c| c.name.0.as_str() == "Registry").unwrap();
    let m = lc.methods.iter().find(|m| m.name.as_str() == "mixed").unwrap();
    let ExprNode::Array { elements, .. } = &*last_expr(&m.body).node else {
        panic!("{:?}", m.body.node);
    };
    // `first` has no caller: it stays unknown. The rest slots do not.
    assert!(matches!(elements[0].ty, Some(Ty::Var { .. })), "{:?}", elements[0].ty);
    assert_eq!(elements[1].ty, Some(Ty::Array { elem: unknown() }));
    assert!(matches!(&elements[2].ty, Some(Ty::Hash { key, .. }) if **key == Ty::Sym), "{:?}", elements[2].ty);
}

#[test]
fn a_local_a_default_assigns_binds_nilable() {
    let app = analyzed();
    let ty = body_ty(&app, "given?");
    assert!(
        matches!(&ty, Ty::Union { variants } if variants.contains(&Ty::Bool) && variants.contains(&Ty::Nil)),
        "{ty:?}"
    );
}

use crate::analyze::indifferent::{self, Shape};
use crate::app::App;
use crate::expr::{Expr, ExprNode, Literal};
use crate::ident::Symbol;
use crate::span::Span;
use crate::ty::Ty;

pub fn apply_indifferent_access_lowering(app: &mut App) {
    coerce_declared_params(app);
    super::for_each_hook_body(app, &mut lower);
    for view in &mut app.views {
        lower(&mut view.body);
    }
    super::for_each_test_body(app, &mut lower);
}

/// A library method's parameter declared `HashWithIndifferentAccess` that
/// its body reads by a Symbol key (`input[:input][:publications]`): the
/// rewrite below reads the string key an indifferent hash answers too, but
/// a caller may hand the method a plain Symbol-keyed Hash, which the
/// declaration does not stop (sorbet-runtime's check is off in the emitted
/// tree) and which Ruby reads by Symbol: Core's `CollectionInput.
/// check_access` is called with the Cardinal executor's `arguments`, a
/// Hash of Symbol keys. The CRuby tree carries Rails' own class, so the
/// parameter is converted on entry where it is not one already
/// (`ActiveSupport.indifferent_param`), and reads either key. A
/// parameter the body assigns keeps what it is given.
fn coerce_declared_params(app: &mut App) {
    // The PostgreSQL app is the CRuby tree, the only one that carries Rails' class.
    if app.primary_database_adapter() != Some(&crate::database::DatabaseAdapter::Postgresql) {
        return;
    }
    for lc in &mut app.library_classes {
        for method in &mut lc.methods {
            let names: Vec<Symbol> = method
                .params
                .iter()
                .filter(|p| !p.rest && !p.forwarding && !p.name.as_str().is_empty())
                .map(|p| p.name.clone())
                .filter(|name| reads_indifferent_var(&method.body, name) && !assigns_var(&method.body, name))
                .collect();
            if names.is_empty() {
                continue;
            }
            let span = method.body.span;
            let mut stmts: Vec<Expr> = names
                .iter()
                .map(|name| {
                    let var = Expr::new(span, ExprNode::Var { id: crate::ident::VarId(0), name: name.clone() });
                    let mut call = support_call(span, "indifferent_param", vec![var]);
                    call.ty = Some(Ty::Untyped);
                    let mut assign = Expr::new(
                        span,
                        ExprNode::Assign {
                            target: crate::expr::LValue::Var { id: crate::ident::VarId(0), name: name.clone() },
                            value: call,
                        },
                    );
                    assign.ty = Some(Ty::Untyped);
                    assign
                })
                .collect();
            let mut body = std::mem::replace(&mut method.body, Expr::new(span, ExprNode::Seq { exprs: vec![] }));
            if let ExprNode::Seq { exprs } = &mut *body.node {
                stmts.append(exprs);
            } else {
                stmts.push(body);
                body = Expr::new(span, ExprNode::Seq { exprs: vec![] });
            }
            *body.node = ExprNode::Seq { exprs: stmts };
            method.body = body;
        }
    }
}

fn reads_indifferent_var(expr: &Expr, name: &Symbol) -> bool {
    let mut found = false;
    visit(expr, &mut |e| {
        if let ExprNode::Send { recv: Some(recv), .. } = &*e.node
            && matches!(&*recv.node, ExprNode::Var { name: n, .. } if n == name)
            && recv.ty.as_ref().is_some_and(is_indifferent_receiver)
        {
            found = true;
        }
    });
    found
}

fn assigns_var(expr: &Expr, name: &Symbol) -> bool {
    let mut found = false;
    visit(expr, &mut |e| {
        let target = match &*e.node {
            ExprNode::Assign { target, .. } | ExprNode::OpAssign { target, .. } => Some(target),
            _ => None,
        };
        if matches!(target, Some(crate::expr::LValue::Var { name: n, .. }) if n == name) {
            found = true;
        }
    });
    found
}

fn visit<'a>(expr: &'a Expr, f: &mut dyn FnMut(&'a Expr)) {
    f(expr);
    expr.node.for_each_child(&mut |child| visit(child, f));
}

fn lower(body: &mut Expr) {
    rewrite(body);
    // Not retyped during the rewrite: a parent call finds its indifferent receiver by the type its child still carries.
    retype(body);
}

fn rewrite(expr: &mut Expr) {
    expr.node.for_each_child_mut(&mut rewrite);
    splat_base_to_hash(expr);
    if !expr.ty.as_ref().is_some_and(|t| !matches!(t, Ty::Var { .. })) {
        return;
    }
    // An untyped receiver (a json column's reader, `extras || {}`) is the
    // usual one in a Rails app, and no runtime defines the call itself:
    // lower it as a Hash receiver's is. The result is typed indifferent,
    // so the calls chained on it (`.merge!(key => value)`) take the
    // modeled path instead of being half-converted.
    if let ExprNode::Send { recv: Some(recv), method, args, .. } = &*expr.node
        && let Some(ty) = untyped_receiver_conversion(method.as_str(), args, recv)
    {
        let mut call = support_call(expr.span, "indifferent", vec![recv.clone()]);
        call.ty = Some(expr.ty.take().filter(indifferent::mentions).unwrap_or(ty));
        *expr = call;
        return;
    }
    if let Some(mut replacement) = rewrite_node(expr.span, expr.decisions, &mut expr.node) {
        replacement.ty = expr.ty.take();
        *expr = replacement;
    }
}

/// `{ **schema, k: v }` where `schema` is an object, not a Hash: Ruby asks
/// the value for `to_hash` (a Component::Schema answers it), and the ingest
/// desugar's `schema.merge({ k: v })` asked for a `merge` it has none of.
/// The receiver is converted the way the splat converts it. A Hash, an
/// indifferent hash (whose merge keeps its keys indifferent) and a value
/// the analysis cannot name keep the plain `merge`.
fn splat_base_to_hash(expr: &mut Expr) {
    if expr.decisions & crate::expr::SPLAT_MERGE_BASE == 0 {
        return;
    }
    let ExprNode::Send { recv: Some(recv), method, .. } = &mut *expr.node else { return };
    if method.as_str() != "merge" {
        return;
    }
    if !recv.ty.as_ref().is_some_and(splatted_object) {
        return;
    }
    let span = recv.span;
    let inner = recv.clone();
    let mut to_hash = Expr::new(
        span,
        ExprNode::Send { recv: Some(inner), method: Symbol::from("to_hash"), args: vec![], block: None, parenthesized: false },
    );
    to_hash.ty = Some(Ty::Hash { key: std::sync::Arc::new(Ty::Untyped), value: std::sync::Arc::new(Ty::Untyped) });
    *recv = to_hash;
}

/// An object type a `**` reads through `to_hash`: a class that is not a
/// Hash, or a union of them (a block parameter the body reassigns from a
/// `new` of its subclass is typed `Base | Sub`; neither side has `merge`).
fn splatted_object(ty: &Ty) -> bool {
    match ty {
        Ty::Class { id, .. } => !matches!(
            id.0.as_str(),
            "Hash" | "HashWithIndifferentAccess" | "ActiveSupport::HashWithIndifferentAccess" | "ActionController::Parameters"
        ),
        Ty::Union { variants } => !variants.is_empty() && variants.iter().all(splatted_object),
        _ => false,
    }
}

// Not a receiver of any other class (an app's own `with_indifferent_access`,
// one already indifferent, which the typed path copies): the gradual ones
// analysis types the call for.
fn untyped_receiver_conversion(method: &str, args: &[Expr], recv: &Expr) -> Option<Ty> {
    if method != "with_indifferent_access" || !args.is_empty() {
        return None;
    }
    indifferent::from_gradual(recv.ty.as_ref()?)
}

fn rewrite_node(span: Span, decisions: u64, node: &mut ExprNode) -> Option<Expr> {
    let ExprNode::Send { recv: Some(recv), method, args, block, .. } = node else { return None };
    if decisions & crate::expr::REVERSE_MERGE != 0
        && let [hash] = args.as_slice()
        && hash.ty.as_ref().is_some_and(|t| indifferent::value_of(t).is_some())
        && !recv.ty.as_ref().is_some_and(|t| indifferent::value_of(t).is_some())
    {
        let defaults = recv.clone();
        *recv = from_hash(span, defaults);
        return None;
    }
    if method.as_str() == "new" && is_class_const(recv) && block.is_none() && indifferent::constructor_args(args) {
        return match args.as_slice() {
            [] => Some(Expr::new(span, ExprNode::Hash { entries: vec![], kwargs: false })),
            [hash] => Some(from_hash(span, hash.clone())),
            _ => None,
        };
    }
    if method.as_str() == "with_indifferent_access" && args.is_empty() && matches!(recv.ty, Some(Ty::Hash { .. })) {
        return Some(support_call(span, "indifferent", vec![recv.clone()]));
    }
    let typed = recv.ty.as_ref().is_some_and(is_indifferent_receiver);
    let maybe = read_from_indifferent(recv) || recv.ty.as_ref().is_some_and(may_be_indifferent);
    if !typed && !(maybe && converts_untyped(method.as_str())) {
        return None;
    }
    match indifferent::shape(method.as_str(), args, block.is_some())? {
        Shape::FirstKey => convert_key(&mut args[0]),
        Shape::AllKeys => {
            args.iter_mut().for_each(convert_key);
            if method.as_str() == "without" {
                *method = Symbol::from("except");
            }
        }
        Shape::Write => {
            convert_key(&mut args[0]);
            convert_assigned(&mut args[1]);
        }
        Shape::Merge => {
            let other = args[0].clone();
            args[0] = from_hash(span, other);
        }
        Shape::ReverseMerge => {
            return Some(Expr::new(
                span,
                ExprNode::Send {
                    recv: Some(from_hash(span, args[0].clone())),
                    method: Symbol::from("merge"),
                    args: vec![recv.clone()],
                    block: None,
                    parenthesized: true,
                },
            ));
        }
        Shape::Copy => *method = Symbol::from("dup"),
        Shape::ToHash => return Some(support_call(span, "indifferent_to_hash", vec![recv.clone()])),
        Shape::SymbolizeKeys => {
            let hash = support_call(span, "indifferent_to_hash", vec![recv.clone()]);
            return Some(support_call(span, "symbolize_keys", vec![hash]));
        }
        Shape::Plain => {}
    }
    None
}

// Not every untyped value: only one read out of an indifferent hash, where every nested Hash is indifferent too.
fn read_from_indifferent(expr: &Expr) -> bool {
    let untyped = match &expr.ty {
        Some(Ty::Untyped) => true,
        Some(ty @ Ty::Union { .. }) => leaves(ty).contains(&&Ty::Untyped),
        _ => false,
    };
    if !untyped {
        return false;
    }
    let ExprNode::Send { recv: Some(recv), method, .. } = &*expr.node else { return false };
    matches!(method.as_str(), "[]" | "fetch" | "dig" | "first" | "last")
        && (recv.ty.as_ref().is_some_and(|t| is_indifferent_receiver(t) || may_be_indifferent(t)) || read_from_indifferent(recv))
}

// Not `include?`, `member?` or `delete`: an Array answers them for a Symbol too, so converting the key would change the answer.
fn converts_untyped(method: &str) -> bool {
    matches!(method, "[]" | "fetch" | "key?" | "has_key?" | "dig" | "values_at" | "slice" | "except" | "[]=" | "store" | "merge" | "update" | "merge!")
}

fn leaves(ty: &Ty) -> Vec<&Ty> {
    match ty {
        Ty::Union { variants } => variants.iter().flat_map(leaves).collect(),
        other => vec![other],
    }
}

fn may_be_indifferent(ty: &Ty) -> bool {
    leaves(ty).iter().any(|v| indifferent::value_of(v).is_some())
}

fn is_indifferent_receiver(ty: &Ty) -> bool {
    let leaves = leaves(ty);
    leaves.iter().any(|v| indifferent::value_of(v).is_some())
        && leaves.iter().all(|v| indifferent::value_of(v).is_some() || **v == Ty::Nil)
}

fn is_class_const(expr: &Expr) -> bool {
    matches!(&*expr.node, ExprNode::Const { path }
        if matches!(path.iter().map(|s| s.as_str()).collect::<Vec<_>>().as_slice(),
            ["ActiveSupport", "HashWithIndifferentAccess"]
            | ["", "ActiveSupport", "HashWithIndifferentAccess"]
            | ["HashWithIndifferentAccess"]
            | ["", "HashWithIndifferentAccess"]))
}

// Not converted again when already indifferent: Rails keeps that hash as it is, values shared.
fn from_hash(span: Span, hash: Expr) -> Expr {
    if hash.ty.as_ref().is_some_and(|t| indifferent::value_of(t).is_some()) {
        let ty = hash.ty.clone();
        let mut dup = Expr::new(
            span,
            ExprNode::Send { recv: Some(hash), method: Symbol::from("dup"), args: vec![], block: None, parenthesized: false },
        );
        dup.ty = ty;
        return dup;
    }
    support_call(span, "indifferent", vec![hash])
}

fn convert_key(key: &mut Expr) {
    if let ExprNode::Splat { value } = &mut *key.node {
        let span = value.span;
        let keys = value.clone();
        *value = support_call(span, "indifferent_keys", vec![keys]);
        return;
    }
    if let ExprNode::Lit { value: Literal::Sym { value } } = &*key.node {
        let text = value.as_str().to_string();
        *key.node = ExprNode::Lit { value: Literal::Str { value: text } };
        key.ty = Some(Ty::Str);
        return;
    }
    let span = key.span;
    match key.ty.as_ref() {
        Some(t) if !may_be_symbol(t) => {}
        Some(Ty::Sym) => {
            let sym = key.clone();
            *key = Expr::new(
                span,
                ExprNode::Send { recv: Some(sym), method: Symbol::from("to_s"), args: vec![], block: None, parenthesized: false },
            );
            key.ty = Some(Ty::Str);
        }
        _ => {
            let raw = key.clone();
            *key = support_call(span, "indifferent_key", vec![raw]);
        }
    }
}

fn may_be_symbol(ty: &Ty) -> bool {
    match ty {
        Ty::Sym | Ty::Untyped | Ty::Var { .. } => true,
        Ty::Union { variants } => variants.iter().any(may_be_symbol),
        _ => false,
    }
}

fn convert_assigned(value: &mut Expr) {
    let Some(ty) = value.ty.as_ref() else { return };
    if indifferent::value_of(ty).is_some() || !may_hold_hash(ty) {
        return;
    }
    let span = value.span;
    let raw = value.clone();
    let mut in_place = Expr::new(span, ExprNode::Lit { value: Literal::Bool { value: true } });
    in_place.ty = Some(Ty::Bool);
    *value = support_call(span, "indifferent_value", vec![raw, in_place]);
}

fn may_hold_hash(ty: &Ty) -> bool {
    match ty {
        Ty::Hash { .. } | Ty::Array { .. } | Ty::Untyped | Ty::Var { .. } | Ty::Record { .. } | Ty::Tuple { .. } => true,
        Ty::Union { variants } => variants.iter().any(may_hold_hash),
        _ => false,
    }
}

fn support_call(span: Span, method: &str, args: Vec<Expr>) -> Expr {
    Expr::new(
        span,
        ExprNode::Send {
            recv: Some(Expr::new(span, ExprNode::Const { path: vec![Symbol::from("ActiveSupport")] })),
            method: Symbol::from(method),
            args,
            block: None,
            parenthesized: true,
        },
    )
}

fn retype(expr: &mut Expr) {
    expr.node.for_each_child_mut(&mut retype);
    if let Some(ty) = expr.ty.as_ref().filter(|t| indifferent::mentions(t)) {
        expr.ty = Some(indifferent::plain(ty));
    }
}

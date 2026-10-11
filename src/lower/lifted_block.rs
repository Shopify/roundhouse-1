//! A DSL block body that becomes a method body.
//!
//! Rails keeps the block of a class-body DSL call as a block: a model
//! callback (`after_create do … end`), a transaction callback, a scope
//! lambda, a normalizer, a CurrentAttributes `resets`, an initializer's
//! SQL function. The lowering lifts its body into a `def` (or into the
//! tail of one), and there the block's own jumps change meaning:
//!
//! - `next v` ends the block with `v`. A `def` has no block to leave, so
//!   a bare `next` there does not even parse (`Invalid next`); the
//!   method's `return v` is the same exit.
//! - In a lambda (`-> { … }`, `lambda { … }`), `break v` and `return v`
//!   end it as `next v` does, so they also become (or stay) `return v`.
//! - In a block (a proc), Rails calls the body after the class body that
//!   created it has returned, so its own `break` and `return` raise
//!   LocalJumpError, and `redo` restarts it forever. No method body says
//!   that; [`Jumps::unliftable`] names the jump so the caller refuses
//!   the declaration instead of emitting something else.
//!
//! A jump inside a block, lambda or loop nested in the body belongs to
//! that one and is left alone (`[1].each { next }` stays `next`).
//!
//! A method body that now returns early cannot be spliced into a
//! larger method (a hook several declarations fold into) without
//! skipping what follows it; [`Jumps::returns_early`] tells the caller
//! to give it a method of its own.

use crate::expr::{Expr, ExprNode, Literal};

/// How the source wrote the body: the jumps of a proc and of a lambda
/// mean different things.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Form {
    /// `do … end` / `{ … }`, or `proc { … }`.
    Block,
    /// `-> { … }` or `lambda { … }`.
    Lambda,
}

/// The jumps in a body that leave the body itself, rather than a
/// block, lambda or loop nested in it.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(crate) struct Jumps {
    pub next: bool,
    pub break_: bool,
    pub redo: bool,
    /// A `return` outside every lambda nested in the body (a nested
    /// block's `return` still leaves the body's frame).
    pub return_: bool,
}

impl Jumps {
    /// The jump no method body can stand for, when the body has one:
    /// `redo` anywhere, and a block's own `break` or `return` (Rails
    /// raises LocalJumpError for those).
    pub(crate) fn unliftable(self, form: Form) -> Option<&'static str> {
        match form {
            _ if self.redo => Some("redo"),
            Form::Block if self.break_ => Some("break"),
            Form::Block if self.return_ => Some("return"),
            _ => None,
        }
    }

    /// Will the lifted method body (after [`to_method_body`]) return
    /// before its last statement?
    pub(crate) fn returns_early(self) -> bool {
        self.next || self.break_ || self.return_
    }
}

/// The jumps that leave `body` itself.
pub(crate) fn jumps(body: &Expr) -> Jumps {
    let mut out = Jumps::default();
    walk(body, Nesting { in_block: false, in_lambda: false }, &mut out);
    out
}

#[derive(Clone, Copy)]
struct Nesting {
    /// Inside a nested block, lambda or loop: `next`/`break`/`redo`
    /// bind there.
    in_block: bool,
    /// Inside a nested lambda: `return` binds there.
    in_lambda: bool,
}

fn walk(e: &Expr, at: Nesting, out: &mut Jumps) {
    let nested = |lambda: bool| Nesting { in_block: true, in_lambda: at.in_lambda || lambda };
    match &*e.node {
        ExprNode::Next { value } | ExprNode::Break { value } => {
            if !at.in_block {
                if matches!(&*e.node, ExprNode::Next { .. }) {
                    out.next = true;
                } else {
                    out.break_ = true;
                }
            }
            if let Some(v) = value {
                walk(v, at, out);
            }
        }
        ExprNode::Redo => out.redo |= !at.in_block,
        ExprNode::Return { value } => {
            out.return_ |= !at.in_lambda;
            walk(value, at, out);
        }
        ExprNode::While { cond, body, .. } => {
            walk(cond, at, out);
            walk(body, nested(false), out);
        }
        // Not in a call's block slot: a lambda literal.
        ExprNode::Lambda { body, .. } => walk(body, nested(true), out),
        ExprNode::Send { recv, method, args, block, .. } => {
            if let Some(r) = recv {
                walk(r, at, out);
            }
            for a in args {
                walk(a, at, out);
            }
            if let Some(b) = block {
                let lambda = recv.is_none() && matches!(method.as_str(), "lambda" | "define_method");
                walk_block(b, at, nested(lambda), out);
            }
        }
        ExprNode::Super { args, block } => {
            for a in args.iter().flatten() {
                walk(a, at, out);
            }
            if let Some(b) = block {
                walk_block(b, at, nested(false), out);
            }
        }
        ExprNode::Apply { fun, args, block } => {
            walk(fun, at, out);
            for a in args {
                walk(a, at, out);
            }
            if let Some(b) = block {
                walk_block(b, at, nested(false), out);
            }
        }
        // A `def` is a frame of its own.
        ExprNode::MethodDefExpr { .. } => {}
        _ => e.node.for_each_child(&mut |c| walk(c, at, out)),
    }
}

/// A call's block: its body under `inner`; a block argument that is
/// not a literal block (`&blk`, `&method(:m)`) is an ordinary operand.
fn walk_block(b: &Expr, at: Nesting, inner: Nesting, out: &mut Jumps) {
    match &*b.node {
        ExprNode::Lambda { body, .. } => walk(body, inner, out),
        _ => walk(b, at, out),
    }
}

/// Rewrite the jumps that leave `body` itself into the method it
/// becomes: `next v` → `return v` (`next` → `return`), and for a
/// lambda its `break v` too. A lambda's own `return` already means the
/// same in the method. Jumps nested in a block, lambda or loop stay.
/// A block's `break`/`return`/`redo` are left as they are: callers
/// refuse those first ([`Jumps::unliftable`]).
pub(crate) fn to_method_body(body: &mut Expr, form: Form) {
    to_method_body_with(body, form, &|value, span| {
        value.unwrap_or_else(|| Expr::new(span, ExprNode::Lit { value: Literal::Nil }))
    });
}

/// [`to_method_body`], with `returned` building each `return`'s value
/// from the jump's own (`None` for a bare `next`).
pub(crate) fn to_method_body_with(body: &mut Expr, form: Form, returned: &dyn Fn(Option<Expr>, crate::span::Span) -> Expr) {
    let span = body.span;
    match &mut *body.node {
        ExprNode::Next { value } => {
            let value = returned(value.take(), span);
            *body = Expr::new(span, ExprNode::Return { value });
        }
        ExprNode::Break { value } if form == Form::Lambda => {
            let value = returned(value.take(), span);
            *body = Expr::new(span, ExprNode::Return { value });
        }
        ExprNode::While { cond, .. } => to_method_body_with(cond, form, returned),
        ExprNode::Lambda { .. } | ExprNode::MethodDefExpr { .. } => {}
        ExprNode::Send { recv, args, block, .. } => {
            if let Some(r) = recv {
                to_method_body_with(r, form, returned);
            }
            for a in args {
                to_method_body_with(a, form, returned);
            }
            if let Some(b) = block {
                // `&blk` is an operand; a literal block keeps its jumps.
                if !matches!(&*b.node, ExprNode::Lambda { .. }) {
                    to_method_body_with(b, form, returned);
                }
            }
        }
        _ => body.node.for_each_child_mut(&mut |c| to_method_body_with(c, form, returned)),
    }
}

/// A body spliced in place into a larger method whose statements go on
/// after it (a controller's dispatcher runs its filter blocks inline),
/// where `return` would end the whole method. Its own guard clauses at
/// its top level — `next if c`, `next unless c`, a `next` statement —
/// become the branch that skips the rest of the body:
///
/// ```text
/// next if c          if c
/// rest          →      nil
///                    else
///                      rest
///                    end
/// ```
///
/// `next`'s value is still evaluated, then dropped: the callers splice
/// bodies whose value nothing reads. `Err` names an own jump that no
/// such restructuring removes (one nested in a branch, a `break`, a
/// `return`), for the caller to refuse by name.
pub(crate) fn splice(body: &Expr) -> Result<Expr, &'static str> {
    if jumps(body) == Jumps::default() {
        return Ok(body.clone());
    }
    let stmts: Vec<Expr> = match &*body.node {
        ExprNode::Seq { exprs } => exprs.clone(),
        _ => vec![body.clone()],
    };
    let mut out = Expr::new(body.span, ExprNode::Seq { exprs: unguard(&stmts)? });
    out.inherit_span(body.span);
    Ok(out)
}

fn unguard(stmts: &[Expr]) -> Result<Vec<Expr>, &'static str> {
    let mut out = Vec::new();
    for (i, stmt) in stmts.iter().enumerate() {
        if let ExprNode::Next { value } = &*stmt.node {
            // What follows is dead.
            if let Some(v) = value {
                own_jump(v)?;
                out.push(v.clone());
            }
            return Ok(out);
        }
        if let Some((cond, skips_when_true, value)) = guard(stmt) {
            let span = stmt.span;
            let skip = match value {
                Some(v) => {
                    own_jump(&v)?;
                    v
                }
                None => Expr::new(span, ExprNode::Lit { value: Literal::Nil }),
            };
            let rest = Expr::new(span, ExprNode::Seq { exprs: unguard(&stmts[i + 1..])? });
            let (then_branch, else_branch) = if skips_when_true { (skip, rest) } else { (rest, skip) };
            out.push(Expr::new(span, ExprNode::If { cond, then_branch, else_branch }));
            return Ok(out);
        }
        own_jump(stmt)?;
        out.push(stmt.clone());
    }
    Ok(out)
}

/// `Err` naming `e`'s own jump, if it has one.
fn own_jump(e: &Expr) -> Result<(), &'static str> {
    let j = jumps(e);
    match () {
        _ if j.next => Err("next"),
        _ if j.break_ => Err("break"),
        _ if j.redo => Err("redo"),
        _ if j.return_ => Err("return"),
        _ => Ok(()),
    }
}

/// `next if c` / `next unless c` (and their `if … end` spellings with
/// nothing else in the branch): the condition, whether `next` is taken
/// when it holds, and `next`'s value.
fn guard(stmt: &Expr) -> Option<(Expr, bool, Option<Expr>)> {
    let ExprNode::If { cond, then_branch, else_branch } = &*stmt.node else { return None };
    if jumps(cond) != Jumps::default() {
        return None;
    }
    fn as_next(e: &Expr) -> Option<Option<Expr>> {
        match &*e.node {
            ExprNode::Next { value } => Some(value.clone()),
            ExprNode::Seq { exprs } if exprs.len() == 1 => as_next(&exprs[0]),
            _ => None,
        }
    }
    fn is_empty(e: &Expr) -> bool {
        match &*e.node {
            ExprNode::Lit { value: Literal::Nil } => true,
            ExprNode::Seq { exprs } => exprs.is_empty(),
            _ => false,
        }
    }
    match (as_next(then_branch), as_next(else_branch)) {
        (Some(value), None) if is_empty(else_branch) => Some((cond.clone(), true, value)),
        (None, Some(value)) if is_empty(then_branch) => Some((cond.clone(), false, value)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(src: &str) -> Expr {
        let parsed = ruby_prism::parse(src.as_bytes());
        let node = parsed.node();
        let program = node.as_program_node().unwrap();
        crate::ingest::ingest_expr(&program.statements().as_node(), "probe.rb").unwrap()
    }

    fn lifted(src: &str, form: Form) -> String {
        let mut e = body(src);
        to_method_body(&mut e, form);
        crate::emit::ruby::emit_expr(&e)
    }

    #[test]
    fn own_next_becomes_return() {
        let out = lifted("next if x\ny", Form::Block);
        assert!(out.contains("return if x"), "{out}");
        assert!(!out.contains("next"), "{out}");
        let out = lifted("next 1 if x\n2", Form::Block);
        assert!(out.contains("return 1 if x"), "{out}");
    }

    #[test]
    fn nested_next_stays() {
        for src in [
            "[1].each { next }",
            "[1].each do |i|\n  next if i\nend",
            "while x\n  next\nend",
            "f = -> { next 1 }",
            "for i in [1]\n  next if i\nend",
            "loop do\n  next\nend",
        ] {
            let out = lifted(src, Form::Block);
            assert!(out.contains("next"), "{src:?} lost its next: {out}");
            assert!(!out.contains("return"), "{src:?}: {out}");
            assert_eq!(jumps(&body(src)), Jumps::default(), "{src:?}");
        }
    }

    #[test]
    fn jumps_name_what_leaves_the_body() {
        assert!(jumps(&body("next if x")).next);
        assert!(jumps(&body("if a\n  b\nelse\n  next 2\nend")).next);
        assert!(jumps(&body("begin\n  next\nrescue\n  nil\nend")).next);
        assert_eq!(jumps(&body("break if x")).unliftable(Form::Block), Some("break"));
        assert_eq!(jumps(&body("return if x")).unliftable(Form::Block), Some("return"));
        // A nested block's `return` still leaves the body's frame.
        assert_eq!(jumps(&body("[1].each { return }")).unliftable(Form::Block), Some("return"));
        // A nested lambda's is its own.
        assert_eq!(jumps(&body("f = -> { return 1 }\nf.call")), Jumps::default());
        assert_eq!(jumps(&body("f = lambda { return 1 }")), Jumps::default());
        assert_eq!(jumps(&body("redo")).unliftable(Form::Lambda), Some("redo"));
        assert_eq!(jumps(&body("break 1 if x")).unliftable(Form::Lambda), None);
        assert!(jumps(&body("return 1 if x")).returns_early());
        assert!(!jumps(&body("x\ny")).returns_early());
    }

    #[test]
    fn splice_turns_guard_clauses_into_branches() {
        let spliced = |src: &str| splice(&body(src)).map(|e| crate::emit::ruby::emit_expr(&e));
        let out = spliced("a\nnext if x\nb\nnext unless y\nc").unwrap();
        assert!(!out.contains("next") && !out.contains("return"), "{out}");
        // `a` always runs, `b` only when `x` does not hold, `c` only when `y` also does.
        let (a, b, c) = (out.find('a').unwrap(), out.find('b').unwrap(), out.find('c').unwrap());
        assert!(a < b && b < c, "{out}");
        // A nested block's own `next` is not a guard and stays.
        let out = spliced("[1].each { next }\nnext if x\nb").unwrap();
        assert!(out.contains("next") && out.contains("each"), "{out}");
        // No restructuring removes these.
        assert_eq!(spliced("if a\n  b\n  next\nend\nc"), Err("next"));
        assert_eq!(spliced("break if x"), Err("break"));
        // (A bare leading `return if x` is already a branch after ingest,
        // `detect_leading_guard`; a valued one stays a `return`.)
        assert_eq!(spliced("return 1 if x\nb"), Err("return"));
        // Nothing of its own: unchanged.
        assert_eq!(spliced("a\nb").unwrap(), crate::emit::ruby::emit_expr(&body("a\nb")));
    }

    #[test]
    fn a_lambdas_break_is_its_return() {
        let out = lifted("break 1 if x\n2", Form::Lambda);
        assert!(out.contains("return 1 if x"), "{out}");
        // A block's break is refused by the caller, not rewritten.
        let out = lifted("break 1 if x\n2", Form::Block);
        assert!(out.contains("break 1 if x"), "{out}");
    }
}

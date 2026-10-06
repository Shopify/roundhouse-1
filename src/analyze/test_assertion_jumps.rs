//! The inliner splices an `assert_difference`, `assert_raises` or
//! `assert_throws` block's statements into the test
//! (`inline_assertions`). A `next`, `break` or `redo` aimed at that
//! block natively leaves only the block; spliced, it is aimed at
//! whatever encloses the assertion (the iterator around it, the
//! `catch` the lowering adds, or nothing, a syntax error), so the test
//! takes another path, often skipping the check. Each such jump is
//! refused by name.
//!
//! The jump's target is found structurally. A loop, a block passed to a
//! call, or a lambda literal inside the spliced block is the target of
//! the jumps inside it, and those are kept, except where that block is
//! itself a spliced assertion's: it is checked as its own.
//!
//! `return` leaves the test natively. Spliced bare into the test (a
//! statement `assert_difference`) it still does. But `assert_raises`
//! and `assert_throws` lower to a `begin`/`catch` around the block, and
//! an assigned `assert_difference` to a `begin … end` value, which some
//! targets emit as a function of its own (TypeScript), where `return`
//! leaves only that; an assigned one can also be a void value. So a
//! `return` there is refused unless a lambda literal is its target,
//! also on CRuby, where it would run.
//!
//! Only the assertions the inliner reaches are checked: the same
//! children it descends into (`for_each_rewritten_child`).

use std::collections::HashSet;

use crate::diagnostic::Diagnostic;
use crate::expr::{Expr, ExprNode};
use crate::lower::test_module_to_library::inline_assertions::{for_each_rewritten_child, spliced_block};
use crate::span::Span;
use crate::App;

struct Walk<'a> {
    own: &'a dyn Fn(&str) -> bool,
    seen: HashSet<Span>,
    out: Vec<Diagnostic>,
}

/// What a jump inside a spliced block may aim at.
#[derive(Clone, Copy)]
struct Scope {
    /// A loop, block or lambda inside the spliced block is the target of
    /// `next`/`break`/`redo`.
    jumps_confined: bool,
    /// The lowering wraps the block, so a `return` must be refused...
    wrapped: bool,
    /// ...unless a lambda literal inside it is the `return`'s target.
    return_confined: bool,
}

impl Walk<'_> {
    fn refuse(&mut self, span: Span, jump: &str, assertion: &str, why: &str) {
        if self.seen.insert(span) {
            self.out.push(Diagnostic::unsupported(
                span,
                None,
                "assertion_block_jump",
                format!("`{jump}` in the block of {assertion} {why}"),
            ));
        }
    }

    /// Find the spliced assertion calls the inliner reaches in `e`;
    /// `value` is set when `e` is the right of an assignment.
    fn scan(&mut self, e: &Expr, value: bool) {
        if let Some(body) = spliced_block(e, self.own) {
            let ExprNode::Send { method, .. } = &*e.node else { unreachable!() };
            let assertion = method.as_str();
            let wrapped = !assertion.ends_with("difference") || value;
            self.inside(body, assertion, Scope { jumps_confined: false, wrapped, return_confined: false });
        }
        let rhs = match &*e.node {
            ExprNode::Assign { value, .. } | ExprNode::MultiAssign { value, .. } => Some(value),
            _ => None,
        };
        for_each_rewritten_child(e, &mut |child| self.scan(child, rhs.is_some_and(|rhs| std::ptr::eq(child, rhs))));
    }

    fn inside(&mut self, e: &Expr, assertion: &str, scope: Scope) {
        match &*e.node {
            ExprNode::Next { .. } | ExprNode::Break { .. } | ExprNode::Redo if !scope.jumps_confined => {
                let jump = match &*e.node {
                    ExprNode::Next { .. } => "next",
                    ExprNode::Break { .. } => "break",
                    _ => "redo",
                };
                self.refuse(e.span, jump, assertion, "leaves the block natively, but the block is inlined into the test, where it would leave what encloses the assertion");
            }
            ExprNode::Return { .. } if scope.wrapped && !scope.return_confined => {
                self.refuse(e.span, "return", assertion, "returns from the test natively, but the lowering wraps the block in code some targets emit as a function of its own, which is all it would return from");
            }
            ExprNode::While { .. } => {
                let inner = Scope { jumps_confined: true, ..scope };
                e.node.for_each_child(&mut |child| self.inside(child, assertion, inner));
                return;
            }
            // A lambda literal is the target of every jump inside it.
            ExprNode::Lambda { .. } => {
                let inner = Scope { jumps_confined: true, return_confined: true, ..scope };
                e.node.for_each_child(&mut |child| self.inside(child, assertion, inner));
                return;
            }
            _ => {}
        }
        // A block is the target of `next`/`break`/`redo`, not of
        // `return`. A nested spliced assertion's block counts as a block
        // here; `scan` checks it as that assertion's.
        let (args, block): (Vec<&Expr>, Option<&Expr>) = match &*e.node {
            ExprNode::Send { recv, args, block, .. } => (recv.iter().chain(args).collect(), block.as_ref()),
            ExprNode::Apply { fun, args, block } => (std::iter::once(fun).chain(args).collect(), block.as_ref()),
            _ => {
                e.node.for_each_child(&mut |child| self.inside(child, assertion, scope));
                return;
            }
        };
        for child in args {
            self.inside(child, assertion, scope);
        }
        match block.map(|b| (b, &*b.node)) {
            Some((_, ExprNode::Lambda { body, .. })) => self.inside(body, assertion, Scope { jumps_confined: true, ..scope }),
            Some((other, _)) => self.inside(other, assertion, scope),
            None => {}
        }
    }
}

pub(super) fn diagnose(app: &App) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for module in &app.test_modules {
        let own = |name: &str| module.defines_instance_method(&app.test_modules, name);
        let mut walk = Walk { own: &own, seen: HashSet::new(), out: Vec::new() };
        if let Some(setup) = &module.setup {
            walk.scan(setup, false);
        }
        for method in &module.helpers {
            walk.scan(&method.body, false);
        }
        for test in &module.tests {
            walk.scan(&test.body, false);
        }
        out.extend(walk.out);
    }
    out
}

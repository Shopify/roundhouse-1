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
//! itself a spliced assertion's: it is checked as its own. `return`
//! leaves the test natively and spliced alike, except in the tail of an
//! `assert_difference` block whose value is assigned, where the
//! lowering's `__diff_value = begin … end` would hold a void value;
//! there it is refused, also when only one branch returns.

use std::collections::HashSet;

use crate::diagnostic::Diagnostic;
use crate::expr::{Expr, ExprNode};
use crate::lower::test_module_to_library::inline_assertions::spliced_block;
use crate::span::Span;
use crate::App;

struct Walk<'a> {
    own: &'a dyn Fn(&str) -> bool,
    seen: HashSet<Span>,
    out: Vec<Diagnostic>,
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

    /// Find spliced assertion calls anywhere in `e`; `value` is set when
    /// `e` is the right of an assignment.
    fn scan(&mut self, e: &Expr, value: bool) {
        if let Some(body) = spliced_block(e, self.own) {
            let ExprNode::Send { method, .. } = &*e.node else { unreachable!() };
            let assertion = method.as_str().to_string();
            let difference = assertion.ends_with("difference");
            self.inside(body, &assertion, false);
            if value && difference {
                self.tail_returns(body, &assertion);
            }
        }
        match &*e.node {
            ExprNode::Assign { value: rhs, .. } | ExprNode::MultiAssign { value: rhs, .. } => {
                e.node.for_each_child(&mut |child| {
                    let is_rhs = std::ptr::eq(child, rhs);
                    self.scan(child, is_rhs);
                });
            }
            _ => e.node.for_each_child(&mut |child| self.scan(child, false)),
        }
    }

    /// Jumps inside a spliced block; `confined` once a nested loop,
    /// block or lambda is their target. A nested spliced assertion's
    /// block counts as a block here, and `scan` refuses the jumps in it
    /// as that assertion's.
    fn inside(&mut self, e: &Expr, assertion: &str, confined: bool) {
        match &*e.node {
            ExprNode::Next { .. } | ExprNode::Break { .. } | ExprNode::Redo if !confined => {
                let jump = match &*e.node {
                    ExprNode::Next { .. } => "next",
                    ExprNode::Break { .. } => "break",
                    _ => "redo",
                };
                self.refuse(e.span, jump, assertion, "leaves the block natively, but the block is inlined into the test, where it would leave what encloses the assertion");
            }
            ExprNode::While { .. } | ExprNode::Lambda { .. } => {
                e.node.for_each_child(&mut |child| self.inside(child, assertion, true));
                return;
            }
            _ => {}
        }
        e.node.for_each_child(&mut |child| self.inside(child, assertion, confined));
    }

    /// `return` in a tail position of `e`.
    fn tail_returns(&mut self, e: &Expr, assertion: &str) {
        match &*e.node {
            ExprNode::Return { .. } => self.refuse(e.span, "return", assertion, "is the value assigned from it, which the inlined block cannot hold"),
            ExprNode::Seq { exprs } => {
                if let Some(last) = exprs.last() {
                    self.tail_returns(last, assertion);
                }
            }
            ExprNode::If { then_branch, else_branch, .. } => {
                self.tail_returns(then_branch, assertion);
                self.tail_returns(else_branch, assertion);
            }
            ExprNode::Case { arms, .. } => {
                for arm in arms {
                    self.tail_returns(&arm.body, assertion);
                }
            }
            ExprNode::BeginRescue { body, rescues, else_branch, .. } => {
                self.tail_returns(else_branch.as_ref().unwrap_or(body), assertion);
                for rescue in rescues {
                    self.tail_returns(&rescue.body, assertion);
                }
            }
            _ => {}
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

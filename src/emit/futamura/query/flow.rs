//! Locals a method assigns on several paths.
//!
//! `scope = Model.where(a: x); scope = scope.where(b: y) if y` leaves the
//! chain at `scope.to_a` with two shapes, which one depending on a run-time
//! condition. The query specializer decides each shape on its own: it asks
//! [`Flow::reaching`] for the assignment that reaches a local's read, and
//! when two do (an `if` joined them) the read is a [`Defs::Branch`] on the
//! `if`. The specializer then explores both outcomes of that `if` (see
//! `Specializer::specialize_paths`), gives each leaf its own statement, and
//! the call site picks the leaf with the same condition Ruby tested, which
//! it may only re-evaluate when [`pure_condition`] says so.
//!
//! Only straight-line code and `if`/`unless`/`elsif`/ternary are followed:
//! an assignment inside a loop, a block that closes over the local, a
//! `case`, a `begin`/`rescue`, `||=` or a multiple assignment makes the
//! local opaque and the site stays Rails'.

use std::cell::{Cell, RefCell};

use crate::expr::{Expr, ExprNode, LValue, Literal};
use crate::ident::{Symbol, VarId};

use super::{Association, Chain, Cx, ModelBodyItem, Site, Specializer};

/// The most `if`s one site may branch on (so 2^n statements).
pub(super) const MAX_DECISIONS: usize = 3;

/// The marker `reaching` fails with while it waits for a decision; the
/// driver reads [`Flow::needed`], never this text.
pub(super) const NEEDS_DECISION: &str = "path-dependent";

/// Which assignments of one local can reach a read of it.
#[derive(Clone)]
enum Defs<'e> {
    /// Nothing assigned it first: a parameter, or a read before any write.
    Unset,
    /// The value expression of the assignment.
    Def(&'e Expr),
    /// The `if` whose two branches leave different assignments.
    Branch(&'e Expr, Box<Defs<'e>>, Box<Defs<'e>>),
    /// Assigned where the flow is not followed.
    Opaque(&'static str),
}

fn same(a: &Defs, b: &Defs) -> bool {
    match (a, b) {
        (Defs::Unset, Defs::Unset) => true,
        (Defs::Def(x), Defs::Def(y)) => std::ptr::eq(*x, *y),
        (Defs::Branch(c, t, e), Defs::Branch(c2, t2, e2)) => std::ptr::eq(*c, *c2) && same(t, t2) && same(e, e2),
        (Defs::Opaque(x), Defs::Opaque(y)) => x == y,
        _ => false,
    }
}

pub(super) struct Flow<'e> {
    body: &'e Expr,
    /// The outcomes of the `if`s decided so far in this attempt.
    taken: RefCell<Vec<(*const Expr, bool)>>,
    /// The `if` an attempt stopped at for want of an outcome.
    needed: Cell<Option<*const Expr>>,
    /// Answer every undecided `if` with its `then` branch and record nothing:
    /// for a question every path answers alike (what model a chain is on).
    probing: Cell<bool>,
}

impl<'e> Flow<'e> {
    pub(super) fn new(body: &'e Expr) -> Self {
        Flow { body, taken: RefCell::new(Vec::new()), needed: Cell::new(None), probing: Cell::new(false) }
    }

    /// Start an attempt from these decisions.
    pub(super) fn begin(&self, decisions: &[(*const Expr, bool)]) {
        *self.taken.borrow_mut() = decisions.to_vec();
        self.needed.set(None);
    }

    /// The `if` the last attempt could not pass.
    pub(super) fn needed(&self) -> Option<&'e Expr> {
        fn find<'x>(e: &'x Expr, at: *const Expr) -> Option<&'x Expr> {
            if std::ptr::eq(e, at) {
                return Some(e);
            }
            let mut found = None;
            e.node.for_each_child(&mut |c| found = found.or_else(|| find(c, at)));
            found
        }
        find(self.body, self.needed.get()?)
    }

    /// `f` with every undecided `if` answered alike (see `probing`).
    pub(super) fn probe<T>(&self, f: impl FnOnce() -> T) -> T {
        let before = self.probing.replace(true);
        let out = f();
        self.probing.set(before);
        out
    }

    /// Suffix that tells one path's statement key from another's.
    pub(super) fn suffix(&self) -> String {
        let taken = self.taken.borrow();
        if taken.is_empty() {
            return String::new();
        }
        format!("_{}", taken.iter().map(|(_, t)| if *t { 't' } else { 'f' }).collect::<String>())
    }

    /// Which branch of this `if` the attempt takes.
    pub(super) fn choose(&self, node: &Expr) -> Result<bool, String> {
        if let Some((_, took)) = self.taken.borrow().iter().find(|(c, _)| std::ptr::eq(*c, node)) {
            return Ok(*took);
        }
        if self.probing.get() {
            return Ok(true);
        }
        self.needed.set(Some(node as *const Expr));
        Err(NEEDS_DECISION.into())
    }

    /// The value expression of the assignment of `name` that reaches the
    /// read `at`, under this attempt's decisions.
    pub(super) fn reaching(&self, at: &Expr, id: VarId, name: &Symbol) -> Result<&'e Expr, String> {
        let mut state = Defs::Unset;
        let mut found = false;
        self.walk(self.body, (id, name), at, &mut state, &mut found);
        if !found {
            return Err(format!("`{}` is not assigned exactly once", name.as_str()));
        }
        self.pick(&state, name)
    }

    fn pick(&self, defs: &Defs<'e>, name: &Symbol) -> Result<&'e Expr, String> {
        match defs {
            Defs::Unset => Err(format!("`{}` is not assigned exactly once", name.as_str())),
            Defs::Opaque(why) => Err(format!("`{}` is reassigned {why}", name.as_str())),
            Defs::Def(value) => Ok(value),
            Defs::Branch(node, then, other) => {
                let branch = if self.choose(node)? { then } else { other };
                self.pick(branch, name)
            }
        }
    }

    /// Evaluate `e` in Ruby's order, tracking what the local holds, until
    /// the read `at` is reached (`found`), whose state is then `state`.
    fn walk(&self, e: &'e Expr, var: (VarId, &Symbol), at: &Expr, state: &mut Defs<'e>, found: &mut bool) {
        if *found {
            return;
        }
        if std::ptr::eq(e, at) {
            *found = true;
            return;
        }
        match &*e.node {
            ExprNode::Seq { exprs } => {
                for x in exprs {
                    self.walk(x, var, at, state, found);
                }
            }
            ExprNode::Assign { target: LValue::Var { id, name }, value } if *id == var.0 && name == var.1 => {
                self.walk(value, var, at, state, found);
                if !*found {
                    *state = Defs::Def(value);
                }
            }
            ExprNode::If { cond, then_branch, else_branch } => {
                self.walk(cond, var, at, state, found);
                if *found {
                    return;
                }
                let mut then_state = state.clone();
                self.walk(then_branch, var, at, &mut then_state, found);
                if *found {
                    *state = then_state;
                    return;
                }
                let mut else_state = state.clone();
                self.walk(else_branch, var, at, &mut else_state, found);
                if *found {
                    *state = else_state;
                    return;
                }
                *state = if same(&then_state, &else_state) { then_state } else { Defs::Branch(e, Box::new(then_state), Box::new(else_state)) };
            }
            ExprNode::Send { recv, args, block, .. } => {
                if let Some(r) = recv {
                    self.walk(r, var, at, state, found);
                }
                for a in args {
                    self.walk(a, var, at, state, found);
                }
                if let Some(b) = block {
                    self.walk_block(b, var, at, state, found);
                }
            }
            _ => {
                let reaches = contains(e, at);
                if reaches {
                    *found = true;
                }
                if assigns(e, var) {
                    *state = Defs::Opaque("where the flow is not followed (a loop, case, rescue, ||=, or multiple assignment)");
                } else if reaches && matches!(&*e.node, ExprNode::Lambda { .. }) {
                    // A closure outside a call's block runs whenever it is called.
                    *state = Defs::Opaque("under a closure that may run later");
                }
            }
        }
    }

    /// A block handed to a call: it runs while the call does, and maybe
    /// again later when the callee keeps it, so a local assigned after it
    /// is not the one its body saw.
    fn walk_block(&self, b: &'e Expr, var: (VarId, &Symbol), at: &Expr, state: &mut Defs<'e>, found: &mut bool) {
        let ExprNode::Lambda { body, .. } = &*b.node else {
            return self.walk(b, var, at, state, found);
        };
        if !assigns(body, var) {
            if contains(b, at) {
                *found = true;
                if self.assigned_after(var, b.span.end) {
                    *state = Defs::Opaque("after a block that reads it");
                }
            }
            return;
        }
        // Assigned in the block: a local of the block when nothing outside
        // assigns it (each call starts fresh), the enclosing one otherwise.
        let local_to_block = matches!(state, Defs::Unset) && count_assigns(body, var) == count_assigns(self.body, var);
        if !local_to_block {
            if contains(b, at) {
                *found = true;
            }
            *state = Defs::Opaque("in a block over a local of the method");
            return;
        }
        if contains(b, at) {
            let mut inner = Defs::Unset;
            self.walk(body, var, at, &mut inner, found);
            *state = inner;
        }
    }

    fn assigned_after(&self, var: (VarId, &Symbol), end: u32) -> bool {
        fn walk(e: &Expr, var: (VarId, &Symbol), end: u32, hit: &mut bool) {
            if let ExprNode::Assign { target: LValue::Var { id, name }, .. } = &*e.node
                && *id == var.0
                && name == var.1
                && e.span.start >= end
            {
                *hit = true;
            }
            e.node.for_each_child(&mut |c| walk(c, var, end, hit));
        }
        let mut hit = false;
        walk(self.body, var, end, &mut hit);
        hit
    }

    /// Whether the body ever writes this instance variable.
    fn writes_ivar(&self, name: &Symbol) -> bool {
        fn walk(e: &Expr, name: &Symbol, hit: &mut bool) {
            match &*e.node {
                ExprNode::Assign { target: LValue::Ivar { name: n }, .. } | ExprNode::OpAssign { target: LValue::Ivar { name: n }, .. } if n == name => *hit = true,
                ExprNode::MultiAssign { targets, .. } if targets.iter().any(|t| matches!(t, LValue::Ivar { name: n } if n == name)) => *hit = true,
                _ => {}
            }
            e.node.for_each_child(&mut |c| walk(c, name, hit));
        }
        let mut hit = false;
        walk(self.body, name, &mut hit);
        hit
    }

    /// Whether a read of this local at `before` sees what it holds at the
    /// call site: it is a parameter (never written) or written once, earlier.
    fn stable_local(&self, id: VarId, name: &Symbol, before: u32) -> bool {
        fn walk<'x>(e: &'x Expr, var: (VarId, &Symbol), starts: &mut Vec<u32>, other: &mut bool) {
            match &*e.node {
                ExprNode::Assign { target: LValue::Var { id, name }, .. } if *id == var.0 && name == var.1 => starts.push(e.span.start),
                ExprNode::OpAssign { target: LValue::Var { id, name }, .. } if *id == var.0 && name == var.1 => *other = true,
                ExprNode::MultiAssign { targets, .. } if targets.iter().any(|t| matches!(t, LValue::Var { id, name } if *id == var.0 && name == var.1)) => *other = true,
                _ => {}
            }
            e.node.for_each_child(&mut |c| walk(c, var, starts, other));
        }
        let (mut starts, mut other) = (Vec::new(), false);
        walk(self.body, (id, name), &mut starts, &mut other);
        !other && starts.iter().all(|s| *s < before) && starts.len() <= 1
    }

    /// The Ruby source of a branch condition the call site may evaluate
    /// again, or why it may not. Reads of locals and instance variables the
    /// method never reassigns and the predicates `present?`, `blank?`,
    /// `nil?`, `any?`, `empty?`, `none?`, `==`, `!=`, `!` and `&&`/`||`
    /// over them: they have no effect of their own, so the call site
    /// reaches the same answer the `if` did.
    pub(super) fn pure_condition(&self, cond: &Expr) -> Result<String, String> {
        if self.pure(cond, cond.span.start) {
            Ok(crate::emit::ruby::emit_expr(cond))
        } else {
            Err("branch condition is not a plain read the call site can repeat".into())
        }
    }

    fn pure(&self, e: &Expr, before: u32) -> bool {
        match &*e.node {
            ExprNode::Var { id, name } => !name.as_str().starts_with("__") && self.stable_local(*id, name, before),
            ExprNode::Ivar { name } => !self.writes_ivar(name),
            ExprNode::Lit { value } => !matches!(value, Literal::Regex { .. }),
            ExprNode::Const { .. } => true,
            ExprNode::Send { recv: None, args, block: None, .. } => args.is_empty(),
            ExprNode::Send { recv: Some(r), method, args, block: None, .. } => {
                let m = method.as_str();
                match (args.len(), m) {
                    (0, "present?" | "blank?" | "nil?" | "any?" | "empty?" | "none?" | "!") => self.pure(r, before),
                    (1, "==" | "!=") => self.pure(r, before) && self.pure(&args[0], before),
                    _ => false,
                }
            }
            ExprNode::BoolOp { left, right, .. } => self.pure(left, before) && self.pure(right, before),
            _ => false,
        }
    }
}

fn contains(e: &Expr, at: &Expr) -> bool {
    if std::ptr::eq(e, at) {
        return true;
    }
    let mut hit = false;
    e.node.for_each_child(&mut |c| hit |= contains(c, at));
    hit
}

/// Does `e` write the local anywhere inside it (by any assignment form)?
fn assigns(e: &Expr, var: (VarId, &Symbol)) -> bool {
    count_assigns(e, var) > 0
}

fn count_assigns(e: &Expr, var: (VarId, &Symbol)) -> usize {
    let own = match &*e.node {
        ExprNode::Assign { target: LValue::Var { id, name }, .. } | ExprNode::OpAssign { target: LValue::Var { id, name }, .. } => usize::from(*id == var.0 && name == var.1),
        ExprNode::MultiAssign { targets, .. } => usize::from(targets.iter().any(|t| matches!(t, LValue::Var { id, name } if *id == var.0 && name == var.1))),
        _ => 0,
    };
    let mut total = own;
    e.node.for_each_child(&mut |c| total += count_assigns(c, var));
    total
}

/// What exploring a site's `if`s found: one statement per path.
enum Paths {
    Leaf(Site),
    Split { cond: String, then: Box<Paths>, other: Box<Paths> },
}

impl Paths {
    /// One site: the call site picks the path's statement by the condition.
    fn into_site(self) -> Site {
        match self {
            Paths::Leaf(site) => site,
            Paths::Split { cond, then, other } => {
                let (then, other) = (then.into_site(), other.into_site());
                Site {
                    span: then.span,
                    call: format!("(({cond}) ? {} : {})", then.call, other.call),
                    define: format!("{}\n  {}", then.define, other.define),
                }
            }
        }
    }
}

impl<'a> Specializer<'a> {
    /// `specialize` for every combination of outcomes of the `if`s the chain
    /// depends on (a local assigned in both branches, an `if` as a link's
    /// receiver); a site that depends on none is specialized as before.
    pub(super) fn specialize_paths(&self, e: &Expr, recv: &Expr, method: &Symbol, args: &[Expr], cx: &Cx) -> Result<Site, String> {
        let paths = self.explore(&mut Vec::new(), cx, &|| self.specialize(e, recv, method, args, cx))?;
        Ok(paths.into_site())
    }

    fn explore(&self, decisions: &mut Vec<(*const Expr, bool)>, cx: &Cx, attempt: &dyn Fn() -> Result<Site, String>) -> Result<Paths, String> {
        cx.flow.begin(decisions);
        let reason = match attempt() {
            Ok(site) if cx.flow.needed.get().is_none() => return Ok(Paths::Leaf(site)),
            Ok(_) => "the chain depends on a branch the specializer did not follow".to_string(),
            Err(reason) => reason,
        };
        let Some(node) = cx.flow.needed() else { return Err(reason) };
        if decisions.len() >= MAX_DECISIONS {
            return Err(format!("chain depends on more than {MAX_DECISIONS} branches"));
        }
        let ExprNode::If { cond, .. } = &*node.node else { return Err(reason) };
        let cond = cx.flow.pure_condition(cond)?;
        let key = node as *const Expr;
        decisions.push((key, true));
        let then = self.explore(decisions, cx, attempt);
        decisions.pop();
        let then = then?;
        decisions.push((key, false));
        let other = self.explore(decisions, cx, attempt);
        decisions.pop();
        Ok(Paths::Split { cond, then: Box::new(then), other: Box::new(other?) })
    }
}

impl<'a> Specializer<'a> {
    /// `lines` written without a receiver in an instance method of a model
    /// that declares `has_many :lines`: the association of `self`, unless the
    /// model also defines a method of that name (the reader would be that).
    pub(super) fn self_assoc_root(&self, method: &Symbol, args: &[Expr], cx: &Cx) -> Result<Option<Chain>, String> {
        let Some(model) = cx.instance_self.as_ref().and_then(|id| self.models.get(id.0.as_str())) else { return Ok(None) };
        let Some(assoc) = model.body.iter().find_map(|item| match item {
            ModelBodyItem::Association { assoc, .. } if assoc.name() == method => Some(assoc),
            _ => None,
        }) else {
            return Ok(None);
        };
        if !matches!(assoc, Association::HasMany { .. }) {
            return Err(format!("`{}` is not a has_many", method.as_str()));
        }
        if model.body.iter().any(|item| matches!(item, ModelBodyItem::Method { method: m, .. } if m.name == *method)) {
            return Err(format!("`{}` is also defined as a method", method.as_str()));
        }
        self.has_many_root(assoc, method, args, cx, || Ok("self".to_string())).map(Some)
    }
}

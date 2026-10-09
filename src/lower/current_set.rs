//! `Current.set(request: r) { … }` → save, assign, run, restore.
//!
//! `ActiveSupport::CurrentAttributes.set` is a scoped write. activesupport
//! 8.2 delegates it to the instance, whose `set` is `Object#with`:
//!
//! ```ruby
//! old_values = {}
//! begin
//!   attributes.each do |key, value|
//!     old_values[key] = public_send(key)
//!     public_send("#{key}=", value)
//!   end
//!   yield self
//! ensure
//!   old_values.each { |key, old_value| public_send("#{key}=", old_value) }
//! end
//! ```
//!
//! `ingest::current_attributes` generates exactly that as a real `set`
//! (instance and class level) on every flattened `Current`, so EVERY call
//! shape runs: a Hash built at run time (Core's
//! `Current.set(pod_attributes(pod_id, options)) { … }`), `**hash`, a
//! forwarded `&block`, a block that takes the instance, or uses its own
//! `next`/`break`.
//!
//! The commonest shape — a literal keyword hash naming the class's
//! attributes, with a plain literal block — is still spelled out here,
//! because the call site names the attributes and each write is then an
//! ordinary typed accessor call, which the strict targets need:
//!
//! ```ruby
//! Current.set(request: r) { body }
//! # →
//! __current_set1_request = Current.request
//! begin
//!   Current.request = r
//!   body
//! ensure
//!   Current.request = __current_set1_request
//! end
//! ```
//!
//! It keeps `with`'s order to the letter. Every value is evaluated
//! first (the Hash is built before `set` runs); then, per pair, the save
//! and the write. A save made after an earlier pair's write sees that
//! write (Core's `Podding::Current#pod_id=` clears `pod_name`). The ensure
//! restores exactly the pairs already saved, in order, so with two or
//! more pairs a progress counter records how far the saves got:
//!
//! ```ruby
//! Current.set(a: x, b: y) { body }
//! # →
//! __current_set1_0_value = x
//! __current_set1_1_value = y
//! __current_set1_saved = 0
//! begin
//!   __current_set1_a = Current.a
//!   __current_set1_saved = 1
//!   Current.a = __current_set1_0_value
//!   __current_set1_b = Current.b
//!   __current_set1_saved = 2
//!   Current.b = __current_set1_1_value
//!   body
//! ensure
//!   Current.a = __current_set1_a if __current_set1_saved >= 1
//!   Current.b = __current_set1_b if __current_set1_saved >= 2
//! end
//! ```
//!
//! ENSURE, NOT A TRAILING ASSIGNMENT: `Current.set` exists to be
//! exception-safe. campfire's `actiontext_opengraph_embeds_test` asserts
//! inside the block, so the first failing assertion would otherwise leave
//! `Current.request` pointing at a test host for every test after it.
//!
//! The block's BODY runs in place. Until this was fixed the whole block
//! (a `Lambda`) was put in the `begin`, so the emitted tree built a
//! lambda there and never called it: every `Podding::Current.set(…) do …
//! end` in Core skipped its body. A body runs in place only when that is
//! the same as yielding to it: the block takes no parameters (`yield
//! self` would bind them), and has no `next`, `break` or `redo` of its
//! own (they leave the block, which no longer exists). Its `return`
//! still returns from the enclosing method through the ensure, as in
//! Ruby. Anything else stays a call to the generated `set`.
//!
//! Each expansion's locals carry a number of their own, so a `set`
//! nested in another one's block (both inlined into one method) keeps
//! its own saved values.

use crate::app::App;
use crate::diagnostic::Diagnostic;
use crate::expr::{Expr, ExprNode, LValue, Literal};
use crate::ident::{Symbol, VarId};
use crate::span::Span;
use std::collections::{BTreeMap, BTreeSet};

/// Per lowered `CurrentAttributes` class: the names a literal `set` may
/// spell inline (a public instance reader `x` and writer `x=`).
type Classes = BTreeMap<String, BTreeSet<String>>;

pub fn apply_current_set_lowering(app: &mut App) -> Vec<Diagnostic> {
    let classes = settable_names(app);
    if classes.is_empty() {
        return Vec::new();
    }
    let mut next_id = 0usize;
    super::for_each_hook_body(app, &mut |e| rewrite(e, &classes, &mut next_id));
    for view in &mut app.views {
        rewrite(&mut view.body, &classes, &mut next_id);
    }
    for tm in &mut app.test_modules {
        if let Some(setup) = &mut tm.setup {
            rewrite(setup, &classes, &mut next_id);
        }
        for t in &mut tm.tests {
            rewrite(&mut t.body, &classes, &mut next_id);
        }
        for m in &mut tm.helpers {
            rewrite(&mut m.body, &classes, &mut next_id);
        }
    }
    Vec::new()
}

fn settable_names(app: &App) -> Classes {
    use crate::dialect::{MethodReceiver, MethodVisibility};
    let mut out = Classes::new();
    for class in &app.current_attribute_classes {
        let mut public: BTreeSet<&str> = BTreeSet::new();
        for lc in app.library_classes.iter().filter(|lc| lc.name == *class) {
            for m in &lc.methods {
                if m.receiver == MethodReceiver::Instance && m.visibility == MethodVisibility::Public {
                    public.insert(m.name.as_str());
                }
            }
        }
        let names = public
            .iter()
            .filter(|n| !n.ends_with('=') && public.contains(format!("{n}=").as_str()))
            .map(|n| n.to_string())
            .collect();
        out.insert(class.0.as_str().to_string(), names);
    }
    out
}

fn rewrite(e: &mut Expr, classes: &Classes, next_id: &mut usize) {
    e.node.for_each_child_mut(&mut |c| rewrite(c, classes, next_id));
    let span = e.span;
    let ExprNode::Send { recv: Some(recv), method, args, block, .. } = &*e.node else { return };
    if method.as_str() != "set" {
        return;
    }
    let ExprNode::Const { path } = &*recv.node else { return };
    // `::Podding::Current` (a rooted path keeps an empty first segment).
    let class = path
        .iter()
        .map(|p| p.as_str())
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("::");
    let Some(settable) = classes.get(&class) else { return };
    // Every shape not spelled here is the class's generated `set`.
    let Some(body) = block.as_ref().and_then(inline_body) else { return };
    let Some(pairs) = keyword_pairs(args) else { return };
    let mut seen = BTreeSet::new();
    if !pairs.iter().all(|(name, _)| settable.contains(name) && seen.insert(name.clone())) {
        return;
    }
    *next_id += 1;
    *e = expand(span, recv, &pairs, body, *next_id);
}

/// The body of a literal block that runs the same in place as yielded
/// to: no parameters, and no `next`/`break`/`redo` of its own.
fn inline_body(block: &Expr) -> Option<Expr> {
    let ExprNode::Lambda { params, rest_param: None, block_param: None, body, signature, .. } = &*block.node else {
        return None;
    };
    if !params.is_empty() || !signature.is_empty() {
        return None;
    }
    let jumps = super::lifted_block::jumps(body);
    if jumps.next || jumps.break_ || jumps.redo {
        return None;
    }
    Some(body.clone())
}

/// `[(attribute, value)]` from the single literal (keyword) hash.
fn keyword_pairs(args: &[Expr]) -> Option<Vec<(String, Expr)>> {
    let [opts] = args else { return None };
    let ExprNode::Hash { entries, .. } = &*opts.node else { return None };
    if entries.is_empty() {
        return None;
    }
    let mut out = Vec::new();
    for (k, v) in entries {
        let name = match &*k.node {
            ExprNode::Lit { value: Literal::Sym { value } } => value.as_str().to_string(),
            ExprNode::Lit { value: Literal::Str { value } } => value.clone(),
            _ => return None,
        };
        out.push((name, v.clone()));
    }
    Some(out)
}

/// A value whose evaluation neither has effects nor reads an attribute,
/// so evaluating it at its write rather than before the saves is the
/// same.
fn inert(value: &Expr) -> bool {
    matches!(
        &*value.node,
        ExprNode::Lit { .. } | ExprNode::Var { .. } | ExprNode::Const { .. } | ExprNode::SelfRef
    )
}

fn expand(span: Span, recv: &Expr, pairs: &[(String, Expr)], body: Expr, id: usize) -> Expr {
    let local = |suffix: &str| Symbol::from(format!("__current_set{id}_{suffix}"));
    let mut stmts: Vec<Expr> = Vec::new();
    // The Hash is built before `set` runs: every value first.
    let values: Vec<Expr> = pairs
        .iter()
        .enumerate()
        .map(|(i, (_, value))| {
            if inert(value) {
                return value.clone();
            }
            let tmp = local(&format!("{i}_value"));
            stmts.push(assign_local(span, tmp.clone(), value.clone()));
            local_read(span, tmp)
        })
        .collect();
    let saved: Vec<Symbol> = pairs.iter().map(|(name, _)| local(name)).collect();
    let mut inside: Vec<Expr> = Vec::new();
    let mut restore: Vec<Expr> = Vec::new();
    if let [(name, _)] = pairs {
        // One pair: a save that raises has nothing to restore, so it
        // can precede the `begin`; the write is inside it.
        stmts.push(assign_local(span, saved[0].clone(), read(span, recv, name)));
        inside.push(write(span, recv, name, values[0].clone()));
        restore.push(write(span, recv, name, local_read(span, saved[0].clone())));
    } else {
        let progress = local("saved");
        stmts.push(assign_local(span, progress.clone(), int(span, 0)));
        for (i, ((name, _), value)) in pairs.iter().zip(values).enumerate() {
            inside.push(assign_local(span, saved[i].clone(), read(span, recv, name)));
            inside.push(assign_local(span, progress.clone(), int(span, i as i64 + 1)));
            inside.push(write(span, recv, name, value));
            let reached = Expr::new(
                span,
                ExprNode::Send {
                    recv: Some(local_read(span, progress.clone())),
                    method: Symbol::from(">="),
                    args: vec![int(span, i as i64 + 1)],
                    block: None,
                    parenthesized: false,
                },
            );
            restore.push(Expr::new(
                span,
                ExprNode::If {
                    cond: reached,
                    then_branch: write(span, recv, name, local_read(span, saved[i].clone())),
                    else_branch: Expr::new(span, ExprNode::Lit { value: Literal::Nil }),
                },
            ));
        }
    }
    inside.push(body);
    stmts.push(Expr::new(
        span,
        ExprNode::BeginRescue {
            body: seq(span, inside),
            rescues: Vec::new(),
            else_branch: None,
            ensure: Some(seq(span, restore)),
            implicit: false,
        },
    ));
    seq(span, stmts)
}

fn seq(span: Span, exprs: Vec<Expr>) -> Expr {
    if exprs.len() == 1 {
        return exprs.into_iter().next().expect("checked");
    }
    Expr::new(span, ExprNode::Seq { exprs })
}

fn int(span: Span, value: i64) -> Expr {
    Expr::new(span, ExprNode::Lit { value: Literal::Int { value } })
}

fn read(span: Span, recv: &Expr, name: &str) -> Expr {
    Expr::new(
        span,
        ExprNode::Send {
            recv: Some(recv.clone()),
            method: Symbol::from(name),
            args: vec![],
            block: None,
            parenthesized: false,
        },
    )
}

fn write(span: Span, recv: &Expr, name: &str, value: Expr) -> Expr {
    Expr::new(
        span,
        ExprNode::Send {
            recv: Some(recv.clone()),
            method: Symbol::from(format!("{name}=")),
            args: vec![value],
            block: None,
            parenthesized: false,
        },
    )
}

fn assign_local(span: Span, name: Symbol, value: Expr) -> Expr {
    Expr::new(
        span,
        ExprNode::Assign {
            target: LValue::Var { id: VarId(0), name },
            value,
        },
    )
}

fn local_read(span: Span, name: Symbol) -> Expr {
    Expr::new(span, ExprNode::Var { id: VarId(0), name })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ident::ClassId;

    /// An app with a lowered `Current` whose attributes are `request`,
    /// `user` and `a` (public reader and writer each), and one test.
    fn app_with(body: &str) -> App {
        let src = format!("class T < ActiveSupport::TestCase\n  test \"t\" do\n    {body}\n  end\nend\n");
        let mut app = App::new();
        app.current_attribute_classes.push(ClassId(Symbol::from("Current")));
        let current = "class Current\n  def request\n    @request\n  end\n  def request=(value)\n    @request = value\n  end\n  def user\n    @user\n  end\n  def user=(value)\n    @user = value\n  end\n  def a\n    @a\n  end\n  def a=(value)\n    @a = value\n  end\n  def only_read\n    1\n  end\nend\n";
        app.library_classes
            .extend(crate::ingest::ingest_library_classes(current.as_bytes(), "current.rb").expect("ingest current"));
        app.test_modules
            .extend(crate::ingest::test::ingest_test_files(src.as_bytes(), "t.rb").expect("ingest"));
        app
    }

    fn lowered(app: &App) -> String {
        crate::emit::ruby::emit_expr(&app.test_modules[0].tests[0].body)
    }

    fn lower(body: &str) -> String {
        let mut app = app_with(body);
        let diags = apply_current_set_lowering(&mut app);
        assert!(diags.is_empty(), "{diags:?}");
        lowered(&app)
    }

    #[test]
    fn one_attribute_saves_assigns_and_restores_in_an_ensure() {
        let out = lower("r = 1\n    Current.set(request: r) do\n      work\n    end");
        assert!(out.contains("__current_set1_request = Current.request"), "{out}");
        assert!(out.contains("Current.request = r"), "{out}");
        assert!(!out.contains("_value"), "a local needs no temporary:\n{out}");
        assert!(out.contains("ensure"), "{out}");
        assert!(out.contains("Current.request = __current_set1_request"), "{out}");
    }

    /// The block's body runs where the block was: the regression was a
    /// lambda literal (`-> { work }`) in the `begin`, built and never
    /// called, so every inlined `set` skipped its body.
    #[test]
    fn the_block_body_runs_in_place_not_as_a_lambda() {
        let out = lower("Current.set(request: r) do\n      work\n      more\n    end");
        assert!(!out.contains("->"), "the body must run, not become a lambda:\n{out}");
        let begin = out.find("begin").expect(&out);
        let ensure = out.find("ensure").expect(&out);
        let work = out.find("work").expect(&out);
        assert!(begin < work && work < ensure, "the body runs inside the begin:\n{out}");
    }

    /// `with`'s order: save then write, per pair, after every value has
    /// been evaluated; the ensure restores only what was saved.
    #[test]
    fn two_attributes_save_and_write_in_turn_and_restore_what_was_saved() {
        let out = lower("u = 1\n    Current.set(request: make_request, user: u) do\n      work\n    end");
        let value = out.find("__current_set1_0_value = make_request").expect(&out);
        let save_request = out.find("__current_set1_request = Current.request").expect(&out);
        let write_request = out.find("Current.request = __current_set1_0_value").expect(&out);
        let save_user = out.find("__current_set1_user = Current.user").expect(&out);
        let write_user = out.find("Current.user = u").expect(&out);
        assert!(value < save_request, "the value is evaluated before any save:\n{out}");
        assert!(save_request < write_request && write_request < save_user && save_user < write_user, "{out}");
        assert!(out.contains("Current.user = __current_set1_user if __current_set1_saved >= 2"), "{out}");
        assert!(out.contains("Current.request = __current_set1_request if __current_set1_saved >= 1"), "{out}");
    }

    /// A Hash built at run time (Core's `Podding::Selector.select_pod`
    /// passes `pod_attributes(pod_id, options)`), `**hash`, a forwarded
    /// `&block`, a block taking the instance, its own `next`/`break`, and
    /// a name that is not a reader/writer pair all reach the generated
    /// `set` unchanged.
    #[test]
    fn every_other_shape_calls_the_generated_set() {
        for call in [
            "Current.set(attrs) do\n      work\n    end",
            "Current.set(**attrs) do\n      work\n    end",
            "Current.set(request: r, &blk)",
            "Current.set(request: r) do |c|\n      c.work\n    end",
            "Current.set(request: r) do\n      next 1 if x\n      work\n    end",
            "Current.set(request: r) do\n      break\n    end",
            "Current.set(only_read: 1) do\n      work\n    end",
            "Current.set(zzz: 1) do\n      work\n    end",
            "Current.set(request: r)",
        ] {
            let out = lower(call);
            assert!(out.contains("Current.set("), "{call:?} must stay a call:\n{out}");
            assert!(!out.contains("__current_set"), "{call:?}:\n{out}");
        }
    }

    /// A nested `next` belongs to its own block; only the set block's
    /// own jumps keep the call.
    #[test]
    fn a_nested_blocks_next_does_not_keep_the_call() {
        let out = lower("Current.set(request: r) do\n      items.each { |i| next if i }\n    end");
        assert!(!out.contains("Current.set("), "{out}");
        assert!(out.contains("next if i"), "{out}");
    }

    /// `::Current` is the same class.
    #[test]
    fn a_rooted_receiver_is_lowered_and_keeps_its_root() {
        let out = lower("::Current.set(request: 1) do\n      work\n    end");
        assert!(!out.contains(".set("), "{out}");
        assert!(out.contains("::Current.request = 1"), "{out}");
    }

    /// Nested sets of the same attribute in one method each keep their
    /// own saved value.
    #[test]
    fn nested_sets_keep_their_own_saved_values() {
        let out = lower("Current.set(a: 1) do\n      Current.set(a: 2) do\n        work\n      end\n    end");
        assert!(out.contains("__current_set1_a = Current.a"), "{out}");
        assert!(out.contains("__current_set2_a = Current.a"), "{out}");
        assert!(out.contains("Current.a = __current_set1_a"), "{out}");
        assert!(out.contains("Current.a = __current_set2_a"), "{out}");
    }

    /// `set` on anything that is not one of the app's own
    /// CurrentAttributes classes is somebody else's method.
    #[test]
    fn another_receivers_set_is_untouched() {
        let out = lower("Cache.set(key: k) do\n      work\n    end");
        assert!(out.contains("Cache.set"), "{out}");
    }
}

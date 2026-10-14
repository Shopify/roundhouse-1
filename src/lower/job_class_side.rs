//! ActiveJob class-side call idiom: `NotifyCommentJob.perform_later(c)`
//! enqueues; under the transpiled runtime the adapter is `:inline` (the
//! solid_queue-era starting point — no queue daemon in-process), so the
//! class-side entry runs the job synchronously:
//!
//!   def self.perform_later(comment)
//!     new.perform(comment)
//!   end
//!
//! `perform_now` gets the identical wrapper (its Rails semantics is
//! already synchronous). `SendWebmentionJob.set(wait: 5.minutes)`
//! returns a scheduling proxy in Rails; inline semantics has nothing
//! to defer, so `Job.set(…).perform_later(args)` folds to
//! `Job.perform_later(args)` at the call site, the dropped options
//! ledgered as residue (see `fold_set_chains`).
//!
//! Job classes are those whose parent chain (within the ingested set)
//! reaches ActiveJob::Base. Same guards as the mailer twin
//! (`mailer_class_side`): positional forwarding only — kwarg or block
//! `perform`s stay unwrapped on the residue ledger.
//!
//! Structural pass on `app.library_classes` (where `app/jobs/*.rb`
//! ingest); runs on the post-analyze hook so every target's tree
//! carries the wrappers.

use std::collections::BTreeSet;

use crate::app::App;
use crate::diagnostic::Diagnostic;
use crate::dialect::LibraryClass;
use crate::expr::{Expr, ExprNode};
use crate::ident::Symbol;
use crate::ty::Ty;

/// A library class's parent, looked up from the class's namespace outward
/// among `classes` as Ruby binds the written name (`class TrackCreateJob <
/// TrackJob` in `module ShopEvent` is `ShopEvent::TrackJob`), as
/// `emit::ruby::library::lexical_parent` does. Ingest records the name as
/// written and binds it only after the concern splices, which read it
/// (`library_class::resolve_library_parents`). A name no class in
/// `classes` (the names of the app's classes, [`class_names`]) has at any
/// level stays as written (`ActiveJob::Base`).
pub(crate) fn lexical_parent_name(lc: &LibraryClass, classes: &BTreeSet<&str>) -> Option<String> {
    let written = lc.parent.as_ref()?.0.as_str();
    if let Some(absolute) = written.strip_prefix("::") {
        return Some(absolute.to_string());
    }
    let mut scope: Vec<&str> = lc.name.0.as_str().split("::").collect();
    scope.pop();
    while !scope.is_empty() {
        let candidate = format!("{}::{written}", scope.join("::"));
        if candidate != lc.name.0.as_str() && classes.contains(candidate.as_str()) {
            return Some(candidate);
        }
        scope.pop();
    }
    Some(written.to_string())
}

/// The names of the classes (not modules) among `classes`.
pub(crate) fn class_names(classes: &[LibraryClass]) -> BTreeSet<&str> {
    classes.iter().filter(|c| !c.is_module).map(|c| c.name.0.as_str()).collect()
}

/// The app's job classes: those whose parent chain (within the ingested
/// set) reaches ActiveJob::Base (ApplicationJob names ActiveJob::Base;
/// concrete jobs name ApplicationJob).
pub(crate) fn job_class_names(classes: &[LibraryClass]) -> BTreeSet<String> {
    let names = class_names(classes);
    let parents: Vec<(&str, Option<String>)> =
        classes.iter().map(|lc| (lc.name.0.as_str(), lexical_parent_name(lc, &names))).collect();
    let mut jobs: BTreeSet<String> = BTreeSet::new();
    loop {
        let mut changed = false;
        for (name, parent) in &parents {
            if jobs.contains(*name) {
                continue;
            }
            if let Some(ps) = parent {
                if ps == "ActiveJob::Base" || jobs.contains(ps) {
                    jobs.insert(name.to_string());
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    jobs
}

/// The modules each `include M, ...` statement of `class`'s body names,
/// as written, with the statement's offset: where each include runs.
/// Only the body's own statements, in each declaration of `class` the
/// file has. The emitter writes a job's include below one of its own
/// callback registrations where it stands among them
/// (`emit::ruby::library`).
pub(crate) fn include_offsets(text: &str, class: &str) -> Vec<(String, u32)> {
    struct Find<'a> {
        scope: Vec<String>,
        class: &'a str,
        out: Vec<(String, u32)>,
    }
    impl Find<'_> {
        fn enter(&mut self, path: &[u8], body: Option<ruby_prism::Node<'_>>) {
            self.scope.push(String::from_utf8_lossy(path).trim_start_matches("::").to_string());
            if let Some(body) = body {
                if self.scope.join("::") == self.class
                    && let Some(statements) = body.as_statements_node()
                {
                    for statement in statements.body().iter() {
                        let Some(call) = statement.as_call_node() else { continue };
                        if call.receiver().is_some() || call.name().as_slice() != b"include" {
                            continue;
                        }
                        let at = call.location().start_offset() as u32;
                        for arg in call.arguments().iter().flat_map(|a| a.arguments().iter()) {
                            if arg.as_constant_read_node().is_some() || arg.as_constant_path_node().is_some() {
                                let written = String::from_utf8_lossy(arg.location().as_slice());
                                self.out.push((written.trim_start_matches("::").to_string(), at));
                            }
                        }
                    }
                }
                ruby_prism::Visit::visit(self, &body);
            }
            self.scope.pop();
        }
    }
    impl<'pr> ruby_prism::Visit<'pr> for Find<'_> {
        fn visit_class_node(&mut self, node: &ruby_prism::ClassNode<'pr>) {
            self.enter(node.constant_path().location().as_slice(), node.body());
        }
        fn visit_module_node(&mut self, node: &ruby_prism::ModuleNode<'pr>) {
            self.enter(node.constant_path().location().as_slice(), node.body());
        }
    }
    let mut find = Find { scope: Vec::new(), class: class.trim_start_matches("::"), out: Vec::new() };
    let parsed = ruby_prism::parse(text.as_bytes());
    ruby_prism::Visit::visit(&mut find, &parsed.node());
    find.out
}

/// The port module a job hierarchy that registers callbacks includes
/// (runtime/spinel/active_job_callbacks.rb).
pub(crate) const CALLBACKS_PORT: &str = "ActiveJob::Callbacks";

/// Whether `call` is a bare or `self.` call of a class method the
/// ActiveJob::Callbacks port gives a job (`before_perform`,
/// `around_enqueue`, `set_callback`, `rescue_from`, ...): what Rails'
/// ActiveJob::Base answers in a job's body. The names are the port's own
/// (`runtime_reads::mixed_in_class_methods`).
pub(crate) fn is_callbacks_port_call(call: &Expr) -> bool {
    let ExprNode::Send { recv, method, .. } = &*call.node else { return false };
    recv.as_ref().is_none_or(|r| matches!(&*r.node, ExprNode::SelfRef))
        && crate::analyze::runtime_reads::mixed_in_class_methods(CALLBACKS_PORT)
            .is_some_and(|names| names.iter().any(|n| n == method.as_str()))
}

/// The statements of a module's `self.included(base)` hook when each one
/// is a call of an ActiveJob::Callbacks class method on `base`
/// (`base.around_enqueue do ... end`): Core's BackgroundQueue::WithFallback
/// and MeasureJobPerformance. Such a hook only registers callbacks on the
/// includer, whatever else the include carries; `None` for any other hook
/// or no hook.
pub(crate) fn callback_registering_hook(module: &LibraryClass) -> Option<Vec<&Expr>> {
    let hook = module.methods.iter().find(|m| {
        m.receiver == crate::dialect::MethodReceiver::Class
            && m.name.as_str() == "included"
            && m.params.len() == 1
            && !m.params[0].keyword
            && m.params[0].default.is_none()
            && m.block_param.is_none()
    })?;
    let base = &hook.params[0].name;
    let statements: Vec<&Expr> = match &*hook.body.node {
        ExprNode::Seq { exprs } => exprs.iter().collect(),
        _ => vec![&hook.body],
    };
    let registers = |e: &Expr| {
        let ExprNode::Send { recv: Some(r), method, .. } = &*e.node else { return false };
        matches!(&*r.node, ExprNode::Var { name, .. } if name == base)
            && crate::analyze::runtime_reads::mixed_in_class_methods(CALLBACKS_PORT)
                .is_some_and(|names| names.iter().any(|n| n == method.as_str()))
    };
    (!statements.is_empty() && statements.iter().all(|e| registers(e))).then_some(statements)
}

/// The jobs that run on the ActiveJob::Callbacks port: every job below
/// the topmost app job class of a hierarchy one of whose classes
/// registers a callback or a handler, in its body, through a carried
/// `included do` statement (`App::included_block_statements`) or through
/// an included module's hook (`callback_registering_hook`). Each such
/// topmost class includes the port first, as Rails' ActiveJob::Base
/// includes ActiveJob::Callbacks ahead of everything an app job class
/// includes.
fn adopt_callbacks_port(app: &mut App, jobs: &BTreeSet<String>) -> BTreeSet<String> {
    let names = class_names(&app.library_classes);
    let parents: std::collections::BTreeMap<&str, String> = app
        .library_classes
        .iter()
        .filter(|lc| jobs.contains(lc.name.0.as_str()))
        .filter_map(|lc| Some((lc.name.0.as_str(), lexical_parent_name(lc, &names)?)))
        .filter(|(_, p)| jobs.contains(p))
        .collect();
    let parent_of = |name: &str| -> Option<String> { parents.get(name).cloned() };
    let top_of = |name: &str| -> String {
        let mut at = name.to_string();
        let mut seen = BTreeSet::new();
        while let Some(parent) = parent_of(&at) {
            if !seen.insert(at.clone()) {
                break;
            }
            at = parent;
        }
        at
    };
    let hooked = |inc: &crate::ident::ClassId| {
        let name = inc.0.as_str().trim_start_matches("::");
        app.library_classes
            .iter()
            .filter(|m| m.is_module && m.name.0.as_str() == name)
            .any(|m| callback_registering_hook(m).is_some())
    };
    let mut tops: BTreeSet<String> = BTreeSet::new();
    for lc in app.library_classes.iter().filter(|lc| jobs.contains(lc.name.0.as_str())) {
        // A literal `queue_as` or `discard_on ActiveJob::DeserializationError`
        // is carried by the job's payload plan (`lower::job_payload`)
        // and needs no chain: alone, it keeps the job on the wrappers.
        let registers = lc
            .unknown_calls
            .iter()
            .any(|c| is_callbacks_port_call(c) && !super::job_payload::is_plan_modelled_call(c))
            || app.included_block_statements.get(&lc.name).is_some_and(|s| !s.is_empty())
            || lc.includes.iter().any(hooked);
        if registers {
            tops.insert(top_of(lc.name.0.as_str()));
        }
    }
    if tops.is_empty() {
        return BTreeSet::new();
    }
    let on_port: BTreeSet<String> = jobs.iter().filter(|job| tops.contains(&top_of(job))).cloned().collect();
    let port = crate::ident::ClassId(Symbol::from(CALLBACKS_PORT));
    for lc in app.library_classes.iter_mut().filter(|lc| tops.contains(lc.name.0.as_str())) {
        if !lc.includes.contains(&port) {
            lc.includes.insert(0, port.clone());
        }
    }
    on_port
}

pub fn apply_job_class_side(app: &mut App) -> Vec<Diagnostic> {
    let mut diags = Vec::new();

    let jobs = job_class_names(&app.library_classes);
    let workers = sidekiq_workers(app);
    let wrapped = workers_with_positional_perform(app, &workers);
    let has_perform = workers_with_any_perform(app, &workers);
    let delay_folds = without_delayed_enqueue_overrides(app, &wrapped);
    if jobs.is_empty() && workers.is_empty() {
        return diags;
    }
    let on_port = adopt_callbacks_port(app, &jobs);

    // `Job.set(wait:/queue:/priority:).perform_later(args)` folds to
    // `Job.perform_later(args)` at the call site. Inline semantics has
    // nothing to schedule, so the options were always dropped; folding
    // drops them HERE (ledgered per site) instead of routing through a
    // class-side `set` answering the class object — which no target's
    // types can name (the RBS said `-> Job`, an instance, and spinel's
    // C returned an `sp_Class` through it: 13 cc errors on lobsters).
    let mut unfolded: BTreeSet<String> = BTreeSet::new();
    let mut folded: Vec<crate::span::Span> = Vec::new();
    let mut delayed: Vec<crate::span::Span> = Vec::new();
    super::for_each_hook_body(app, &mut |body| {
        fold_set_chains(body, &jobs, &mut unfolded, &mut folded);
        fold_delayed_enqueues(body, &delay_folds, &mut delayed);
    });
    for span in delayed {
        diags.push(crate::lower::residue_diagnostic(
            "job_class_side",
            "worker-delay",
            span,
            "inline job semantics",
            "`perform_in`/`perform_at` delays are dropped under inline job semantics".to_string(),
        ));
    }
    for span in folded {
        diags.push(crate::lower::residue_diagnostic(
            "job_class_side",
            "job-set-options",
            span,
            "inline job semantics",
            "`set(wait:/queue:/priority:)` options are dropped under inline job semantics"
                .to_string(),
        ));
    }

    // The jobs given a synthesized `perform_later`: the ones
    // `lower::job_payload` can plan a payload for.
    let mut payload_jobs: BTreeSet<String> = BTreeSet::new();
    for lc in app.library_classes.iter_mut() {
        let is_worker = workers.contains(lc.name.0.as_str());
        if !is_worker && !jobs.contains(lc.name.0.as_str()) {
            continue;
        }
        if is_worker {
            // A worker with no `perform` anywhere in its own chain is an
            // abstract base, not an entry point: strip its Sidekiq wiring
            // quietly, as a wrapped worker's is stripped below, and
            // generate no wrapper. A concrete subclass that defines its
            // own positional `perform` is still wrapped (`wrapped`
            // walks the same chain), so this only drops the base's own
            // dead include and options.
            if !has_perform.contains(lc.name.0.as_str()) {
                lc.includes.retain(|inc| !SIDEKIQ_MODULES.contains(&inc.0.as_str()));
                lc.unknown_calls.retain(|call| {
                    !matches!(&*call.node, ExprNode::Send { recv: None, method, .. }
                        if SIDEKIQ_CLASS_MACROS.contains(&method.as_str()))
                });
                continue;
            }
            // The include and options stay on a worker no wrapper is generated for, so it reads as the unsupported class it is.
            if !wrapped.contains(lc.name.0.as_str()) {
                let span = lc
                    .methods
                    .iter()
                    .find(|m| m.receiver == crate::dialect::MethodReceiver::Instance && m.name.as_str() == "perform")
                    .map_or(lc.parent_span, |m| m.name_span);
                diags.push(Diagnostic::unsupported(span, None, "Sidekiq worker without a positional perform",
                    "the inline perform_async wrapper forwards positional arguments only"));
                continue;
            }
            lc.includes.retain(|inc| !SIDEKIQ_MODULES.contains(&inc.0.as_str()));
            let before = lc.unknown_calls.len();
            lc.unknown_calls.retain(|call| {
                !matches!(&*call.node, ExprNode::Send { recv: None, method, .. }
                    if SIDEKIQ_CLASS_MACROS.contains(&method.as_str()))
            });
            if lc.unknown_calls.len() != before {
                diags.push(crate::lower::residue_diagnostic(
                    "job_class_side",
                    "worker-options",
                    crate::span::Span::synthetic(),
                    "inline job semantics",
                    format!(
                        "job_class_side: `{}` — sidekiq_options / retry hooks are dropped under inline job semantics",
                        lc.name.0.as_str()
                    ),
                ));
            }
        }
        let class_side: BTreeSet<&str> = lc
            .methods
            .iter()
            .filter(|m| m.receiver == crate::dialect::MethodReceiver::Class)
            .map(|m| m.name.as_str())
            .collect();
        let Some(perform) = lc
            .methods
            .iter()
            .find(|m| {
                m.receiver == crate::dialect::MethodReceiver::Instance
                    && m.name.as_str() == "perform"
            })
            .cloned()
        else {
            continue;
        };
        // A job on the ActiveJob::Callbacks port takes `perform_now` and
        // `perform_later` from it, which run its chains around `perform`
        // and the enqueue, forward every kind of argument and answer what
        // Rails' do. Only `set` is still this pass's.
        let port = on_port.contains(lc.name.0.as_str());
        if !port {
            if perform.params.iter().any(|p| p.forwarding) {
                diags.push(Diagnostic::unsupported(perform.name_span, None, "full forwarding job wrapper",
                    "the generated job wrapper cannot preserve a full forwarding packet"));
                continue;
            }
            if perform.params.iter().any(crate::dialect::Param::is_nameless_rest) {
                diags.push(Diagnostic::unsupported(perform.name_span, None, "anonymous positional rest job wrapper",
                    "the generated job wrapper has no name to forward a nameless rest by"));
                continue;
            }
            if perform.params.iter().any(|p| p.keyword) {
                diags.push(residue(&perform, "keyword parameters do not forward positionally"));
                continue;
            }
            if perform.block_param.is_some() {
                diags.push(residue(&perform, "the wrapper cannot forward a block"));
                continue;
            }
        }

        let (param_tys, ret_ty): (Vec<Option<Ty>>, Option<Ty>) = match &perform.signature {
            Some(Ty::Fn { params, ret, .. }) if params.len() == perform.params.len() => (
                params.iter().map(|p| Some((*p.ty).clone())).collect(),
                Some((**ret).clone()),
            ),
            _ => (vec![None; perform.params.len()], None),
        };

        let span = perform.body.span;
        let mut wrappers: Vec<crate::dialect::MethodDef> = Vec::new();
        let entries: &[&str] = if is_worker {
            &["perform_async"]
        } else if port {
            &[]
        } else {
            &["perform_later", "perform_now"]
        };
        for &entry in entries {
            if class_side.contains(entry) {
                continue;
            }
            let args: Vec<Expr> = perform
                .params
                .iter()
                .zip(param_tys.iter())
                .enumerate()
                .map(|(i, (p, ty))| {
                    let mut v = Expr::new(
                        span,
                        ExprNode::Var {
                            id: crate::ident::VarId(i as u32),
                            name: p.name.clone(),
                        },
                    );
                    v.ty = ty.clone();
                    // The wrapper declares the instance method's params as
                    // they are, flattened keywords included.
                    crate::lower::keyword_marks::mark_forwarded_param(&mut v, p);
                    v
                })
                .collect();
            let mut new_call = Expr::new(
                span,
                ExprNode::Send {
                    recv: None,
                    method: Symbol::from("new"),
                    args: vec![],
                    block: None,
                    parenthesized: false,
                },
            );
            new_call.ty = Some(Ty::Class { id: lc.name.clone(), args: vec![].into() });
            let mut body = Expr::new(
                span,
                ExprNode::Send {
                    recv: Some(new_call),
                    method: Symbol::from("perform"),
                    args,
                    block: None,
                    parenthesized: true,
                },
            );
            body.ty = ret_ty.clone();
            // `perform_later` LOGS before dispatching. Rails' test
            // helpers count enqueues, and the inline adapter has no
            // queue to count — so the log is the seam that answers
            // them, and `perform_later` is the only entry that
            // enqueues (`perform_now` runs without one, in Rails as
            // here).
            let body = if matches!(entry, "perform_later" | "perform_async") {
                let mut record = Expr::new(
                    span,
                    ExprNode::Send {
                        recv: Some(Expr::new(
                            span,
                            ExprNode::Const { path: vec![Symbol::from("ActiveJob")] },
                        )),
                        method: Symbol::from("record_performed"),
                        args: vec![{
                            let mut lit = Expr::new(
                                span,
                                ExprNode::Lit {
                                    value: crate::expr::Literal::Str {
                                        value: lc.name.0.as_str().to_string(),
                                    },
                                },
                            );
                            lit.ty = Some(Ty::Str);
                            lit
                        }],
                        block: None,
                        parenthesized: true,
                    },
                );
                record.ty = Some(Ty::Nil);
                // …and THEN ONE OF THREE THINGS, in this order.
                //
                // `ActiveJob.enqueue_only` — the `:test` adapter: record
                // and return. Rails' test environment runs `:test`, not
                // `:inline`, and the difference is load-bearing rather
                // than cosmetic: campfire's `Message` has
                // `after_create_commit -> { room.receive(self) }` whose
                // tail is a `perform_later`, so dispatching would run
                // `Room::MessagePusher` for every message a FIXTURE
                // loads and take the whole suite down in its
                // unresolvable nested join.
                //
                // `ActiveJob.drain_registered` — a real queue: hand the
                // work over and return, which is what Rails' production
                // adapters do and what `perform_later` has always
                // promised. A ZERO-ARGUMENT PROC closing over the
                // arguments is what makes this expressible on a strict
                // target: a job queue is heterogeneous by nature, and
                // `() -> nil` is the one type every entry shares
                // whatever it closed over.
                //
                // Otherwise INLINE, unchanged. A job put on a queue
                // nobody drains is a job silently dropped, so the
                // enqueue arm is taken only when a drain has said it
                // exists — `main.rb` registers one, the emitted test
                // harness does not.
                let active_job = |sp| {
                    Expr::new(sp, ExprNode::Const { path: vec![Symbol::from("ActiveJob")] })
                };
                let mut nil_arm = Expr::new(
                    span,
                    ExprNode::Lit { value: crate::expr::Literal::Nil },
                );
                nil_arm.ty = Some(Ty::Nil);

                // The Proc: `{ new.perform(...); nil }`. The trailing
                // nil is what makes every entry's type the same one —
                // `perform` answers whatever the app wrote.
                let mut block_body = Expr::new(
                    span,
                    ExprNode::Seq { exprs: vec![body.clone(), nil_arm.clone()] },
                );
                block_body.ty = Some(Ty::Nil);
                let block = Expr::new(
                    span,
                    ExprNode::Lambda { signature: Default::default(), extra_params: Vec::new(),
                        params: Vec::new(),
                        rest_param: None,
                        block_param: None,
                        body: block_body,
                        block_style: crate::expr::BlockStyle::Do,
                    },
                );
                // AN ARGUMENT, NOT A BLOCK: `ActiveJob.enqueue` names its
                // parameter `^() -> nil` so the runtime file types
                // fully, and RBS has no syntax for naming a block.
                let held_block = block.clone();
                let mut enqueue = Expr::new(
                    span,
                    ExprNode::Send {
                        recv: Some(active_job(span)),
                        method: Symbol::from("enqueue"),
                        args: vec![block],
                        block: None,
                        parenthesized: true,
                    },
                );
                enqueue.ty = Some(Ty::Nil);

                let mut drained = Expr::new(
                    span,
                    ExprNode::Send {
                        recv: Some(active_job(span)),
                        method: Symbol::from("drain_registered"),
                        args: vec![],
                        block: None,
                        parenthesized: true,
                    },
                );
                drained.ty = Some(Ty::Bool);
                let mut queued_or_inline = Expr::new(
                    span,
                    ExprNode::If {
                        cond: drained,
                        then_branch: enqueue,
                        else_branch: body,
                    },
                );
                queued_or_inline.ty = Some(Ty::Nil);

                let mut gate = Expr::new(
                    span,
                    ExprNode::Send {
                        recv: Some(active_job(span)),
                        method: Symbol::from("enqueue_only"),
                        args: vec![],
                        block: None,
                        parenthesized: true,
                    },
                );
                gate.ty = Some(Ty::Bool);
                let mut cond = Expr::new(
                    span,
                    ExprNode::Send {
                        recv: Some(gate),
                        method: Symbol::from("!"),
                        args: Vec::new(),
                        block: None,
                        parenthesized: false,
                    },
                );
                cond.ty = Some(Ty::Bool);
                // Under `enqueue_only` the work is HELD rather than
                // dropped, so a blockless `perform_enqueued_jobs` can run
                // it later, as Rails' `:test` adapter can
                // (basecamp/once-campfire#296's tests post first and
                // perform after). Same Proc the drain takes.
                let mut hold = Expr::new(
                    span,
                    ExprNode::Send {
                        recv: Some(active_job(span)),
                        method: Symbol::from("hold"),
                        args: vec![
                            {
                                let mut lit = Expr::new(
                                    span,
                                    ExprNode::Lit {
                                        value: crate::expr::Literal::Str {
                                            value: lc.name.0.as_str().to_string(),
                                        },
                                    },
                                );
                                lit.ty = Some(Ty::Str);
                                lit
                            },
                            held_block,
                        ],
                        block: None,
                        parenthesized: true,
                    },
                );
                hold.ty = Some(Ty::Nil);
                let mut guarded = Expr::new(
                    span,
                    ExprNode::If {
                        cond,
                        then_branch: queued_or_inline,
                        else_branch: hold,
                    },
                );
                guarded.ty = Some(Ty::Nil);
                // The wrapper answers NIL, not the perform's value.
                // Rails' `perform_later` answers the job (or `false`),
                // never the result — nothing in any corpus reads it —
                // and a Nil return is what lets the guarded call sit in
                // statement position on the strict targets instead of
                // forcing a `<perform-return> | nil` union.
                let mut seq =
                    Expr::new(span, ExprNode::Seq { exprs: vec![record, guarded, nil_arm] });
                seq.ty = Some(Ty::Nil);
                seq
            } else {
                body
            };
            let mut w = perform.clone();
            w.name = Symbol::from(entry);
            w.receiver = crate::dialect::MethodReceiver::Class;
            w.body = body;
            wrappers.push(w);
        }

        // `set(options) → self`, only for a job something still calls
        // `set` on OTHER than in the chain folded above (a scheduling
        // proxy kept in a variable, say). Its value is the class object,
        // which `Ty` cannot name, so the signature says `untyped` rather
        // than claim an instance it does not return.
        if !is_worker && !class_side.contains("set") && unfolded.contains(lc.name.0.as_str()) {
            let mut body = Expr::new(span, ExprNode::SelfRef);
            body.ty = Some(Ty::Untyped);
            let mut w = perform.clone();
            w.name = Symbol::from("set");
            w.receiver = crate::dialect::MethodReceiver::Class;
            w.params = vec![crate::dialect::Param::positional(Symbol::from("options"))];
            w.signature = Some(Ty::Fn {
                params: vec![crate::ty::Param {
                    name: Symbol::from("options"),
                    ty: Ty::Untyped.into(),
                    kind: crate::ty::ParamKind::Required,
                }].into(),
                block: None,
                ret: std::sync::Arc::new(Ty::Untyped),
                effects: crate::effect::EffectSet::pure(),
            });
            w.body = body;
            wrappers.push(w);
            diags.push(residue(
                &perform,
                "set(wait:/queue:/priority:) options are dropped under inline job semantics",
            ));
        }

        if wrappers.iter().any(|w| w.name.as_str() == "perform_later") {
            payload_jobs.insert(lc.name.0.as_str().to_string());
        } else if port && !is_worker {
            // The port's `perform_later` is the port's own; its
            // `_raw_enqueue` writes the payload this plan describes.
            payload_jobs.insert(lc.name.0.as_str().to_string());
        }
        lc.methods.extend(wrappers);
    }
    app.job_plans = super::job_payload::plan_jobs(app, &jobs, &payload_jobs, &on_port);
    diags
}

/// Fold `Job.set(…).perform_later(args)` / `.perform_now(args)` to the
/// class-side call; record every `Job.set` left standing.
fn fold_set_chains(
    e: &mut Expr,
    jobs: &BTreeSet<String>,
    unfolded: &mut BTreeSet<String>,
    folded: &mut Vec<crate::span::Span>,
) {
    let job_of = |r: &Expr| -> Option<String> {
        let ExprNode::Const { path } = &*r.node else { return None };
        let name = path.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("::");
        jobs.contains(&name).then_some(name)
    };
    if let ExprNode::Send { recv: Some(inner), method, .. } = &mut *e.node {
        if matches!(method.as_str(), "perform_later" | "perform_now") {
            let job_recv = match &*inner.node {
                ExprNode::Send { recv: Some(j), method: set, block: None, .. }
                    if set.as_str() == "set" && job_of(j).is_some() =>
                {
                    Some(j.clone())
                }
                _ => None,
            };
            if let Some(j) = job_recv {
                folded.push(inner.span);
                *inner = j;
            }
        }
    }
    if let ExprNode::Send { recv: Some(r), method, .. } = &*e.node {
        if method.as_str() == "set" {
            if let Some(name) = job_of(r) {
                unfolded.insert(name);
            }
        }
    }
    e.node.for_each_child_mut(&mut |c| fold_set_chains(c, jobs, unfolded, folded));
}

/// The modules whose `include` makes a class a Sidekiq worker.
const SIDEKIQ_MODULES: &[&str] = &["Sidekiq::Worker", "Sidekiq::Job"];

/// Class-body calls a worker makes to configure the queue, which
/// inline semantics has none of.
const SIDEKIQ_CLASS_MACROS: &[&str] = &["sidekiq_options", "sidekiq_retry_in", "sidekiq_retries_exhausted"];

/// Library classes that include a Sidekiq worker module, or descend from
/// one that does (mastodon's `UpdateDistributionWorker < RawDistributionWorker`).
fn sidekiq_workers(app: &App) -> BTreeSet<String> {
    let by_name: crate::det::HashMap<&str, &crate::dialect::LibraryClass> =
        app.library_classes.iter().map(|lc| (lc.name.0.as_str(), lc)).collect();
    let mut out = BTreeSet::new();
    for lc in &app.library_classes {
        let mut cur = Some(lc);
        for _ in 0..32 {
            let Some(c) = cur else { break };
            if c.includes.iter().any(|inc| SIDEKIQ_MODULES.contains(&inc.0.as_str())) {
                out.insert(lc.name.0.as_str().to_string());
                break;
            }
            cur = c.parent.as_ref().and_then(|p| by_name.get(p.0.as_str()).copied());
        }
    }
    out
}

fn workers_with_positional_perform(app: &App, workers: &BTreeSet<String>) -> BTreeSet<String> {
    let by_name: crate::det::HashMap<&str, &crate::dialect::LibraryClass> =
        app.library_classes.iter().map(|lc| (lc.name.0.as_str(), lc)).collect();
    let mut out = BTreeSet::new();
    for name in workers {
        let mut cur = by_name.get(name.as_str()).copied();
        for _ in 0..32 {
            let Some(c) = cur else { break };
            let perform = c.methods.iter().find(|m| {
                m.receiver == crate::dialect::MethodReceiver::Instance && m.name.as_str() == "perform"
            });
            if let Some(m) = perform {
                // A rest parameter is excluded because the wrapper forwards it as one Array argument, not splatted.
                if m.block_param.is_none() && !m.params.iter().any(|p| p.forwarding || p.keyword || p.rest) {
                    out.insert(name.clone());
                }
                break;
            }
            cur = c.parent.as_ref().and_then(|p| by_name.get(p.0.as_str()).copied());
        }
    }
    out
}

/// Whether `name`'s own class chain (itself, then ancestors) defines an
/// instance `perform` at all, regardless of its parameter shape —
/// including one provided by an included module rather than the class
/// body itself. Library classes never get concern methods spliced into
/// `methods` the way models and controllers do (`ingest::app`'s
/// `splice_concerns_into_models`/`_controllers` has no library-class
/// twin), so a worker's own `perform` from a mixin is invisible unless
/// this walks `includes` too.
///
/// Tells an abstract base worker (no `perform` anywhere — never an
/// entry point) apart from one whose `perform` exists somewhere (on
/// itself, an ancestor, or a mixin) but isn't positional — that case
/// still goes through `workers_with_positional_perform` and the
/// existing "without a positional perform" refusal, exactly as before;
/// this function does not widen wrapping to mixin-provided `perform`s,
/// only widens what counts as "a perform exists".
///
/// An explicit superclass that isn't in `app.library_classes` is
/// treated the same as an unresolved include: it may provide
/// `perform`, so the worker is not stripped as abstract.
fn workers_with_any_perform(app: &App, workers: &BTreeSet<String>) -> BTreeSet<String> {
    let by_name: std::collections::HashMap<&str, &crate::dialect::LibraryClass> =
        app.library_classes.iter().map(|lc| (lc.name.0.as_str(), lc)).collect();

    fn defines_instance_perform(c: &crate::dialect::LibraryClass) -> bool {
        c.methods.iter().any(|m| {
            m.receiver == crate::dialect::MethodReceiver::Instance && m.name.as_str() == "perform"
        })
    }

    // Whether an include target provides `perform`: the Sidekiq marker
    // modules that make the class a worker in the first place never
    // do, so they're excluded up front — otherwise every worker would
    // read as "maybe performs" through its own `include Sidekiq::
    // Worker`. Any OTHER include that resolves to an ingested library
    // class answers via its own methods or (transitively) its own
    // includes. An include this app never ingested — a gem module, or
    // any other name ingest didn't resolve to a library class — is
    // unknown, so this is conservative: it may as well provide
    // `perform`, so the class takes the existing refusal path rather
    // than being silently stripped.
    fn include_provides_perform(
        id: &crate::ident::ClassId,
        by_name: &std::collections::HashMap<&str, &crate::dialect::LibraryClass>,
        seen: &mut std::collections::HashSet<crate::ident::ClassId>,
    ) -> bool {
        if SIDEKIQ_MODULES.contains(&id.0.as_str()) {
            return false;
        }
        if !seen.insert(id.clone()) {
            return false;
        }
        let Some(c) = by_name.get(id.0.as_str()).copied() else {
            return true;
        };
        defines_instance_perform(c)
            || c.includes.iter().any(|inc| include_provides_perform(inc, by_name, seen))
    }

    let mut out = BTreeSet::new();
    for name in workers {
        let mut cur = by_name.get(name.as_str()).copied();
        let mut seen = std::collections::HashSet::new();
        for _ in 0..32 {
            let Some(c) = cur else { break };
            let found = defines_instance_perform(c)
                || c.includes.iter().any(|inc| include_provides_perform(inc, &by_name, &mut seen));
            if found {
                out.insert(name.clone());
                break;
            }
            // No parent declared → end of chain (true abstract base if
            // nothing above provided perform). An explicit parent that
            // isn't ingested is unknown — same conservatism as an
            // unresolved include: count as a possible perform provider.
            let Some(parent) = c.parent.as_ref() else {
                break;
            };
            if !by_name.contains_key(parent.0.as_str()) {
                out.insert(name.clone());
                break;
            }
            cur = by_name.get(parent.0.as_str()).copied();
        }
    }
    out
}

fn without_delayed_enqueue_overrides(app: &App, wrapped: &BTreeSet<String>) -> BTreeSet<String> {
    let by_name: crate::det::HashMap<&str, &crate::dialect::LibraryClass> =
        app.library_classes.iter().map(|lc| (lc.name.0.as_str(), lc)).collect();
    wrapped
        .iter()
        .filter(|name| {
            let mut cur = by_name.get(name.as_str()).copied();
            for _ in 0..32 {
                let Some(c) = cur else { break };
                if c.methods.iter().any(|m| {
                    m.receiver == crate::dialect::MethodReceiver::Class
                        && matches!(m.name.as_str(), "perform_in" | "perform_at")
                }) {
                    return false;
                }
                cur = c.parent.as_ref().and_then(|p| by_name.get(p.0.as_str()).copied());
            }
            true
        })
        .cloned()
        .collect()
}

/// Fold `Worker.perform_in(delay, args)` / `perform_at(time, args)` to
/// `Worker.perform_async(args)`; the delay is what inline semantics drops.
fn fold_delayed_enqueues(e: &mut Expr, workers: &BTreeSet<String>, delayed: &mut Vec<crate::span::Span>) {
    if let ExprNode::Send { recv: Some(r), method, args, block: None, .. } = &mut *e.node {
        let is_worker = matches!(&*r.node, ExprNode::Const { path }
            if workers.contains(&path.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("::")));
        if is_worker && matches!(method.as_str(), "perform_in" | "perform_at") && !args.is_empty() {
            args.remove(0);
            *method = Symbol::from("perform_async");
            delayed.push(e.span);
        }
    }
    e.node.for_each_child_mut(&mut |c| fold_delayed_enqueues(c, workers, delayed));
}

fn residue(m: &crate::dialect::MethodDef, reason: &str) -> Diagnostic {
    crate::lower::residue_diagnostic(
        "job_class_side",
        "job-class-entry",
        m.body.span,
        reason,
        format!("job_class_side: `{}` — {}", m.name.as_str(), reason),
    )
}

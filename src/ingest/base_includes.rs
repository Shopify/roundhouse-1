//! The modules boot includes into ActiveRecord::Base
//! ([`App::framework_base_includes`]).
//!
//! Active Record runs `ActiveSupport.run_load_hooks(:active_record,
//! ActiveRecord::Base)` as base.rb ends, and every block registered for
//! the hook runs with `self` the base (activesupport 8.1.4
//! lazy_load_hooks.rb: `base.class_eval(&block)`). A gem's railtie uses it
//! to mix a module into every model:
//!
//! ```ruby
//! # shopify-money 4.1.1 lib/money_column/railtie.rb
//! module MoneyColumn
//!   class Railtie < Rails::Railtie
//!     ActiveSupport.on_load(:active_record) do
//!       ActiveRecord::Base.send(:include, MoneyColumn::ActiveRecordHooks)
//!     end
//!   end
//! end
//! ```
//!
//! and the module's `self.included(base)` extends the base with the class
//! methods models call in their bodies (`money_column :price,
//! currency_column: :currency`). The emitted tree runs the registration
//! as written (a railtie's body statement, a support file's or a boot
//! initializer's program) and the runtime fires the hook on its base, so
//! the include happens there too. What this pass decides is only what the
//! analysis needs to know about it: which modules every model inherits
//! that way, so a model-body call of one of their macros is judged as an
//! app macro (`lower::model_to_library::macro_replay`) rather than
//! reported as an unknown call.
//!
//! RECOGNIZED SHAPES, each with constant arguments: inside an
//! `ActiveSupport.on_load(:active_record)` block, `include M`,
//! `send(:include, M)` and the same on `self` or on `ActiveRecord::Base`;
//! anywhere in what boot runs, `ActiveRecord::Base.include(M)` and
//! `ActiveRecord::Base.send(:include, M)`; and a support file's carried
//! reopen of ActiveRecord::Base (`on_load_reopen` records a lib file's
//! hook `include` as one). A constant resolves as Ruby reads it where it
//! is written: from the railtie's namespace outwards for a railtie body,
//! from the top level for a file's or an initializer's statement. A
//! module the tree does not carry is not recorded (nothing would give its
//! macros); a computed argument is not this pass's.

use crate::app::{App, ProgramItem};
use crate::expr::{Expr, ExprNode, Literal};
use crate::ident::ClassId;

const BASE: &str = "ActiveRecord::Base";

pub(super) fn collect(app: &mut App) {
    app.framework_base_includes = gather(app, Hooks::ActiveRecord);
    app.load_hook_includes = gather(app, Hooks::Others);
}

/// Which lazy-load hooks' `include`s a pass reads: `:active_record`'s,
/// with the explicit `ActiveRecord::Base.include` shapes, or every other
/// hook's (self is the hooked base, which has no constant to name).
#[derive(Clone, Copy, PartialEq)]
enum Hooks {
    ActiveRecord,
    Others,
}

fn gather(app: &App, hooks: Hooks) -> Vec<ClassId> {
    let mut found: Vec<ClassId> = Vec::new();
    let mut record = |app: &App, scope: &str, written: &str| {
        if let Some(id) = resolve(app, scope, written) {
            if !found.contains(&id) {
                found.push(id);
            }
        }
    };
    // Gems load first (Bundler.require), their railtie bodies with them.
    for railtie in &app.railties {
        let Some(lc) = app.library_classes.iter().find(|c| c.name == railtie.name) else { continue };
        let scope = lc.name.0.as_str().to_string();
        for expr in lc.unknown_calls.iter().chain(lc.class_ivar_initializers.iter()) {
            for written in base_includes_in(expr, false, hooks) {
                record(app, &scope, &written);
            }
        }
    }
    for program in &app.file_programs {
        let mut statements = Vec::new();
        program_statements(&program.items, &mut statements);
        for expr in statements {
            for written in base_includes_in(expr, false, hooks) {
                record(app, "", &written);
            }
        }
    }
    if hooks == Hooks::ActiveRecord {
        for lc in app.library_classes.iter().filter(|c| c.name.0.as_str().trim_start_matches("::") == BASE) {
            for include in &lc.includes {
                record(app, "", include.0.as_str());
            }
        }
    }
    for program in &app.initializer_programs {
        let Some(holder) = app.library_classes.iter().find(|c| c.name == program.holder) else { continue };
        for method in &holder.methods {
            for written in base_includes_in(&method.body, false, hooks) {
                record(app, "", &written);
            }
        }
    }
    found
}

fn program_statements<'a>(items: &'a [ProgramItem], out: &mut Vec<&'a Expr>) {
    for item in items {
        match item {
            ProgramItem::Statement(expr) | ProgramItem::Guard(expr) => out.push(expr),
            ProgramItem::Branch { then_items, else_items, .. } => {
                program_statements(then_items, out);
                program_statements(else_items, out);
            }
            _ => {}
        }
    }
}

fn const_name(expr: &Expr) -> Option<String> {
    let ExprNode::Const { path } = &*expr.node else { return None };
    let rooted = path.first().is_some_and(|s| s.as_str().is_empty());
    let joined = path.iter().map(|s| s.as_str()).filter(|s| !s.is_empty()).collect::<Vec<_>>().join("::");
    Some(if rooted { format!("::{joined}") } else { joined })
}

fn is_base(expr: &Expr) -> bool {
    const_name(expr).is_some_and(|n| n.trim_start_matches("::") == BASE)
}

/// `include M, ...` / `send(:include, M, ...)`: the modules as written.
fn include_args(args: &[Expr], method: &str) -> Option<Vec<String>> {
    let modules = match method {
        "include" => args,
        "send" | "public_send" | "__send__" => match args.split_first() {
            Some((first, rest)) if matches!(&*first.node, ExprNode::Lit { value: Literal::Sym { value } } if value.as_str() == "include") => rest,
            _ => return None,
        },
        _ => return None,
    };
    if modules.is_empty() {
        return None;
    }
    modules.iter().map(const_name).collect()
}

/// The modules `expr` includes into the base, as written. `in_hook`:
/// `self` is the base (an `on_load(:active_record)` block's body).
fn base_includes_in(expr: &Expr, in_hook: bool, hooks: Hooks) -> Vec<String> {
    let mut out = Vec::new();
    walk(expr, in_hook, hooks, &mut out);
    out
}

fn walk(expr: &Expr, in_hook: bool, hooks: Hooks, out: &mut Vec<String>) {
    if let ExprNode::Send { recv, method, args, block, .. } = &*expr.node {
        let on_base = match recv {
            Some(r) => (hooks == Hooks::ActiveRecord && is_base(r)) || (in_hook && matches!(&*r.node, ExprNode::SelfRef)),
            None => in_hook,
        };
        if on_base {
            if let Some(modules) = include_args(args, method.as_str()) {
                out.extend(modules);
                return;
            }
        }
        let hook = method.as_str() == "on_load"
            && recv.as_ref().and_then(const_name).is_some_and(|n| n.trim_start_matches("::") == "ActiveSupport")
            && matches!(args.first().map(|a| &*a.node), Some(ExprNode::Lit { value: Literal::Sym { value } })
                if (value.as_str() == "active_record") == (hooks == Hooks::ActiveRecord));
        if hook {
            if let Some(ExprNode::Lambda { body, .. }) = block.as_ref().map(|b| &*b.node) {
                walk(body, true, hooks, out);
            }
            return;
        }
        // Another block runs with its own `self` or later: only an
        // explicit `ActiveRecord::Base.include` inside it counts.
        if let Some(r) = recv {
            walk(r, in_hook, hooks, out);
        }
        for arg in args {
            walk(arg, in_hook, hooks, out);
        }
        if let Some(ExprNode::Lambda { body, .. }) = block.as_ref().map(|b| &*b.node) {
            walk(body, false, hooks, out);
        }
        return;
    }
    if matches!(&*expr.node, ExprNode::Lambda { .. }) {
        expr.node.for_each_child(&mut |child| walk(child, false, hooks, out));
        return;
    }
    expr.node.for_each_child(&mut |child| walk(child, in_hook, hooks, out));
}

/// The carried module `written` names, read from `scope` outwards.
fn resolve(app: &App, scope: &str, written: &str) -> Option<ClassId> {
    let find = |name: &str| app.library_classes.iter().find(|c| c.is_module && c.name.0.as_str() == name).map(|c| c.name.clone());
    if let Some(rooted) = written.strip_prefix("::") {
        return find(rooted);
    }
    let segments: Vec<&str> = if scope.is_empty() { Vec::new() } else { scope.split("::").collect() };
    (0..=segments.len()).rev().find_map(|n| {
        if n == 0 {
            find(written)
        } else {
            find(&format!("{}::{written}", segments[..n].join("::")))
        }
    })
}

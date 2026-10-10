//! `ActiveSupport.on_load(:hook) do … end` at the top of a `lib/` file
//! — a class reopen deferred until the framework class it reopens has
//! loaded. The class finder skips blocks, so nothing inside one is a
//! library class, and until this pass the whole file was DROPPED
//! SILENTLY: campfire's `lib/rails_ext/action_text_attachables.rb`
//! reopens `ActionText::Attachment.from_node` inside one, and the tree
//! neither carried it nor said so.
//!
//! WHAT IS CARRIED, AND HOW. That reopen exists so rotating
//! `SECRET_KEY_BASE` does not orphan every @mention: an sgid whose
//! signature fails is decoded by hand (Rails' `_rails.data` envelope,
//! and the Rails-7 Marshal shape behind it) and, for the models a
//! `%w[…]` constant names, the record is found anyway. The body is
//! thirty lines of `JSON.parse(…).dig`, a `rescue`-chained base64
//! fallback, a regex over Marshal bytes and a `GlobalID.find` on an
//! untyped string — every one a typing fight — for a method whose
//! whole CONTENT is that list. So the pass reads the shape and bakes
//! the fact: the list lands on `App::attachable_unsigned_models`,
//! `project::apply_attachable_locate` writes it into the emitted
//! `global_id_locator.rb` as `Attachment.permitted_without_signature`,
//! and the runtime's `SignedGlobalId.unverified_uri` is the decode,
//! transcribed once (`runtime/ruby/action_text.rb`). The same rule
//! `lower::attachable` applies to the mint: the compile-time fact is
//! the model name, not the code that reads it off `self.class`.
//!
//! The FINGERPRINT, all three required: the reopened class is
//! `ActionText::Attachment`, it defines `from_node` on its singleton,
//! and its body names the `"_rails"` envelope key — that string is the
//! hand-decode, and a `from_node` reopen without it is doing something
//! else. The list is the one constant in the body whose value is an
//! array of string literals; two such constants, or none, is a shape
//! this pass does not know.
//!
//! Anything else inside an `on_load` block — a reopen of another
//! class, a `from_node` reopen without the fingerprint, a module — is
//! reported as a survey line naming the hook and the class, rather
//! than dropped in silence. That is the contract the route ingest
//! took for `to: redirect(…)`: a hole nobody can see is how a gap
//! stays open.
//!
//! Direct receiverless `include` calls with literal module names are carried
//! for known framework hooks. Other include shapes in those hooks are
//! reported without executing the hook. A hook with no class to reopen,
//! whose block declares nothing, is not this pass's: the file program
//! registers it as written ([`registered_as_written`]). This is not an exhaustive ledger of hook bodies;
//! nested/conditional calls and other executable statements remain gaps.

use ruby_prism::Node;

use crate::app::App;
use crate::ident::{ClassId, Symbol};

use super::library_class::library_class_from_module_node_with_scope;
use super::util::{
    class_name_path, constant_id_str, constant_path_of, find_all_classes_with_scope,
    find_all_modules_with_scope, flatten_statements,
};
use super::{survey, IngestError};

/// The reopened class the tolerant-sgid shape lives on.
const ATTACHMENT: &str = "ActionText::Attachment";
/// The envelope key the hand-decode reads; its presence is what says
/// the reopen is the rotated-secret tolerance and not another
/// `from_node`.
const ENVELOPE_KEY: &str = "_rails";

/// Scan one file's top-level `ActiveSupport.on_load` blocks. Records
/// the tolerant-sgid list on `app` when the fingerprint matches, and a
/// survey line for other reopens and direct includes inside such a block.
pub(super) fn ingest_on_load_reopens(source: &[u8], file: &str, app: &mut App) {
    let result = super::prism::parse(source, file);
    let root = result.node();
    let src = String::from_utf8_lossy(source);
    let Some(prog) = root.as_program_node() else { return };
    for stmt in prog.statements().body().iter() {
        let Some(call) = stmt.as_call_node() else { continue };
        if constant_id_str(&call.name()) != "on_load" {
            continue;
        }
        let Some(recv) = call.receiver() else { continue };
        if constant_path_of(&recv).map(|p| p.join("::")).as_deref() != Some("ActiveSupport") {
            continue;
        }
        if registered_as_written(&call) {
            continue;
        }
        let hook = call
            .arguments()
            .and_then(|args| args.arguments().iter().next())
            .and_then(|a| a.as_symbol_node().map(|s| String::from_utf8_lossy(s.unescaped()).into_owned()))
            .unwrap_or_else(|| "?".to_string());
        let Some(body) = call.block().and_then(|b| b.as_block_node()).and_then(|b| b.body()) else {
            continue;
        };

        if let Some(statements) = body.as_statements_node() {
            for stmt in statements.body().iter() {
                let Some(include) = stmt.as_call_node() else { continue };
                if include.receiver().is_some() || constant_id_str(&include.name()) != "include" {
                    continue;
                }
                // Class-method macros are indexed independently of mixin carrying
                // (`App.load_hook_class_macros`), including carried literal hooks.
                if hook == "active_record" {
                    for arg in include.arguments().iter().flat_map(|a| a.arguments().iter()) {
                        if let Some(path) = constant_path_of(&arg) {
                            let id = ClassId(Symbol::from(path.join("::")));
                            if !app.load_hook_class_macros.contains(&id) {
                                app.load_hook_class_macros.push(id);
                            }
                        }
                    }
                }
                let carried = hook_class(&hook).is_some()
                    && include.arguments().is_some_and(|args| {
                        args.arguments().iter().next().is_some()
                            && args.arguments().iter().all(|arg| constant_path_of(&arg).is_some())
                    });
                if carried {
                    continue;
                }
                let loc = include.location();
                let declaration = String::from_utf8_lossy(loc.as_slice());
                survey::record(&IngestError::Unsupported {
                    file: file.into(),
                    message: format!(
                        "`ActiveSupport.on_load(:{hook})` contains `{declaration}`, which is not carried: \
                         load-hook mixin installation is unsupported"
                    ),
                });

            }
        }

        for (scope, class) in find_all_classes_with_scope(&body) {
            let mut path = scope.clone();
            path.extend(class_name_path(&class).unwrap_or_default());
            let name = path.join("::");
            if name == ATTACHMENT {
                if let Some(models) = tolerant_sgid_models(&class, &src) {
                    app.attachable_unsigned_models = models;
                    continue;
                }
            }
            // `on_load(:active_storage_attachment) { class Markdown;
            // include Uploads; end }` — a reopen that only `include`s
            // literal modules. Queue (or push) so the concern splice
            // applies `has_many_attached`. File order can visit this
            // hook before the model exists; [`apply_pending`] runs
            // before the splice. Other on_load reopens stay surveyed.
            if hook == "active_storage_attachment" {
                if let Some(includes) = includes_only_reopen(&class) {
                    let mods: Vec<Symbol> = includes.iter().map(|p| Symbol::from(p.as_str())).collect();
                    if let Some(model) = app.models.iter_mut().find(|m| m.name.0.as_str() == name) {
                        for path in &includes {
                            push_model_include(model, path);
                        }
                    } else {
                        app.pending_attachment_on_load
                            .push((Symbol::from(name.as_str()), mods));
                    }
                    continue;
                }
            }
            not_carried(file, &hook, &name);
        }
        for (scope, module) in find_all_modules_with_scope(&body) {
            // A module DEFINED in the block is an ordinary top-level
            // module (`module` does not close over the block's self):
            // carry it as the library class it would be outside the
            // block, so what the hook includes has methods to resolve.
            match library_class_from_module_node_with_scope(&module, &scope, file) {
                Ok(lc) => app.library_classes.push(lc),
                Err(err) => survey::record(&err),
            }
        }
        carry_hook_includes(body, &hook, file, app);
    }
}

/// A file's top-level `ActiveSupport.on_load(:hook) do … end` that its
/// file program carries as the statement it is
/// (`library_class::program_items`). Ruby registers the block with
/// lazy_load_hooks when the file loads and runs it only when something
/// fires `:hook` (activesupport 8.1.4 lazy_load_hooks.rb), so the
/// emitted file registers it the same way, with the runtime's port of
/// lazy_load_hooks; a tree without the port refuses it
/// (`project::reject_unported_load_hooks`). Shopify Core's
/// granular_exceptions.rb mixes its Trilogy translations in this way, on
/// a hook only the MySQL adapter fires.
///
/// Not this shape, and read by this pass instead: a hook it carries as a
/// reopen of the hooked class ([`hook_class`]), and a block that
/// declares a class or module (a reopen, or a module defined in it).
///
/// A test-case hook ([`TEST_LOAD_HOOKS`]) on a PostgreSQL tree is this
/// shape even where [`hook_class`] names its base (Core's
/// essentials/lib/patches/action_dispatch_url_for_param_filter.rb:81
/// `on_load(:action_controller_test_case) { include
/// TestParametersReservedParams }`): the emitted test helper fires those
/// hooks where Rails does (`project::test_case_load_hooks_block`), so
/// the lib file's hook runs there, in registration order with the
/// initializers' and engines' hooks for the same base, not as a reopen
/// at boot ahead of them. Other trees, which have no lazy_load_hooks
/// port, keep the reopen.
///
/// So is a hook of a hooked class on a PostgreSQL tree whose block only
/// `prepend`s literal modules on that class (Core's
/// essentials/lib/patches/active_record/attributes_coercion.rb:18
/// `on_load(:active_record) { prepend ActiveRecordAttributesCoercion }`):
/// the runtime's lazy_load_hooks runs it on the base, where before it
/// was dropped in silence and Base#init_with never saw the coercion.
///
/// [`TEST_LOAD_HOOKS`]: super::initializer_programs::TEST_LOAD_HOOKS
pub(super) fn registered_as_written(call: &ruby_prism::CallNode<'_>) -> bool {
    if constant_id_str(&call.name()) != "on_load"
        || call.receiver().and_then(|r| constant_path_of(&r)).map(|p| p.join("::")).as_deref() != Some("ActiveSupport")
    {
        return false;
    }
    let args: Vec<Node<'_>> = call.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default();
    let [hook] = args.as_slice() else { return false };
    let Some(hook) = hook.as_symbol_node().map(|s| String::from_utf8_lossy(s.unescaped()).into_owned()) else {
        return false;
    };
    let Some(block) = call.block().and_then(|b| b.as_block_node()) else { return false };
    if hook_base_reopen(call).is_some() {
        return true;
    }
    if hook_class(&hook).is_some() && !deferred_test_hook(&hook) && !(postgresql_tree() && prepends_only(&block)) {
        return false;
    }
    block.body().is_none_or(|body| {
        find_all_classes_with_scope(&body).is_empty() && find_all_modules_with_scope(&body).is_empty()
    })
}

/// A hook of a PostgreSQL tree whose block is only a reopen of the
/// class the hook hands it, through its enclosing modules (Core's
/// essentials/lib/patches/action_dispatch_request_buyer_session.rb:
/// `on_load(:action_controller_test_case) { module ActionController;
/// class TestCase; def buyer_session … end; end; end }`): the hook's
/// name and that class's node. Rails runs the block under
/// `base.class_eval` once the base loads, and the reopen opens that same
/// base, so its body is the hook's body: the file program registers the
/// hook with the body written straight into the block
/// (`library_class::file_program`), which the runtime's lazy_load_hooks
/// runs on the base where Rails does. A test-case base is defined by
/// the emitted test helper, after boot, so a reopen carried at boot
/// would open the class too early; before this, the whole block was
/// dropped and `buyer_session` was undefined in every controller test.
///
/// Not this shape: a superclass on the reopen, a body that writes a
/// constant (Ruby writes it on the base, the block on the file's scope),
/// anything beside the reopen in the block or its modules, a block with
/// parameters, a hook [`hook_class`] does not name.
pub(super) fn hook_base_reopen<'pr>(call: &ruby_prism::CallNode<'pr>) -> Option<(String, ruby_prism::ClassNode<'pr>)> {
    if !postgresql_tree()
        || constant_id_str(&call.name()) != "on_load"
        || call.receiver().and_then(|r| constant_path_of(&r)).map(|p| p.join("::")).as_deref() != Some("ActiveSupport")
    {
        return None;
    }
    let args: Vec<Node<'pr>> = call.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default();
    let [hook] = args.as_slice() else { return None };
    let hook = hook.as_symbol_node().map(|s| String::from_utf8_lossy(s.unescaped()).into_owned())?;
    let base = hook_class(&hook)?;
    let block = call.block()?.as_block_node()?;
    if block.parameters().is_some() {
        return None;
    }
    let mut path: Vec<String> = Vec::new();
    let mut node = block.body()?;
    loop {
        let [only] = <[Node<'pr>; 1]>::try_from(flatten_statements(node)).ok()?;
        if let Some(module) = only.as_module_node() {
            path.extend(super::util::module_name_path(&module)?);
            node = module.body()?;
            continue;
        }
        let class = only.as_class_node()?;
        path.extend(class_name_path(&class)?);
        if path.join("::") != base || class.superclass().is_some() {
            return None;
        }
        if let Some(body) = class.body() {
            body.as_statements_node()?;
            if flatten_statements(body).iter().any(|s| s.as_constant_write_node().is_some() || s.as_constant_path_write_node().is_some()) {
                return None;
            }
        }
        return Some((hook, class));
    }
}

thread_local! {
    /// Set for the ingest of a PostgreSQL tree ([`defer_test_hooks`]).
    static DEFER_TEST_HOOKS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Restores the previous [`defer_test_hooks`] setting when dropped.
pub(super) struct DeferTestHooks(bool);

impl Drop for DeferTestHooks {
    fn drop(&mut self) {
        DEFER_TEST_HOOKS.with(|cell| cell.set(self.0));
    }
}

/// For the ingest of one tree: whether its test-case hooks are
/// registered as written ([`registered_as_written`]), which a PostgreSQL
/// tree's are.
pub(super) fn defer_test_hooks(postgresql: bool) -> DeferTestHooks {
    DeferTestHooks(DEFER_TEST_HOOKS.with(|cell| cell.replace(postgresql)))
}

/// A test-case hook the ingest registers as written rather than reopening
/// its base ([`registered_as_written`]).
fn deferred_test_hook(hook: &str) -> bool {
    DEFER_TEST_HOOKS.with(|cell| cell.get())
        && super::initializer_programs::TEST_LOAD_HOOKS.iter().any(|(name, _, _)| *name == hook)
}

/// Whether this ingest is of a PostgreSQL tree, whose runtime ports
/// lazy_load_hooks ([`defer_test_hooks`]).
fn postgresql_tree() -> bool {
    DEFER_TEST_HOOKS.with(|cell| cell.get())
}

/// A load hook's block whose statements are all `prepend Mod` calls with
/// literal module paths, on the hooked class (receiverless) or on a
/// literal class path (`ActiveRecord::Reflection::AssociationReflection
/// .prepend(Mod)`, which the block runs the same wherever it is).
fn prepends_only(block: &ruby_prism::BlockNode<'_>) -> bool {
    let Some(body) = block.body() else { return false };
    let stmts = flatten_statements(body);
    !stmts.is_empty()
        && stmts.iter().all(|stmt| {
            stmt.as_call_node().is_some_and(|call| {
                call.receiver().is_none_or(|recv| constant_path_of(&recv).is_some())
                    && constant_id_str(&call.name()) == "prepend"
                    && call.block().is_none()
                    && call.arguments().is_some_and(|args| {
                        args.arguments().iter().next().is_some()
                            && args.arguments().iter().all(|arg| constant_path_of(&arg).is_some())
                    })
            })
        })
}

/// The framework class a load hook hands its block, for the hooks whose
/// receiver is one class. Deliberately a short list: a hook this does not
/// name is reported rather than guessed at.
fn hook_class(hook: &str) -> Option<&'static str> {
    Some(match hook {
        "action_dispatch_request" => "ActionDispatch::Request",
        "action_dispatch_integration_test" => "ActionDispatch::IntegrationTest",
        "action_controller" | "action_controller_base" => "ActionController::Base",
        "action_controller_test_case" => "ActionController::TestCase",
        "action_view" => "ActionView::Base",
        "action_mailer" => "ActionMailer::Base",
        "active_job" => "ActiveJob::Base",
        "active_record" => "ActiveRecord::Base",
        _ => return None,
    })
}

/// `include(X)` / `include X` written straight in a load hook's block
/// runs against the hooked class: the same mixin as
/// `class ActionDispatch::Request; include(X); end`, and recorded the
/// same way, as a reopen of that class.
fn carry_hook_includes(body: Node<'_>, hook: &str, file: &str, app: &mut App) {
    let mut mixins: Vec<String> = Vec::new();
    for stmt in flatten_statements(body) {
        let Some(call) = stmt.as_call_node() else { continue };
        if call.receiver().is_some() || constant_id_str(&call.name()) != "include" {
            continue;
        }
        let Some(args) = call.arguments() else { continue };
        mixins.extend(args.arguments().iter().filter_map(|a| constant_path_of(&a)).map(|p| p.join("::")));
    }
    if mixins.is_empty() {
        return;
    }
    // Unsupported direct includes were reported at their original byte spans.
    let Some(class) = hook_class(hook) else { return };
    let source = format!("class {class}\n  include {}\nend\n", mixins.join(", "));
    match super::library_class::ingest_library_classes(source.as_bytes(), file) {
        Ok(classes) => app.library_classes.extend(classes),
        Err(err) => survey::record(&err),
    }
}

fn not_carried(file: &str, hook: &str, name: &str) {
    survey::record(&IngestError::Unsupported {
        file: file.into(),
        message: format!(
            "`ActiveSupport.on_load(:{hook})` reopens `{name}`, which is not carried: a reopen \
             of a framework class inside a load hook is dropped (the tolerant-sgid \
             `ActionText::Attachment.from_node` shape is the one read)"
        ),
    });
}

/// When `class` body's only statements are receiverless `include Mod`
/// calls with literal module constant paths, return those paths.
/// Anything else (methods, constants, nested classes, non-literal
/// includes) means this is not the carried shape.
fn includes_only_reopen(class: &ruby_prism::ClassNode<'_>) -> Option<Vec<String>> {
    let body = class.body()?;
    let stmts = flatten_statements(body);
    if stmts.is_empty() {
        return None;
    }
    let mut paths = Vec::new();
    for stmt in stmts {
        let call = stmt.as_call_node()?;
        if call.receiver().is_some() || constant_id_str(&call.name()) != "include" {
            return None;
        }
        let args = call.arguments()?;
        let mut any = false;
        for arg in args.arguments().iter() {
            let path = constant_path_of(&arg)?;
            paths.push(path.join("::"));
            any = true;
        }
        if !any {
            return None;
        }
    }
    Some(paths)
}

/// Apply queued `active_storage_attachment` includes onto models that
/// were not yet in `app.models` when the load hook was scanned.
/// Leftovers (no matching model) are surveyed like other uncarried
/// reopens.
///
/// Returns the names of models that received a fresh `include`, so a
/// later concern splice can target only those — a full re-splice would
/// duplicate every previously expanded concern body.
pub(super) fn apply_pending(app: &mut App) -> Vec<Symbol> {
    let pending = std::mem::take(&mut app.pending_attachment_on_load);
    let mut still = Vec::new();
    let mut touched = Vec::new();
    for (model_name, mods) in pending {
        if let Some(model) = app.models.iter_mut().find(|m| m.name.0 == model_name) {
            for path in &mods {
                push_model_include(model, path.as_str());
            }
            touched.push(model_name);
        } else {
            // Model may appear later via synthesis (`has_markdown` /
            // Attachment table). Keep the queue for a second apply
            // after synthesize; [`drain_pending`] surveys leftovers.
            still.push((model_name, mods));
        }
    }
    app.pending_attachment_on_load = still;
    touched
}

/// Survey any `on_load(:active_storage_attachment)` includes that
/// never found a model — call after the last synthesize pass.
pub(super) fn drain_pending(app: &mut App) {
    let pending = std::mem::take(&mut app.pending_attachment_on_load);
    for (model_name, _) in pending {
        not_carried(
            "<on_load>",
            "active_storage_attachment",
            model_name.as_str(),
        );
    }
}

/// Push `include Mod` as a model-body Unknown so
/// `splice_concerns_into_models` can expand the concern's
/// `included do` macros onto the model.
fn push_model_include(model: &mut crate::dialect::Model, module_path: &str) {
    use crate::dialect::ModelBodyItem;
    use crate::expr::{Expr, ExprNode};
    use crate::span::Span;

    let path: Vec<Symbol> = module_path.split("::").map(Symbol::from).collect();
    let expr = Expr::new(
        Span::synthetic(),
        ExprNode::Send {
            recv: None,
            method: Symbol::from("include"),
            args: vec![Expr::new(Span::synthetic(), ExprNode::Const { path })],
            block: None,
            parenthesized: false,
        },
    );
    model.body.push(ModelBodyItem::Unknown {
        expr,
        leading_comments: Vec::new(),
        leading_blank_line: false,
    });
}

/// The `%w[…]` of model names when `class` is the tolerant `from_node`
/// reopen, else None.
fn tolerant_sgid_models(class: &ruby_prism::ClassNode<'_>, src: &str) -> Option<Vec<Symbol>> {
    let body = class.body()?;
    let loc = body.location();
    let text = &src[loc.start_offset()..loc.end_offset()];
    if !text.contains(&format!("\"{ENVELOPE_KEY}\"")) {
        return None;
    }
    let mut has_from_node = false;
    let mut lists: Vec<Vec<Symbol>> = Vec::new();
    collect(&body, false, &mut has_from_node, &mut lists);
    if !has_from_node {
        return None;
    }
    match lists.as_slice() {
        [one] => Some(one.clone()),
        _ => None,
    }
}

/// Walk the class body through `class << self`, noting a class-side
/// `from_node` (`def self.from_node`, or a bare `def` inside the
/// singleton block) and every constant whose value is an array of
/// string literals.
fn collect(node: &Node<'_>, class_side: bool, has_from_node: &mut bool, lists: &mut Vec<Vec<Symbol>>) {
    if let Some(s) = node.as_statements_node() {
        for stmt in s.body().iter() {
            collect(&stmt, class_side, has_from_node, lists);
        }
        return;
    }
    if let Some(sc) = node.as_singleton_class_node() {
        if let Some(b) = sc.body() {
            collect(&b, true, has_from_node, lists);
        }
        return;
    }
    if let Some(def) = node.as_def_node() {
        if constant_id_str(&def.name()) == "from_node" && (class_side || def.receiver().is_some()) {
            *has_from_node = true;
        }
        return;
    }
    if let Some(cw) = node.as_constant_write_node() {
        if let Some(arr) = cw.value().as_array_node() {
            let mut out = Vec::new();
            for el in arr.elements().iter() {
                let Some(s) = el.as_string_node() else { return };
                out.push(Symbol::from(String::from_utf8_lossy(s.unescaped()).as_ref()));
            }
            lists.push(out);
        }
    }
}

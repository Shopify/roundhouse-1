//! A model extension a boot initializer mixes in through the model's own
//! load hook:
//!
//! ```ruby
//! # components/shop_identity/config/initializers/model_extensions.rb
//! ActiveSupport.on_load(:shop) do
//!   class Shop
//!     include Extension
//!   end
//! end
//!
//! # components/platform/essentials/app/models/shop.rb, end of the body
//! ActiveSupport.run_load_hooks(:shop)
//! ```
//!
//! Rails registers the block when the initializer runs, and
//! `run_load_hooks(:shop)` runs it when Shop's body reaches the call
//! (activesupport 8.1.4 lazy_load_hooks.rb: `base.class_eval(&block)`,
//! base `Object` when none is passed). The `class Shop` reopen in the
//! block is the top-level Shop, so the include lands on Shop right
//! there. Core splits Shop, ApiClient, User and its subclasses, Feature,
//! ShopRoute, ShopBillingSetting and Employee this way; without the
//! include none of their extension methods or associations existed.
//!
//! The pass moves each such `include` into the model body at the
//! `run_load_hooks` site, where the concern splice and the emit treat it
//! like one the model wrote. The `on_load` statement is then consumed.
//! It lifts only when all of these hold:
//!
//! * the initializer is one the boot runs, at its top level;
//! * one class body fires the hook (`initializer_programs::
//!   class_load_hooks`), and that class is a model, or a class the
//!   ingest keeps as a library class (an STI subclass, `RegularUser <
//!   User`), whose include list takes them;
//! * every statement of the block is a `class <Model>` reopen without a
//!   superclass whose body is only `include` calls with constant
//!   arguments, or such an `include` itself (below).
//!
//! A constant in the reopen is looked up from `<Model>` outward, as one
//! written in the model body is, so the lifted include is qualified the
//! same way (`qualify_relative_includes`). A bare `include M` in the
//! block (`run_load_hooks(:h, self)` evaluates it in the class) also
//! lifts, but its constants resolve from the initializer's top level, not
//! from the class: only a path that names a known module as written, and
//! that no scope of the class also defines, reads the same in the class
//! body (`include ShopBillingSetting::Extension`; a bare `include
//! Extension` there stays). Any shape not lifted is left as it was: the
//! initializer scan runs what it can and names the rest at its site
//! (`initializer_unsupported`).
//!
//! A controller fires a hook the same way (Core's `AdminAreaController`
//! ends with `ActiveSupport.run_load_hooks(:admin_area_controller, self)`
//! and access_and_auth's initializer includes `IdentityAwareController`
//! there). Its block is bare `include M` calls, lifted to the end of the
//! controller body as the class's own includes, and the controller's
//! lowering writes them (`controller_concern_blocks` replays each
//! concern's `ClassMethods` and `included` macros after them). The block
//! may also `require "x"`: a controller body has nowhere to write it, so
//! the block stays whole at its hook, and only the includes before the
//! first `require` are lifted (the hook's own `include` of a module the
//! class already has is a no-op).

use std::path::{Path, PathBuf};

use ruby_prism::{CallNode, Node};

use crate::app::{App, ClassLoadHook};
use crate::dialect::{ControllerBodyItem, ModelBodyItem};
use crate::span::SourceFile;
use crate::vfs::Vfs;

use super::util::{class_name_path, constant_id_str, constant_path_of, flatten_statements};

pub(super) fn lift<V: Vfs + ?Sized>(
    app: &mut App,
    vfs: &V,
    dir: &Path,
    sources: &[SourceFile],
    boot_initializers: &[PathBuf],
) {
    let (sites, _) = super::initializer_programs::class_load_hooks(
        sources,
        dir,
        &app.initializer_files,
        &super::initializer_programs::known_constants(app),
    );
    let mut lifted_from: crate::det::HashSet<crate::span::FileId> = crate::det::HashSet::default();
    // What `qualify_relative_includes` resolves against.
    let known: Known =
        app.library_classes.iter().map(|lc| lc.name.clone()).chain(app.concern_model_items.keys().cloned()).collect();
    for entry in boot_initializers {
        let Ok(bytes) = vfs.read(entry) else { continue };
        if !bytes.windows(7).any(|w| w == b"on_load") {
            continue;
        }
        let file = entry.display().to_string();
        let result = ruby_prism::parse(&bytes);
        let root = result.node();
        let Some(program) = root.as_program_node() else { continue };
        for stmt in program.statements().body().iter() {
            let Some((hook, body)) = on_load_block(&stmt) else { continue };
            let mut fired = sites.iter().filter(|site| site.hook == hook);
            let (Some(site), None) = (fired.next(), fired.next()) else { continue };
            let is_controller = app.controllers.iter().any(|c| c.name.0.as_str() == site.class);
            let Some(includes) = literal_includes(&body, site, &known, is_controller) else { continue };
            if is_controller && !app.models.iter().any(|m| m.name.0.as_str() == site.class) {
                // A controller body holds no `require` the lowering would
                // write, so a block with one stays whole and keeps running
                // at the hook, `require` included; the includes before the
                // first `require` are lifted too (the hook's own `include`
                // of a module already included is a no-op). Those after it
                // name what the `require` loads and wait for the hook.
                let has_require = includes.iter().any(|call| constant_id_str(&call.name()) == "require");
                let lifted: Vec<&CallNode> =
                    includes.iter().take_while(|call| constant_id_str(&call.name()) != "require").collect();
                if lifted.is_empty() {
                    continue;
                }
                let Ok(exprs) =
                    lifted.iter().map(|call| super::expr::ingest_expr(&call.as_node(), &file)).collect::<Result<Vec<_>, _>>()
                else {
                    continue;
                };
                // The controller body's end: the hook fires from the last
                // statement of the class body, after an earlier hook
                // block's lifted statements.
                let controller = app.controllers.iter_mut().find(|c| c.name.0.as_str() == site.class).expect("checked above");
                controller.body.extend(exprs.into_iter().map(|expr| ControllerBodyItem::Unknown {
                    expr,
                    leading_comments: Vec::new(),
                    leading_blank_line: false,
                }));
                if !has_require {
                    super::initializers::consume_node(&file, &stmt);
                }
                continue;
            }
            let Some(model) = app.models.iter_mut().find(|m| m.name.0.as_str() == site.class) else {
                // An STI subclass of a model (`RegularUser < User`) is a
                // library class here; its includes are a list.
                let Some(class) =
                    app.library_classes.iter_mut().find(|lc| !lc.is_module && lc.name.0.as_str() == site.class)
                else {
                    continue;
                };
                for call in &includes {
                    for arg in call.arguments().into_iter().flat_map(|args| args.arguments().iter()) {
                        let Some(path) = constant_path_of(&arg) else { continue };
                        let id = crate::ident::ClassId(crate::ident::Symbol::from(path.join("::").as_str()));
                        if !class.includes.contains(&id) {
                            class.includes.push(id);
                        }
                    }
                }
                super::initializers::consume_node(&file, &stmt);
                continue;
            };
            let Ok(exprs) = includes.iter().map(|call| super::expr::ingest_expr(&call.as_node(), &file)).collect::<Result<Vec<_>, _>>() else {
                continue;
            };
            let mut at = model
                .body
                .iter()
                .rposition(|item| item.span().file == site.span.file && item.span().start < site.span.start)
                .map_or(0, |i| i + 1);
            // After what an earlier hook block lifted to the same site:
            // Rails runs the blocks in registration order.
            while model.body.get(at).is_some_and(|item| lifted_from.contains(&item.span().file)) {
                at += 1;
            }
            lifted_from.insert(super::sources::file_id(&file));
            let items = exprs
                .into_iter()
                .map(|expr| ModelBodyItem::Unknown { expr, leading_comments: Vec::new(), leading_blank_line: false });
            model.body.splice(at..at, items);
            super::initializers::consume_node(&file, &stmt);
        }
    }
}

/// `ActiveSupport.on_load(:hook) do … end` (a Symbol or String name, no
/// options): the hook name and the block's statements.
fn on_load_block<'pr>(stmt: &Node<'pr>) -> Option<(String, Vec<Node<'pr>>)> {
    let call = stmt.as_call_node()?;
    if constant_id_str(&call.name()) != "on_load" || constant_path_of(&call.receiver()?)?.join("::") != "ActiveSupport" {
        return None;
    }
    let args: Vec<Node<'pr>> = call.arguments()?.arguments().iter().collect();
    let [name] = args.as_slice() else { return None };
    let hook = if let Some(sym) = name.as_symbol_node() {
        String::from_utf8_lossy(sym.unescaped()).into_owned()
    } else {
        String::from_utf8_lossy(name.as_string_node()?.unescaped()).into_owned()
    };
    let block = call.block()?.as_block_node()?;
    let body = block.body().map(flatten_statements).unwrap_or_default();
    Some((hook, body))
}

/// The include calls of a block that holds only `class <Model>` reopens
/// of the class firing the hook, each holding only includes, and, when
/// the hook's base is that class, bare includes naming modules only the
/// top level resolves; `None` for any other block.
fn literal_includes<'pr>(
    body: &[Node<'pr>],
    site: &ClassLoadHook,
    known: &Known,
    requires: bool,
) -> Option<Vec<CallNode<'pr>>> {
    let mut out = Vec::new();
    for stmt in body {
        if let Some(class) = stmt.as_class_node() {
            if class.superclass().is_some() || class_name_path(&class)?.join("::") != site.class {
                return None;
            }
            for inner in class.body().map(flatten_statements).unwrap_or_default() {
                out.push(include_call(&inner)?);
            }
        } else if site.base == site.class {
            if requires && let Some(call) = literal_require(stmt) {
                out.push(call);
                continue;
            }
            let call = include_call(stmt)?;
            let args = call.arguments()?.arguments();
            if !args.iter().all(|arg| constant_path_of(&arg).is_some_and(|path| top_level_only(&path, &site.class, known))) {
                return None;
            }
            out.push(call);
        } else {
            return None;
        }
    }
    (!out.is_empty()).then_some(out)
}

type Known = crate::det::HashSet<crate::ident::ClassId>;

/// `path`, written in the block, names a known module from the top level
/// (where the block's constants resolve), and no scope of `class` defines
/// it, so `qualify_relative_includes` reads it the same in the class body.
fn top_level_only(path: &[String], class: &str, known: &Known) -> bool {
    let tail = path.join("::");
    let id = |name: &str| crate::ident::ClassId(crate::ident::Symbol::from(name));
    let scope: Vec<&str> = class.split("::").collect();
    known.contains(&id(&tail))
        && (1..=scope.len()).all(|depth| !known.contains(&id(&format!("{}::{tail}", scope[..depth].join("::")))))
}

/// `require "path"` with one string literal: a controller block's
/// `require "trust_battery/challenge_required_response_handling"` before
/// the include that needs it keeps its place among the lifted includes.
fn literal_require<'pr>(stmt: &Node<'pr>) -> Option<CallNode<'pr>> {
    let call = stmt.as_call_node()?;
    if call.receiver().is_some() || call.block().is_some() || constant_id_str(&call.name()) != "require" {
        return None;
    }
    let args: Vec<Node<'pr>> = call.arguments()?.arguments().iter().collect();
    matches!(args.as_slice(), [arg] if arg.as_string_node().is_some()).then_some(call)
}

/// `include M` / `include M, N` with constant arguments.
fn include_call<'pr>(stmt: &Node<'pr>) -> Option<CallNode<'pr>> {
    let call = stmt.as_call_node()?;
    if call.receiver().is_some() || call.block().is_some() || constant_id_str(&call.name()) != "include" {
        return None;
    }
    let args: Vec<Node<'pr>> = call.arguments()?.arguments().iter().collect();
    (!args.is_empty() && args.iter().all(|arg| constant_path_of(arg).is_some())).then_some(call)
}

/// A boot initializer's include into a model through the main
/// autoloader's load callback:
///
/// ```ruby
/// # components/shop_identity/config/initializers/application_record.rb
/// Rails.autoloaders.main.on_load("ApplicationRecord") do
///   ApplicationRecord.include(BelongsToShopThroughAssociation)
/// end
/// ```
///
/// Zeitwerk 2.7 runs an `on_load(cpath)` callback right after the file
/// that defines the constant is autoloaded (`Loader::Callbacks#
/// on_file_autoloaded` → `run_on_load_callbacks(cref.path, cref.get,
/// file)`), so the include lands on ApplicationRecord after its body and
/// before any subclass body runs: a subclass's `class X <
/// ApplicationRecord` is what autoloads it. The pass appends the include
/// to the model's body after the items of the file that declares it,
/// where the concern splice and the emit treat it like one written last
/// in the body. `initializer_programs` carries the statement as
/// `Rails.autoloaders.main.on_load_lifted(cpath, site)`, which registers
/// nothing and names a constant already loaded when it runs, which
/// Zeitwerk would not call the callback for
/// (runtime/spinel/rails_autoloaders.rb).
///
/// It lifts only when the initializer is one the boot runs, the callback
/// names a model by a literal path, and every statement of the block is
/// `<Cpath>.include(M, …)` or `.send(:include, M, …)` (or the same on the
/// block's first parameter, the constant's value) whose constants read
/// the same from the initializer's top level as from the class body
/// (`top_level_only`). Anything else is left to `initializer_programs`,
/// which registers the block with the ported loader.
pub(super) fn lift_autoload_includes<V: Vfs + ?Sized>(app: &mut App, vfs: &V, boot_initializers: &[PathBuf]) {
    let known: Known =
        app.library_classes.iter().map(|lc| lc.name.clone()).chain(app.concern_model_items.keys().cloned()).collect();
    // How many includes each model got so far: Zeitwerk runs a
    // constant's callbacks in registration order.
    let mut appended: crate::det::HashMap<String, usize> = crate::det::HashMap::default();
    for entry in boot_initializers {
        let Ok(bytes) = vfs.read(entry) else { continue };
        if !bytes.windows(11).any(|w| w == b"autoloaders") {
            continue;
        }
        let file = entry.display().to_string();
        let result = ruby_prism::parse(&bytes);
        let root = result.node();
        let Some(program) = root.as_program_node() else { continue };
        for stmt in program.statements().body().iter() {
            let Some((cpath, param, body)) = autoload_on_load_block(&stmt) else { continue };
            let Some(index) = app.models.iter().position(|m| m.name.0.as_str() == cpath) else { continue };
            let Some(includes) = receiver_includes(&body, &cpath, param.as_deref(), &known) else { continue };
            let mut exprs = Vec::new();
            for (call, args) in &includes {
                let Ok(args) = args.iter().map(|a| super::expr::ingest_expr(a, &file)).collect::<Result<Vec<_>, _>>() else {
                    break;
                };
                let loc = call.location();
                let span = crate::span::Span {
                    file: super::sources::file_id(&file),
                    start: loc.start_offset() as u32,
                    end: loc.end_offset() as u32,
                };
                exprs.push(crate::expr::Expr::new(
                    span,
                    crate::expr::ExprNode::Send {
                        recv: None,
                        method: crate::ident::Symbol::from("include"),
                        args,
                        block: None,
                        parenthesized: false,
                    },
                ));
            }
            if exprs.len() != includes.len() {
                continue;
            }
            let model = &mut app.models[index];
            let own = model.span.file;
            let end = model.body.iter().rposition(|item| item.span().file == own).map_or(model.body.len(), |i| i + 1);
            let done = appended.entry(cpath.clone()).or_default();
            let at = (end + *done).min(model.body.len());
            *done += exprs.len();
            let items = exprs
                .into_iter()
                .map(|expr| ModelBodyItem::Unknown { expr, leading_comments: Vec::new(), leading_blank_line: false });
            model.body.splice(at..at, items);
            app.lifted_autoload_callbacks.push(crate::app::LiftedAutoloadCallback {
                cpath: cpath.clone(),
                file: file.clone(),
                start: stmt.location().start_offset() as u32,
            });
        }
    }
}

/// `Rails.autoloaders.main.on_load("Cpath") do |value, abspath| … end`
/// (or `.once`'s):
/// the constant path, the block's first parameter and its statements.
pub(super) fn autoload_on_load_block<'pr>(stmt: &Node<'pr>) -> Option<(String, Option<String>, Vec<Node<'pr>>)> {
    let call = stmt.as_call_node()?;
    if constant_id_str(&call.name()) != "on_load" || rails_autoloader(&call.receiver()?).is_none() {
        return None;
    }
    let args: Vec<Node<'pr>> = call.arguments()?.arguments().iter().collect();
    let [name] = args.as_slice() else { return None };
    let cpath = String::from_utf8_lossy(name.as_string_node()?.unescaped()).into_owned();
    let block = call.block()?.as_block_node()?;
    let param = block
        .parameters()
        .and_then(|p| p.as_block_parameters_node())
        .and_then(|p| p.parameters())
        .and_then(|p| p.requireds().iter().next())
        .and_then(|p| p.as_required_parameter_node())
        .map(|p| constant_id_str(&p.name()).to_string());
    let body = block.body().map(flatten_statements).unwrap_or_default();
    Some((cpath, param, body))
}

/// `Rails.autoloaders.main` or `Rails.autoloaders.once`: which.
pub(super) fn rails_autoloader(node: &Node<'_>) -> Option<&'static str> {
    let loader = node.as_call_node()?;
    let which = match constant_id_str(&loader.name()) {
        "main" => "main",
        "once" => "once",
        _ => return None,
    };
    if loader.arguments().is_some() || loader.block().is_some() {
        return None;
    }
    let loaders = loader.receiver()?;
    let loaders = loaders.as_call_node()?;
    (constant_id_str(&loaders.name()) == "autoloaders"
        && loaders.arguments().is_none()
        && loaders.block().is_none()
        && loaders.receiver().and_then(|r| constant_path_of(&r)).is_some_and(|p| p.join("::") == "Rails"))
    .then_some(which)
}

/// Each statement's call and module arguments, when every statement is
/// `<recv>.include(M, …)` or `<recv>.send(:include, M, …)` on the constant
/// `cpath` or the block parameter, naming modules the class body reads
/// the same; `None` otherwise.
fn receiver_includes<'pr>(
    body: &[Node<'pr>],
    cpath: &str,
    param: Option<&str>,
    known: &Known,
) -> Option<Vec<(CallNode<'pr>, Vec<Node<'pr>>)>> {
    let mut out = Vec::new();
    for stmt in body {
        let call = stmt.as_call_node()?;
        if call.block().is_some() {
            return None;
        }
        let recv = call.receiver()?;
        let on_class = constant_path_of(&recv).is_some_and(|p| p.join("::").trim_start_matches("::") == cpath)
            || recv.as_local_variable_read_node().is_some_and(|v| Some(constant_id_str(&v.name())) == param);
        if !on_class {
            return None;
        }
        let mut args: Vec<Node<'pr>> = call.arguments()?.arguments().iter().collect();
        match constant_id_str(&call.name()) {
            "include" => {}
            "send" | "__send__" => {
                if args.first()?.as_symbol_node()?.unescaped() != b"include" {
                    return None;
                }
                args.remove(0);
            }
            _ => return None,
        }
        if args.is_empty() {
            return None;
        }
        for arg in &args {
            let path = constant_path_of(arg)?;
            if !top_level_only(&path, cpath, known) {
                return None;
            }
        }
        out.push((call, args));
    }
    (!out.is_empty()).then_some(out)
}

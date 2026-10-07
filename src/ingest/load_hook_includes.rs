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
//!   class_load_hooks`), and that class is a model;
//! * every statement of the block is a `class <Model>` reopen without a
//!   superclass whose body is only `include` calls with constant
//!   arguments.
//!
//! A constant in the reopen is looked up from `<Model>` outward, as one
//! written in the model body is, so the lifted include is qualified the
//! same way (`qualify_relative_includes`). A bare `include M` in the
//! block (`run_load_hooks(:h, self)` evaluates it in the class) is not
//! lifted: its constants resolve from the initializer's top level, not
//! from the class. Any shape not lifted is left as it was: the initializer
//! scan runs what it can and names the rest at its site
//! (`initializer_unsupported`).

use std::path::{Path, PathBuf};

use ruby_prism::{CallNode, Node};

use crate::app::{App, ClassLoadHook};
use crate::dialect::ModelBodyItem;
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
    let (sites, _) = super::initializer_programs::class_load_hooks(sources, dir, &app.initializer_files);
    let mut lifted_from: crate::det::HashSet<crate::span::FileId> = crate::det::HashSet::default();
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
            let Some(includes) = literal_includes(&body, site) else { continue };
            let Ok(exprs) = includes.iter().map(|call| super::expr::ingest_expr(&call.as_node(), &file)).collect::<Result<Vec<_>, _>>() else {
                continue;
            };
            let Some(model) = app.models.iter_mut().find(|m| m.name.0.as_str() == site.class) else { continue };
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
/// of the class firing the hook, each holding only includes, or `None`
/// for any other block.
fn literal_includes<'pr>(body: &[Node<'pr>], site: &ClassLoadHook) -> Option<Vec<CallNode<'pr>>> {
    let mut out = Vec::new();
    for stmt in body {
        let class = stmt.as_class_node()?;
        if class.superclass().is_some() || class_name_path(&class)?.join("::") != site.class {
            return None;
        }
        for inner in class.body().map(flatten_statements).unwrap_or_default() {
            out.push(include_call(&inner)?);
        }
    }
    (!out.is_empty()).then_some(out)
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

//! Each superclass and `include` target the source writes, resolved
//! once from Rubydex's answers and stored on the IR in place.
//!
//! Ruby looks a constant up in the lexical scopes around the reference
//! (`Module.nesting`), then in the ancestors of the innermost one, then
//! at the top level. The written name alone cannot say which: in
//!
//! ```ruby
//! module Billing
//!   class Invoice < Record; end      # Billing::Record
//! end
//! class Billing::Receipt < Record; end # ::Record
//! ```
//!
//! both classes write `Record`, and only the second opens `Billing`
//! without making it a lexical scope. Rubydex has already resolved
//! every written reference against the whole app, so ingest asks it
//! once here, and analysis, lowering, emit and the IDE read the stored
//! name instead of each re-deriving it from the class's own name.
//!
//! A name keeps its written spelling when it has no source position
//! (a class a lowering synthesized) or Rubydex has no namespace answer
//! for it (a gem's class, a value). Ruby's own lookup found
//! no source declaration there, so no lexical guess could do better.

use crate::App;
use crate::det::HashMap;
use crate::analyze::ConstResolver;
use crate::dialect::{ControllerBodyItem, ModelBodyItem};
use crate::expr::{Expr, ExprNode};
use crate::ident::{ClassId, Symbol};
use crate::span::{SourceFile, Span};

/// Rewrite every source-written superclass and mixin in `app` to the
/// declaration Rubydex resolved it to.
pub(super) fn resolve(app: &mut App, sources: &[SourceFile], resolver: &ConstResolver) {
    let names = Names { sources, resolver };
    let aliases = module_aliases(app, &names);
    for class in &mut app.library_classes {
        // A `Data.define` block's includes are absolute already, and its
        // spans locate the factory call, not a written name.
        let data = class.data_define().is_some();
        if let Some(parent) = &mut class.parent {
            names.rewrite_parent(&class.name, parent, class.parent_span);
        }
        if data {
            continue;
        }
        for index in 0..class.includes.len() {
            let written = class.includes[index].0.as_str().to_string();
            let Some(span) = names.include_span(&class.include_spans, index, &written) else { continue };
            names.rewrite_include(&class.name, &mut class.includes[index], span, &aliases);
        }
        // `extend`/`prepend` a library class's own body does not model
        // structurally (unlike top-level `include`, above) stay in
        // `unknown_calls` as a raw `Send`; resolve those the same way,
        // so the Ruby/Spinel emitter's own lookup for which ones to
        // replay (`app_extension_calls` in `src/emit/ruby/library.rs`)
        // reads Rubydex's answer instead of guessing a second one.
        for call in &mut class.unknown_calls {
            names.rewrite_mixin_call(call, &["extend", "prepend"]);
        }
    }
    // A concern's `included do` statements, which the concern splices
    // copy onto each includer: an `include Guard` there names the module
    // as seen from inside the concern, not from the includer.
    for items in app.concern_model_items.values_mut() {
        for item in items {
            if let ModelBodyItem::Unknown { expr, .. } = item {
                names.rewrite_mixin_call(expr, &["include"]);
            }
        }
    }
    for model in &mut app.models {
        if let Some(parent) = &mut model.parent {
            names.rewrite_parent(&model.name, parent, model.parent_span);
        }
        for item in &mut model.body {
            if let ModelBodyItem::Unknown { expr, .. } = item {
                names.rewrite_mixin_call(expr, &["include"]);
            }
        }
    }
    for controller in &mut app.controllers {
        if let Some(parent) = &mut controller.parent {
            names.rewrite_parent(&controller.name, parent, controller.parent_span);
        }
        for sibling in &mut controller.sibling_classes {
            let mut parent = ClassId(sibling.parent.clone());
            names.rewrite_parent(&ClassId(sibling.name.clone()), &mut parent, sibling.parent_span);
            sibling.parent = parent.0;
        }
        for item in &mut controller.body {
            if let ControllerBodyItem::Unknown { expr, .. } = item {
                names.rewrite_mixin_call(expr, &["include"]);
            }
        }
    }
}

/// Each app constant that aliases a module (`Helpers = Gem::Helpers` in
/// a class or module body), by its full name: the module it names, as
/// Rubydex resolved the target or, for a gem's module it does not see,
/// as written. An alias of an alias is followed to the end.
fn module_aliases(app: &App, names: &Names<'_>) -> HashMap<String, (ClassId, bool)> {
    let mut aliases: HashMap<String, (ClassId, bool)> = HashMap::default();
    for class in &app.library_classes {
        for (name, value) in &class.constants {
            let ExprNode::Const { path } = &*value.node else { continue };
            let written = path.iter().map(Symbol::as_str).filter(|s| !s.is_empty()).collect::<Vec<_>>().join("::");
            let target = names.resolved(value.span, &written).unwrap_or_else(|| ClassId(Symbol::from(written.as_str())));
            let alias = format!("{}::{name}", class.name.0);
            if target.0.as_str() != alias {
                // Whether the target is a full name: Rubydex's answer, or
                // a rooted one (`::Gem::Helpers`). A relative name it has
                // no answer for stays relative.
                let rooted = path.first().is_some_and(|head| head.as_str().is_empty());
                let absolute = rooted || target.0.as_str() != written;
                aliases.insert(alias, (target, absolute));
            }
        }
    }
    let resolved: Vec<(String, (ClassId, bool))> = aliases
        .iter()
        .map(|(alias, target)| {
            let mut target = target.clone();
            for _ in 0..8 {
                match aliases.get(target.0.0.as_str()) {
                    Some(next) if next.0 != target.0 => target = next.clone(),
                    _ => break,
                }
            }
            (alias.clone(), target)
        })
        .collect();
    resolved.into_iter().collect()
}

struct Names<'a> {
    sources: &'a [SourceFile],
    resolver: &'a ConstResolver,
}

impl Names<'_> {
    fn source(&self, span: Span) -> Option<&str> {
        if span.is_synthetic() {
            return None;
        }
        let index = span.file.0.checked_sub(1)? as usize;
        Some(self.sources.get(index)?.text.as_str())
    }

    /// The namespace `written`, ending at `span.end`, resolves to.
    /// `None` unless the source spells that name there, so a span that
    /// a pass moved or a name it rewrote is never answered for a
    /// different reference.
    fn resolved(&self, span: Span, written: &str) -> Option<ClassId> {
        let text = self.source(span)?;
        let path: Vec<Symbol> = written.split("::").filter(|s| !s.is_empty()).map(Symbol::from).collect();
        let last = path.last()?.as_str();
        let start = (span.end as usize).checked_sub(last.len())?;
        if text.get(start..span.end as usize) != Some(last) {
            return None;
        }
        let Some(class) = self.resolver.namespace(span, &path) else {
            return self.resolved_prefix(span, &path, text);
        };
        let name = class.0.as_str();
        // A scoped `A::B` never names a top-level constant: Ruby does not
        // search `Object` for a scoped lookup (since 2.5 it is a
        // NameError). Rubydex does when the app reopens `A` without
        // declaring `B` (a gem's `A::B`), and its answer `Object` would
        // replace the real, undeclared `A::B`. `Object::B` is the
        // exception: it names the top level.
        if path.len() > 1 && !name.contains("::") && path[0].as_str() != "Object" {
            return None;
        }
        // The answer may end in another name: an app alias of a class
        // (`JobBase = BaseJob`) resolves to the class it names, as Ruby
        // binds it. A runtime or RBS alias (`Queue`) keeps the name the
        // modeled runtime knows it by.
        (name.rsplit("::").next() == Some(last) || !self.resolver.is_framework_alias(span, &path)).then(|| class.clone())
    }

    /// A name Rubydex has no namespace for (a gem's module) whose leading
    /// segments it does resolve: Ruby binds the first segment lexically
    /// and then looks the rest up inside it, so `Testing::Assertions`
    /// inside `module ActiveSupport` is `ActiveSupport::Testing::
    /// Assertions` once the app declares `ActiveSupport::Testing`. Stored
    /// as written, the name reads as the top-level `Testing::Assertions`
    /// to every consumer that is not Ruby's own lexical lookup.
    /// The longest resolved prefix, with the rest appended.
    ///
    /// Not for a name led by a framework root (`Rails::Railtie` inside
    /// `class Billing`): the framework defines it before any app file
    /// loads, while an app namesake (`Billing::Rails`, from a file that
    /// may load after this one) is only there once its file has run.
    /// Binding the prefix statically read `Billing::Rails::Railtie`, a
    /// NameError at boot. The written name stays. Likewise for a head
    /// Ruby's standard library declares (`Logger::Formatter` inside
    /// `module Tessera`, whose `Tessera::Logger` a later file opens).
    fn resolved_prefix(&self, span: Span, path: &[Symbol], text: &str) -> Option<ClassId> {
        if path.first().is_some_and(|head| {
            super::app::FRAMEWORK_ROOTS.contains(&head.as_str()) || self.resolver.has_stdlib_namespace(head.as_str())
        }) {
            return None;
        }
        let written_len: usize = path.iter().map(|s| s.as_str().len()).sum::<usize>() + 2 * (path.len() - 1);
        let start = (span.end as usize).checked_sub(written_len)?;
        (1..path.len()).rev().find_map(|keep| {
            let prefix = &path[..keep];
            let prefix_len = prefix.iter().map(|s| s.as_str().len()).sum::<usize>() + 2 * (keep - 1);
            let end = start + prefix_len;
            let last = prefix.last()?.as_str();
            if text.get(end - last.len()..end) != Some(last) {
                return None;
            }
            let class = self.resolver.namespace(Span { file: span.file, start: start as u32, end: end as u32 }, prefix)?;
            let name = class.0.as_str();
            if name.rsplit("::").next() != Some(last) {
                return None;
            }
            let rest = path[keep..].iter().map(Symbol::as_str).collect::<Vec<_>>().join("::");
            Some(ClassId(Symbol::from(format!("{name}::{rest}"))))
        })
    }

    /// A library class's `include` of `included`, at `span`.
    ///
    /// The resolved name is stored without a leading `::`, as every
    /// by-name consumer of `includes` expects, except where the class's
    /// own scope would read its head as something else (`include ::A::B`
    /// in an owner that also declares `Owner::A`): the emitted `include`
    /// sits inside one body per segment of the owner's name, so it keeps
    /// `::A::B` there.
    ///
    /// An include of an app alias of a module (`Helpers = Gem::Helpers`,
    /// which Rubydex answers as a value) includes the aliased module.
    fn rewrite_include(&self, owner: &ClassId, included: &mut ClassId, span: Span, aliases: &HashMap<String, (ClassId, bool)>) {
        let path: Vec<Symbol> = included.0.as_str().split("::").filter(|s| !s.is_empty()).map(Symbol::from).collect();
        if let Some((name, true)) = self.resolver.resolved_name(span, &path)
            && let Some((target, absolute)) = aliases.get(name.0.as_str())
        {
            *included = if *absolute { self.rooted_where_shadowed(owner, target) } else { target.clone() };
            return;
        }
        if let Some(resolved) = self.resolved(span, included.0.as_str()) {
            *included = self.rooted_where_shadowed(owner, &resolved);
        }
    }

    /// The full name `name`, rooted (`::A::B`) when a scope of `owner`
    /// (itself or an enclosing namespace) declares a namesake of its
    /// first segment, so the `include` written inside the owner's body
    /// still names it.
    fn rooted_where_shadowed(&self, owner: &ClassId, name: &ClassId) -> ClassId {
        if self.shadowed(owner, name) { ClassId(Symbol::from(format!("::{}", name.0))) } else { name.clone() }
    }

    /// Whether a scope of `owner` (itself or an enclosing namespace)
    /// declares a namesake of `name`'s first segment.
    fn shadowed(&self, owner: &ClassId, name: &ClassId) -> bool {
        let head = name.0.as_str().split("::").next().unwrap_or_default();
        let mut scope = Some(owner.0.as_str());
        while let Some(prefix) = scope {
            if self.resolver.has_source_namespace(&format!("{prefix}::{head}")) {
                return true;
            }
            scope = prefix.rsplit_once("::").map(|(prefix, _)| prefix);
        }
        false
    }

    /// The span of the `include` call that writes `written`: the call
    /// recorded at `index` when it spells the name, else the one call
    /// among `calls` that does (a pass that merged a reopen's includes,
    /// or added one, can leave the two lists out of step).
    fn include_span(&self, calls: &[Span], index: usize, written: &str) -> Option<Span> {
        if let Some(span) = calls.get(index).and_then(|call| self.argument_span(*call, written)) {
            return Some(span);
        }
        let mut found = calls.iter().filter_map(|call| self.argument_span(*call, written));
        let span = found.next()?;
        found.next().is_none().then_some(span)
    }

    /// A superclass of the class `owner`. Ruby evaluates the superclass
    /// before it creates the class, so `module Admin; class
    /// NotesController < NotesController` names the top-level one;
    /// Rubydex answers with the class being declared. A class is never
    /// its own superclass, so that answer leaves the name as written.
    fn rewrite_parent(&self, owner: &ClassId, parent: &mut ClassId, span: Span) {
        if let Some(resolved) = self.resolved(span, parent.0.as_str())
            && resolved != *owner
        {
            *parent = resolved;
        }
    }

    /// The span of `written` as an argument of the `include` call at
    /// `call`: the one place inside the call that spells it whole.
    fn argument_span(&self, call: Span, written: &str) -> Option<Span> {
        let text = self.source(call)?.get(call.start as usize..call.end as usize)?;
        let identifier = |c: char| c.is_alphanumeric() || c == '_' || c == ':';
        let mut found = None;
        for (offset, _) in text.match_indices(written) {
            let before = text[..offset].chars().next_back();
            let after = text[offset + written.len()..].chars().next();
            if before.is_some_and(identifier) || after.is_some_and(identifier) {
                continue;
            }
            if found.is_some() {
                // `include A, A` or a name repeated in a comment; ask
                // about neither rather than guess.
                return None;
            }
            let end = call.start + (offset + written.len()) as u32;
            found = Some(Span { file: call.file, start: call.start + offset as u32, end });
        }
        found
    }

    /// `include X` in a model or controller body, or (passing
    /// `&["extend", "prepend"]`) a library class's own `extend`/
    /// `prepend` captured verbatim in its `unknown_calls`. A rooted
    /// `::X` is absolute as written.
    fn rewrite_mixin_call(&self, expr: &mut Expr, methods: &[&str]) {
        let ExprNode::Send { recv: None, method, args, .. } = &mut *expr.node else { return };
        if !methods.contains(&method.as_str()) {
            return;
        }
        for arg in args.iter_mut() {
            let span = arg.span;
            let ExprNode::Const { path } = &mut *arg.node else { continue };
            if path.first().is_none_or(|head| head.as_str().is_empty()) {
                continue;
            }
            let written = path.iter().map(Symbol::as_str).collect::<Vec<_>>().join("::");
            if let Some(resolved) = self.resolved(span, &written) {
                *path = resolved.0.as_str().split("::").map(Symbol::from).collect();
            }
        }
    }
}

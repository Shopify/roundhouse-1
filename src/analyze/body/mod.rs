//! Body-typer: the Rails-agnostic core of type inference.
//!
//! Given an [`Expr`] + a [`Ctx`] + a method dispatch table (a
//! `HashMap<ClassId, ClassInfo>`), walks the tree and annotates each
//! node's `ty` field. This module knows nothing about schemas,
//! controllers, `before_action` chains, or any other Rails dialect —
//! it's pure "Ruby body type inference against known signatures."
//!
//! The Rails-aware [`super::Analyzer`] pre-computes the dispatch
//! table from `App.models` and then threads a [`BodyTyper`] over each
//! method body, action body, and view body.
//!
//! Runtime-extraction code (src/runtime_src.rs) uses the same
//! [`BodyTyper`] with a simpler dispatch table (no user classes, just
//! the primitive method tables).

use crate::det::HashMap;
use rubydex::model::identity_maps::IdentityHashMap;
use rubydex::model::ids::{DeclarationId, declaration_id_from_lookup_name};

use crate::expr::{Expr, ExprNode, HashRest, LValue, Literal, MatchPattern, Pattern};
use crate::ident::{ClassId, Symbol, TyVar};
use crate::ty::{Row, Ty};

/// A break in a bytes block can replace the method's String result. Nested
/// lambdas/iterators and while/until loops own their breaks independently.
// `Class[C]` is a class object that dispatch unwraps to `C`.
fn instance_shaped(ty: &Ty) -> bool {
    match ty {
        Ty::Class { id, .. } => id.0.as_str() != "Class",
        Ty::Union { variants } => variants.iter().all(|v| matches!(v, Ty::Nil) || instance_shaped(v)),
        _ => false,
    }
}

fn bytes_block_has_escaping_break(e: &Expr) -> bool {
    match &*e.node {
        ExprNode::Break { .. } => true,
        ExprNode::Lambda { .. } | ExprNode::While { .. } => false,
        _ => {
            let mut found = false;
            e.node.for_each_child(&mut |child| found |= bytes_block_has_escaping_break(child));
            found
        }
    }
}

/// A forwarded `&local` whose binding may be nil at runtime — Ruby then
/// passes no block. Bare `Nil` is handled separately as definite absence.
fn forwarded_block_may_be_nil(ty: &Ty) -> bool {
    matches!(ty, Ty::Union { variants } if variants.iter().any(|v| matches!(v, Ty::Nil)))
}

mod diagnostic;
mod deps;
pub(crate) use deps::{class_set_key, Classes, DepKey, DepRecorder, Mix};
mod const_resolution;
pub(crate) use const_resolution::{ConstResolver, ConstResolverTask};
pub use const_resolution::PreparedConstResolver;
use const_resolution::ResolvedConstant;
mod narrowing;
mod send;
pub(crate) use send::PARAM_VALUE;
pub(crate) use send::string_answers;

/// Typed constants for generated expressions without Ruby source spans.
/// The class's constants shadow the shared app registry. Source-backed
/// expressions use Rubydex declaration IDs instead of this scope.
#[derive(Clone, Default)]
pub struct ConstScope {
    own: std::sync::Arc<HashMap<Symbol, Ty>>,
    global: std::sync::Arc<HashMap<Symbol, Ty>>,
}

impl ConstScope {
    /// Just the app-wide registry.
    pub fn global(map: HashMap<Symbol, Ty>) -> Self {
        Self { own: Default::default(), global: std::sync::Arc::new(map) }
    }

    /// This scope's registry with `own` layered over it (a later entry
    /// wins, as `extend` would).
    pub fn with_own(&self, own: impl IntoIterator<Item = (Symbol, Ty)>) -> Self {
        Self { own: std::sync::Arc::new(own.into_iter().collect()), global: self.global.clone() }
    }

    pub fn get(&self, name: &Symbol) -> Option<&Ty> {
        self.own.get(name).or_else(|| self.global.get(name))
    }

    /// Only the constants this scope's class declares itself.
    pub fn get_own(&self, name: &Symbol) -> Option<&Ty> {
        self.own.get(name)
    }

    /// Only the app-wide, by-bare-name registry.
    pub fn get_global(&self, name: &Symbol) -> Option<&Ty> {
        self.global.get(name)
    }
}

/// Recursion context — what `self` is, what locals/ivars are in scope.
/// Immutable during descent; clone to enter a new scope (Let body,
/// block body, Seq walk with new ivar/local bindings).
#[derive(Clone, Default)]
pub struct Ctx {
    pub self_ty: Option<Ty>,
    /// What `self_ty` was before a self binding (`T.bind(self, T)`,
    /// `#: self as T`) replaced it for the rest of a body. Names in a
    /// declared type still resolve in the lexical namespace, not in
    /// the one `self` was bound to. `None` while nothing is bound.
    pub lexical_self: Option<Ty>,
    /// Ivar bindings observed as a `Seq` walks its statements in order.
    /// `@post = Post.find(...)` in stmt 1 lets `@post.destroy` in stmt 2
    /// dispatch correctly.
    pub ivar_bindings: HashMap<Symbol, Ty>,
    /// Local-variable bindings in the current scope: let-bound names,
    /// assignments accumulated through a `Seq`, and block parameters
    /// seeded from a receiver-aware dispatch.
    pub local_bindings: HashMap<Symbol, Ty>,
    /// Locals whose assigned value is proven to be a class/module object.
    /// Nominal instance types alone do not establish this identity.
    pub class_objects: crate::det::HashSet<Symbol>,
    /// Module/class-level typed constants such as
    /// `STATUS_CODES = { ok: 200, ... }.freeze`. Rubydex IDs resolve
    /// source-backed reads; this scope types generated expressions.
    ///
    /// SHARED, not owned: written once when a context is built and only
    /// read after, while `Ctx` is cloned for every block, branch and
    /// method the typer enters. As an owned map each of those clones
    /// copied the whole registry — every constant in the app, merged
    /// into each class's view of it — and on Mastodon that copying was
    /// most of the analysis time.
    pub constants: ConstScope,
    /// When set, the body-typer writes back `Some(SelfRef)` on the
    /// recv slot of bare Sends that resolve via `self_ty`'s dispatch
    /// table. Off by default — opt-in per call site so targets that
    /// model self differently (e.g. Crystal's module-of-self-actions
    /// controller shape, Python/Elixir/Go's similar) aren't forced to
    /// recognize a new IR shape across every existing per-target
    /// rewriter at once. Enable for paths that consume the
    /// LibraryClass directly as instance methods (runtime/ruby/* via
    /// parse_library_with_rbs, view_to_library's body-typer pass,
    /// the upcoming TS controller_thin path).
    pub annotate_self_dispatch: bool,
    /// Set when typing a view/layout body. In that context `yield` renders
    /// content to a String (bare `yield` → the layout's `body` String param;
    /// `yield :slot` → `ViewHelpers.get_slot` → String — see
    /// `lower/view_to_library/partial.rs::emit_yield`), so the Yield arm types
    /// it `String` instead of the generic `Untyped`. Off elsewhere, where a
    /// block's return type isn't tracked through the method signature.
    pub in_view: bool,
    /// Set while typing the body of a class-side method (`def self.x`,
    /// `class << self; def x`). `self` there is the class object, so an
    /// implicit-self `new` answers "an instance of whichever class
    /// received the call", not of the class the `def` sits in.
    pub class_side: bool,
    /// Set while typing a class-method whose name is a first-class model
    /// DSL lowerer (`has_markdown`, `has_rich_text`, …). Those bodies are
    /// macro templates: association/`scope`/`class_eval` leftovers are
    /// claimed by the dedicated lowerer at call sites, so attaching
    /// `ActiveRecord::Base … lacks a shared runtime` on the template
    /// itself is noise that hides the real ledger.
    pub claimed_macro_template: bool,
    /// The `def` whose body is being typed, by name and side
    /// (`true` for a class-side method). `super` re-sends it to the
    /// parent, so a block inherits it, even one that rebinds `self`:
    /// a block's `super` is its method's. `None` outside a method body
    /// and wherever the caller does not say.
    pub enclosing_def: Option<(Symbol, bool)>,
    // `!class_side` cannot stand in: scope bodies type with it false, and their bare `active` is the scope.
    pub instance_body: bool,
}

/// User-class dispatch data: table name (if any), instance shape,
/// class/instance method tables. Built by [`super::Analyzer`] from
/// Rails schema + conventions; the body-typer reads it.
#[derive(Default, Clone)]
pub struct ClassInfo {
    /// Kind of the indexed source declaration; never inferred from its name.
    pub is_module: bool,
    /// Constant values declared by external gem RBI/RBS files, or by
    /// a modeled stdlib module (`registry::stdlib`, e.g. `Process`).
    pub constants: HashMap<Symbol, Ty>,
    /// The sorbet type aliases the class declares (`Name =
    /// T.type_alias { X }`), as the types they name
    /// (`ingest::sorbet_check::alias_ty`); `expand_type_aliases` reads them.
    pub type_aliases: HashMap<Symbol, Ty>,
    /// If this class maps to a database table, which one.
    pub table: Option<crate::ident::TableRef>,
    /// Instance-state shape (columns + attr_accessor).
    pub attributes: Row,
    /// Methods callable on the class itself: `Post.all`, `Post.find(id)`.
    pub class_methods: HashMap<Symbol, Ty>,
    /// Source declares its own class-side `new`, whose result need not be self.
    pub declares_constructor: bool,
    /// Which of `class_methods` had their return type inferred FROM a
    /// query chain over this model — every `scope`, plus any class
    /// method whose body tail is such a chain.
    ///
    /// Relation- and Array-receiver dispatch delegates to the element
    /// model's class-method table, and this is the membership test that
    /// keeps the delegation honest. Forwarding arbitrary class methods
    /// would be Rails' method_missing semantics; an unresolved send
    /// should surface rather than silently forward. But a method whose
    /// body IS a relation chain over this model is exactly what Rails
    /// delegates through `scoping`, and it is the one case where the
    /// answer is knowable rather than guessed.
    ///
    /// The relation-RETURNING half of that surface already delegated by
    /// matching on `Ty::Relation`. This set extends it to the half that
    /// TERMINATES — campfire's `def self.original; order(:created_at)
    /// .first; end`, reached as `Current.user.rooms.original`, whose
    /// return is a record and so matches no relation shape. Without it
    /// the chain typed to nothing and `room_url(…)` rendered
    /// `/rooms/#<Room:0x…>`.
    pub relation_derived: crate::det::HashSet<Symbol>,
    /// Scopes whose body the seed classifier read as ending in a
    /// MATERIALIZING terminal — `ordered.last(PAGE_SIZE)` — as opposed
    /// to the ones it could not read at all. Both kinds may carry the
    /// `Array[Self]` seed (the counted terminal by classification, the
    /// unreadable body as the conservative stand-in), and only this set
    /// tells them apart at a Relation receiver: a materializing scope
    /// answers its Array, an unclassified one is assumed to still be a
    /// query and re-wraps as the relation.
    pub materializing_scopes: crate::det::HashSet<Symbol>,
    /// Methods whose every return is the value of the BLOCK they were
    /// called with — `yield` on each path, or the block handed on to
    /// another such method (lobsters' `get_from_cache(opts, &)`, which
    /// answers `yield` uncached and `Rails.cache.fetch(key, &)` cached).
    /// No signature can say that without method generics, which the RBS
    /// reader does not model, so dispatch answers the CALL SITE's block
    /// type for these, the way it already does for `then` and
    /// `transaction`. Harvested each fixpoint round
    /// (`Analyzer::harvest_method_returns`).
    pub block_value_methods: crate::det::HashSet<Symbol>,
    /// Methods callable on an instance: `post.title`, `post.destroy`.
    pub instance_methods: HashMap<Symbol, Ty>,
    /// AccessorKind per method — lets the body-typer flag Method
    /// dispatches so emitters add parens. AttributeReader/Writer
    /// dispatches keep `parenthesized` as ingested. Default `Method`
    /// is assumed when a name isn't present.
    pub class_method_kinds: HashMap<Symbol, crate::dialect::AccessorKind>,
    pub instance_method_kinds: HashMap<Symbol, crate::dialect::AccessorKind>,
    /// Parent class for inheritance lookups. The force-parens check
    /// in the Send arm walks this chain so methods inherited from
    /// (e.g.) ActiveRecord::Base resolve when called on a subclass
    /// (`new Article(...).save` looks up `save` on Article first,
    /// then ApplicationRecord, then Base — finding it as
    /// `AccessorKind::Method` and forcing the parens). Class lookups
    /// use the same `ClassId` shape the registry keys use.
    pub parent: Option<crate::ident::ClassId>,
    /// `has_many :memberships do def grant_to(users) … end end` — the
    /// methods an association EXTENSION BLOCK declares, keyed
    /// `(association, method)`.
    ///
    /// They cannot live in `instance_methods` of either class
    /// involved: they are not the owner's (you call them on the
    /// association) and not the target's (`Membership` has no
    /// `grant_to`). And the association read types `Array<Membership>`,
    /// which has forgotten which association produced it — so the
    /// dispatch arms cannot find them from the receiver's TYPE at all.
    /// The Send arm looks them up from the receiver's SHAPE instead,
    /// which is the same two hops the emit-time flattening reads
    /// (`room.memberships.grant_to(u)` → `room.memberships_grant_to(u)`).
    pub assoc_extensions: HashMap<(Symbol, Symbol), Ty>,
    /// Modules mixed in via `include` (e.g. a controller's
    /// `include IntervalHelper`). A mixed-in module's instance methods
    /// become instance methods of the includer, so dispatch consults
    /// these (their own instance_methods in the registry) after the
    /// class's own methods and before walking the parent. Empty for
    /// most classes.
    pub includes: Vec<crate::ident::ClassId>,
    /// Modeled modules the class body `extend`s, in source order: its
    /// singleton's ancestors, where a `class << self` body looks a
    /// constant up after its lexical scopes.
    pub extends: Vec<crate::ident::ClassId>,
    /// The model declares a `default_scope` that `lower::default_scope`
    /// reproduces at runtime: the arel pass leaves its queries (and its
    /// STI subclasses', found through `parent`) on the runtime relation,
    /// which applies the scope.
    pub default_scoped: bool,
    /// Declared by a gem's RBI rather than by the app or the catalog
    /// (`crate::gem_boundary`). Its own table is the gem's whole
    /// declared surface, so a miss on the class itself is a real miss --
    /// but an APP class that inherits from it also inherits whatever the
    /// gem's macros generate, which no declaration lists.
    pub gem_boundary: bool,
    /// The gem whose RBI declared it, when `gem_boundary`. An RBI is a
    /// type, not a runtime: a constant it declares runs only on a tree
    /// that loads that gem (`crate::gem_boundary::GemBoundary::loads`).
    pub gem: Option<String>,
    /// Some emitted tree loads `gem`, so its constants run as the gem's.
    pub gem_loaded: bool,
    /// The surface cannot be enumerated (`method_missing`, an unresolved
    /// mixin, an ancestor the RBI could not name): a lookup that misses
    /// is unknown, not wrong.
    pub open: bool,
    /// Declared in the app's own source (a model, controller, or
    /// library class), as opposed to the framework catalog. An app class
    /// is reachable from a bare name only through Ruby's constant
    /// lookup (the lexical scope, then the ancestors), never by its last
    /// segment alone.
    pub app_declared: bool,
}

impl ClassInfo {
    /// Whether `name` is one of this model's real SCHEMA TABLE columns.
    /// `attributes` is built once, straight off `Schema::tables` (see
    /// `ingest::model::row_from_table`), never merged with method-only
    /// surface like a `has_secure_password` reader or a plain `def` —
    /// so this is a strictly narrower, more precise test than "does
    /// `instance_methods` know this name," which also answers yes for
    /// synthesized non-column readers (`password_reset_token`). Shared
    /// by the body-typer's and the arel lowerer's dynamic-finder
    /// handling (#558) so a per-column check can't drift between them.
    pub fn has_schema_column(&self, name: &Symbol) -> bool {
        self.attributes.fields.contains_key(name)
    }
}

/// Resolve a single-segment Const ref (like `Const { path:
/// ["HashWithIndifferentAccess"] }` from app source) to a fully-
/// qualified ClassId by walking the class registry. Returns the
/// fully-qualified `Symbol` when exactly one registry key matches —
/// "matches" meaning the key has `::` separator(s) AND the last
/// segment equals `name`. App-class single-segment keys (`Article`,
/// `Comment`) don't match (no `::`), so they stay bare. Multiple
/// matches (rare — would be e.g. `ActiveRecord::Base` and
/// `ActionController::Base`) leave the ref bare too — body-typer
/// can't disambiguate without lexical scope, and the bare form
/// still types via the registry's last-segment alias entry.
/// The class a written owner path names, resolved the way Ruby
/// resolves a constant: the enclosing scopes from the inside out, then
/// the top level.
///
/// `DEFAULT_MODE = Mode::Fill` inside `UI::Selector` means
/// `UI::Selector::Mode`, and nothing else does — which matters in an
/// app where a dozen components each declare their own `Mode`. Walking
/// the nesting is what tells them apart; a suffix match over the class
/// registry cannot, and gives up on the ambiguity instead.
/// A class name written inside `scope` (a qualified class name),
/// resolved lexically: `scope::name`, then each enclosing namespace,
/// and finally the name as written. `None` when no registered class
/// answers, so an unknown name (a gem's class) is left as written.
pub(crate) fn lexical_class(
    written: &ClassId,
    scope: &str,
    classes: Classes<'_>,
) -> Option<ClassId> {
    let name = written.0.as_str();
    let mut parts: Vec<&str> = scope.split("::").filter(|s| !s.is_empty()).collect();
    while !parts.is_empty() {
        let candidate = ClassId(Symbol::from(format!("{}::{name}", parts.join("::")).as_str()));
        if classes.contains_key(&candidate) {
            return Some(candidate);
        }
        parts.pop();
    }
    classes.contains_key(written).then(|| written.clone())
}

/// Ruby's whole constant lookup for a class name written in `scope`: the
/// lexical scopes, then `scope`'s ancestors (its includes, the last one
/// first, then its superclass chain), then the top level. Core's
/// `Counters::LiveAggregatedCounter` includes `AggregatedCounter` and
/// annotates `@aggregated_counts = {} #: Hash[String, AggregatedData]`,
/// naming `AggregatedCounter::AggregatedData`. `lexical_class` alone left it
/// unresolved, so the annotation was refused as a declared type with no
/// declaration.
pub(crate) fn cref_class(written: &ClassId, scope: &str, classes: Classes<'_>) -> Option<ClassId> {
    let name = written.0.as_str();
    let mut parts: Vec<&str> = scope.split("::").filter(|s| !s.is_empty()).collect();
    while !parts.is_empty() {
        let candidate = ClassId(Symbol::from(format!("{}::{name}", parts.join("::")).as_str()));
        if classes.contains_key(&candidate) {
            return Some(candidate);
        }
        parts.pop();
    }
    fn ancestors(id: &ClassId, classes: Classes<'_>, out: &mut Vec<ClassId>) {
        if out.contains(id) || out.len() > 64 {
            return;
        }
        out.push(id.clone());
        let Some(info) = classes.get(id) else { return };
        // A mixin is recorded as written (`include AggregatedCounter`),
        // so it is found from inside the class body that wrote it; a
        // superclass from the namespace around the class. A module that
        // only nests classes is no registry entry of its own: each of its
        // lexical spellings stands in as an owner.
        for module in info.includes().iter().rev() {
            match lexical_class(module, id.0.as_str(), classes) {
                Some(module) => ancestors(&module, classes, out),
                None => {
                    let mut parts: Vec<&str> = id.0.as_str().split("::").collect();
                    while !parts.is_empty() {
                        out.push(ClassId(Symbol::from(format!("{}::{}", parts.join("::"), module.0).as_str())));
                        parts.pop();
                    }
                    out.push(module.clone());
                }
            }
        }
        if let Some(parent) = info.parent() {
            let outer = id.0.as_str().rsplit_once("::").map_or("", |(outer, _)| outer);
            let parent = lexical_class(parent, outer, classes).unwrap_or_else(|| parent.clone());
            ancestors(&parent, classes, out);
        }
    }
    let mut chain = Vec::new();
    ancestors(&ClassId(Symbol::from(scope)), classes, &mut chain);
    chain
        .iter()
        .skip(1)
        .map(|ancestor| ClassId(Symbol::from(format!("{}::{name}", ancestor.0.as_str()).as_str())))
        .find(|candidate| classes.contains_key(candidate))
        .or_else(|| classes.contains_key(written).then(|| written.clone()))
}

/// `ty` with every sorbet type alias it names (`Name = T.type_alias { X }`,
/// in `ClassInfo::type_aliases`) replaced by the type the alias names.
/// An alias is found the way `lexical_class` finds a class, innermost
/// enclosing scope of `scope` first, and its own type is read in the
/// scope that declares it. Bounded, so an alias cycle stays a class.
pub(crate) fn expand_type_aliases(ty: &Ty, scope: &str, classes: Classes<'_>) -> Ty {
    let has_class = std::cell::Cell::new(false);
    ty.map_class_ids(&|id| {
        has_class.set(true);
        id.clone()
    });
    if !has_class.get() {
        return ty.clone();
    }
    expand_type_aliases_by(
        ty,
        scope,
        &|owner, name| classes.get(owner).and_then(|info| info.type_aliases.get(name).cloned()),
        &|id, _| id.clone(),
        true,
    )
}

/// [`expand_type_aliases`] over any alias table: `alias(owner, name)` is the
/// type `owner::name = T.type_alias { ... }` names, and `class(id, scope)`
/// resolves a class name that is not an alias, read in `scope` (the alias's
/// own scope inside an expanded alias). Recurses through signatures too.
/// `normalize` joins a union an alias expanded into the analyzer's lattice
/// (a cast's type); without it a union keeps its written arms (a declared
/// signature, read as written).
pub(crate) fn expand_type_aliases_by(
    ty: &Ty,
    scope: &str,
    alias: &dyn Fn(&ClassId, &Symbol) -> Option<Ty>,
    class: &dyn Fn(&ClassId, &str) -> ClassId,
    normalize: bool,
) -> Ty {
    struct Expand<'a> {
        alias: &'a dyn Fn(&ClassId, &Symbol) -> Option<Ty>,
        class: &'a dyn Fn(&ClassId, &str) -> ClassId,
        normalize: bool,
    }
    impl Expand<'_> {
        fn ty(&self, ty: &Ty, scope: &str, depth: u8) -> Ty {
            match ty {
                Ty::Class { id, args } if args.is_empty() => {
                    if depth < 8 && let Some((owner, aliased)) = self.find(id, scope) {
                        return self.ty(&aliased, &owner, depth + 1);
                    }
                    Ty::Class { id: (self.class)(id, scope), args: Vec::new() }
                }
                Ty::Class { id, args } => {
                    let args: Vec<Ty> = args.iter().map(|t| self.ty(t, scope, depth)).collect();
                    if is_stdlib_generic(id) {
                        return Ty::Class { id: id.clone(), args };
                    }
                    // A constant naming a class (`Results::Result =
                    // ::Shopify::Adt::Result`) takes the written arguments.
                    if depth < 8
                        && let Some((owner, aliased)) = self.find(id, scope)
                        && matches!(&aliased, Ty::Class { args: none, .. } if none.is_empty())
                        && let Ty::Class { id: target, args: target_args } = self.ty(&aliased, &owner, depth + 1)
                        && target_args.is_empty()
                    {
                        return Ty::Class { id: target, args };
                    }
                    Ty::Class { id: (self.class)(id, scope), args }
                }
                Ty::Array { elem } => Ty::Array { elem: Box::new(self.ty(elem, scope, depth)) },
                Ty::Hash { key, value } => Ty::Hash {
                    key: Box::new(self.ty(key, scope, depth)),
                    value: Box::new(self.ty(value, scope, depth)),
                },
                Ty::Tuple { elems } => Ty::Tuple { elems: elems.iter().map(|t| self.ty(t, scope, depth)).collect() },
                Ty::Record { row } => Ty::Record {
                    row: crate::ty::Row {
                        fields: row.fields.iter().map(|(name, t)| (name.clone(), self.ty(t, scope, depth))).collect(),
                        rest: row.rest.clone(),
                    },
                },
                Ty::Union { variants } => {
                    let arms: Vec<Ty> = variants.iter().map(|t| self.ty(t, scope, depth)).collect();
                    if self.normalize { union_many(arms) } else { Ty::Union { variants: arms } }
                }
                Ty::Fn { params, block, ret, effects } => Ty::Fn {
                    params: params
                        .iter()
                        .map(|p| crate::ty::Param { name: p.name.clone(), ty: self.ty(&p.ty, scope, depth), kind: p.kind.clone() })
                        .collect(),
                    block: block.as_ref().map(|b| Box::new(self.ty(b, scope, depth))),
                    ret: Box::new(self.ty(ret, scope, depth)),
                    effects: effects.clone(),
                },
                other => other.clone(),
            }
        }

        fn find(&self, written: &ClassId, scope: &str) -> Option<(String, Ty)> {
            let (prefix, name) = match written.0.as_str().rsplit_once("::") {
                Some((prefix, name)) => (prefix.trim_start_matches("::"), name),
                None => ("", written.0.as_str()),
            };
            let mut parts: Vec<&str> = scope.split("::").filter(|s| !s.is_empty()).collect();
            loop {
                let owner = parts.iter().copied().chain((!prefix.is_empty()).then_some(prefix)).collect::<Vec<_>>().join("::");
                if !owner.is_empty()
                    && let Some(aliased) = (self.alias)(&ClassId(Symbol::from(owner.as_str())), &Symbol::from(name))
                {
                    return Some((owner, aliased));
                }
                if parts.pop().is_none() {
                    return None;
                }
            }
        }
    }
    Expand { alias, class, normalize }.ty(ty, scope, 0)
}

/// A parameterized stdlib generic written unqualified: `Set[String]`,
/// `Range[Integer]`, `Enumerable[T]`, `Enumerator[T]`, `Class[top]`,
/// `Module[top]`, and `T::Set[X]` read the same way. Sorbet reads such a
/// name as `T::Set[String]` (the stdlib class) whatever the lexical scope
/// holds: inside `module Metafields`, beside `Metafields::Set`, `#: ->
/// Set[String]` is `T::Set[String]` while a bare `#: -> Set` is
/// `Metafields::Set` (srb 0.6.13502 `T.reveal_type`). So only the
/// argument-less spelling resolves lexically.
fn is_stdlib_generic(id: &ClassId) -> bool {
    matches!(
        id.0.as_str(),
        "Set" | "Range" | "Enumerable" | "Enumerator" | "Enumerator::Lazy" | "Enumerator::Chain" | "Class" | "Module"
    )
}

/// Reusable body-type walker. Holds a borrow of the dispatch table so
/// repeated `analyze_expr` calls reuse the same lookup structures
/// without cloning.
#[derive(Clone)]
pub struct BodyTyper<'a> {
    classes: &'a HashMap<ClassId, ClassInfo>,
    /// Notes every registry read when the caller wants to know what the
    /// typing depended on (the incremental library pass).
    deps: Option<&'a DepRecorder>,
    const_resolver: Option<std::sync::Arc<ConstResolver>>,
    typed_constants: Option<&'a IdentityHashMap<DeclarationId, Ty>>,
    data_factories: Option<&'a HashMap<crate::span::Span, Ty>>,
    /// Methods whose value is an ActiveSupport inquirer (see
    /// [`crate::analyze::inquiry`]); empty for the bare constructor,
    /// which the runtime-source typer and tests use.
    inquirers: Option<&'a crate::det::HashSet<Symbol>>,
    /// The base each load hook fires with (see `Analyzer::load_hook_bases`).
    load_hook_bases: Option<&'a std::collections::BTreeMap<String, ClassId>>,
    /// What the natively loaded gems' installed sources declare.
    gem_sources: Option<std::sync::Arc<crate::gem_boundary::GemSourceConstants>>,
}

impl<'a> BodyTyper<'a> {
    /// Submodule accessor for the dispatch table. `classes` itself
    /// stays private to this module; `send.rs` reaches it here.
    pub(super) fn classes(&self) -> Classes<'a> {
        Classes::new(self.classes, self.deps)
    }

    /// A source constant's inferred value, noted as a read.
    fn typed_constant(&self, declaration: &DeclarationId) -> Option<&'a Ty> {
        if let Some(deps) = self.deps {
            deps.note(DepKey::Constant(declaration));
        }
        self.typed_constants.and_then(|values| values.get(declaration))
    }

    /// The full name `path` has in the innermost of its Ruby lexical
    /// scopes (Rubydex's nesting, so `class A::B` does not search `A`)
    /// that models it, as a class or as a constant of the scope's table.
    /// Top-level names are left to the exact lookup.
    fn lexical_external_constant(&self, span: crate::span::Span, path: &[Symbol]) -> Option<ClassId> {
        let (name, owner) = path.split_last()?;
        let written = path.iter().map(Symbol::as_str).collect::<Vec<_>>().join("::");
        for scope in self.const_resolver.as_ref()?.unresolved_nesting(span, path) {
            let candidate = ClassId(Symbol::from(format!("{scope}::{written}").as_str()));
            if self.classes().contains_key(&candidate) {
                return Some(candidate);
            }
            let holder = match owner {
                [] => scope.to_string(),
                _ => format!("{scope}::{}", owner.iter().map(Symbol::as_str).collect::<Vec<_>>().join("::")),
            };
            if self.classes().get(&ClassId(Symbol::from(holder.as_str()))).is_some_and(|info| info.constants.contains_key(name)) {
                return Some(candidate);
            }
        }
        None
    }

    /// `Head::Rest` read through a constant of an enclosing scope that
    /// names a class or module (`Head = Gem::Module`, an entry of the
    /// scope's `ClassInfo::type_aliases` from `class_constant_aliases`):
    /// the path Ruby reads, `Gem::Module::Rest`. The innermost lexical
    /// scope that has the alias answers, as Ruby's lexical lookup does.
    fn aliased_constant_path(&self, span: crate::span::Span, path: &[Symbol]) -> Option<Vec<Symbol>> {
        let (head, rest) = path.split_first()?;
        if rest.is_empty() || head.as_str().is_empty() {
            return None;
        }
        let resolver = self.const_resolver.as_ref()?;
        resolver.unresolved_nesting(span, path).iter().find_map(|scope| {
            let info = self.classes().get(&ClassId(Symbol::from(scope.as_ref())))?;
            let Ty::Class { id, .. } = info.type_aliases.get(head)? else { return None };
            let mut target: Vec<Symbol> = id.0.as_str().split("::").map(Symbol::from).collect();
            target.extend(rest.iter().cloned());
            Some(target)
        })
    }

    /// The nearest ancestor of the enclosing class that models `name`,
    /// in Ruby's order: each class's included modules (the last included
    /// first), then its superclass. A superclass counts by its modeled
    /// constant table; a module also by a modeled class nested in it
    /// (`Result` in a class that includes shopify-adt's `Shopify::Adt`).
    fn inherited_constant_owner(&self, ctx: &Ctx, name: &Symbol) -> Option<ClassId> {
        let Some(Ty::Class { id, .. }) = ctx.lexical_self.as_ref().or(ctx.self_ty.as_ref()) else { return None };
        let mut info = self.classes().get(id)?;
        for _ in 0..32 {
            if let Some(owner) = self.included_constant_owner(&info.includes, name, 0) {
                return Some(owner);
            }
            let class = info.parent.clone()?;
            info = self.classes().get(&class)?;
            if info.constants.contains_key(name) {
                return Some(class);
            }
        }
        None
    }

    /// In a `class << self` body (Rubydex's innermost scope is the
    /// singleton, `Contact::<Contact>`) Ruby looks a constant up in the
    /// singleton's ancestors: the modules the class extends, the last
    /// first. Core's `CustomerApiSimulatedResponse::CompanyContact` reads
    /// `TIME_BEFORE` from `extend Checkouts::One::SimulatedCheckoutService`
    /// in a singleton that also includes a module Rubydex cannot see.
    /// The full name found, and the source declaration when it is a value
    /// (a class nested in the module has none).
    fn extended_constant(&self, span: crate::span::Span, path: &[Symbol]) -> Option<(ClassId, Option<DeclarationId>)> {
        let resolver = self.const_resolver.as_ref()?;
        let innermost = resolver.unresolved_nesting(span, path).first()?;
        if !innermost.rsplit("::").next()?.starts_with('<') {
            return None;
        }
        let attached = innermost.split("::").filter(|s| !s.starts_with('<')).collect::<Vec<_>>().join("::");
        let info = self.classes().get(&ClassId(Symbol::from(attached.as_str())))?;
        let written = path.iter().map(Symbol::as_str).collect::<Vec<_>>().join("::");
        let mut seen = crate::det::HashSet::default();
        info.extends.iter().rev().find_map(|module| self.module_constant(resolver, module, &written, &mut seen))
    }

    /// `written` under `module` or, Ruby's order, the modules it includes
    /// (the last first).
    fn module_constant(
        &self,
        resolver: &ConstResolver,
        module: &ClassId,
        written: &str,
        seen: &mut crate::det::HashSet<ClassId>,
    ) -> Option<(ClassId, Option<DeclarationId>)> {
        if !seen.insert(module.clone()) {
            return None;
        }
        // A later pass reads the path this one qualified.
        let full = match written.strip_prefix(module.0.as_str()) {
            Some(rest) if rest.starts_with("::") => written.to_string(),
            _ => format!("{}::{written}", module.0),
        };
        if let Some(declaration) = resolver.source_constant(&full) {
            return Some((ClassId(Symbol::from(full.as_str())), Some(declaration)));
        }
        let id = ClassId(Symbol::from(full.as_str()));
        if self.classes().contains_key(&id) {
            return Some((id, None));
        }
        let includes = self.classes().get(module)?.includes.clone();
        includes.iter().rev().find_map(|inner| self.module_constant(resolver, inner, written, seen))
    }

    fn included_constant_owner(&self, includes: &[ClassId], name: &Symbol, depth: u8) -> Option<ClassId> {
        if depth > 8 {
            return None;
        }
        for module in includes.iter().rev() {
            let Some(info) = self.classes().get(module) else { continue };
            if info.constants.contains_key(name)
                || self.classes().contains_key(&ClassId(Symbol::from(format!("{}::{}", module.0, name).as_str())))
            {
                return Some(module.clone());
            }
            if let Some(owner) = self.included_constant_owner(&info.includes, name, depth + 1) {
                return Some(owner);
            }
        }
        None
    }

    /// The gem whose RBI is the only declaration of the constant `path`
    /// names: the class or module itself, else a value its owner assigns.
    /// With it, whether an emitted tree loads that gem.
    fn declaring_gem(&self, path: &[Symbol]) -> Option<(String, bool)> {
        if let Some(info) = self.classes().get(&written_class_id(path)).filter(|info| info.gem.is_some()) {
            return info.gem.clone().map(|gem| (gem, info.gem_loaded));
        }
        let (name, owner) = path.split_last()?;
        if owner.is_empty() {
            return None;
        }
        let info = self.classes().get(&written_class_id(owner))?;
        info.constants.contains_key(name).then(|| info.gem.clone().map(|gem| (gem, info.gem_loaded))).flatten()
    }

    /// The gem owning the nearest enclosing namespace of `name` that the
    /// registry knows, when that namespace is a gem's (its RBI declares
    /// it, and neither the app nor the framework catalog does).
    fn namespace_gem(&self, name: &str) -> Option<String> {
        let mut scope = name;
        while let Some((outer, _)) = scope.rsplit_once("::") {
            scope = outer;
            if let Some(info) = self.classes().get(&ClassId(Symbol::from(scope))) {
                if info.app_declared {
                    return None;
                }
                return info.gem.clone();
            }
        }
        None
    }

    /// The gem that provides the unindexed constant `name` at runtime on
    /// the CRuby tree: the gem owning its nearest known namespace, when
    /// the tree loads that gem natively.
    fn runtime_constant_owner(&self, name: &str) -> Option<String> {
        let gem = self.namespace_gem(name)?;
        let mut scope = name;
        while let Some((outer, _)) = scope.rsplit_once("::") {
            scope = outer;
            if let Some(info) = self.classes().get(&ClassId(Symbol::from(scope))) {
                return info.gem_loaded.then_some(gem);
            }
        }
        None
    }

    /// How a gem the CRuby tree loads natively provides the unindexed
    /// constant `name` written in lexical `scope`: `Ok((fact, gem))`, the
    /// fact phrased for the diagnostic. By the gem's installed source when
    /// it was read: the constant it declares exactly, else a namespace it
    /// fills under computed names (namespace ownership, said so). Without
    /// a read source, by the namespace its RBI declares. `Err` carries the
    /// refusal the source proves (its nearest namespace is a fully read
    /// gem's, which does not declare it), or is empty.
    fn runtime_constant_provider(&self, name: &str, scope: &str) -> Result<(String, String), String> {
        if let Some(sources) = self.gem_sources.as_deref() {
            let version = |gem: &str| sources.scans.get(gem).map(|s| s.version.as_str()).unwrap_or("");
            match sources.provider(name, scope) {
                Some(provider) => return Ok((provider.describe(name, sources), provider.gem().to_string())),
                None => {
                    if let Some(gem) = sources.complete_namespace_owner(name) {
                        return Err(format!(
                            "{name} has no indexed declaration, and no installed source of a gem the CRuby tree loads declares it (its nearest namespace is {gem} {}'s, read whole)",
                            version(gem)
                        ));
                    }
                }
            }
        }
        match self.runtime_constant_owner(name) {
            Some(gem) => Ok((
                format!(
                    "{name} sits in a namespace the {gem} gem's RBI declares (namespace ownership; {})",
                    if self.gem_sources.is_some() { "that gem's source was not read" } else { "no gem home was read" }
                ),
                gem,
            )),
            None => Err(String::new()),
        }
    }

    /// Whether `self`'s class, its includes or its ancestors register
    /// `method` — an app definition, whatever type it answered.
    fn app_defines(&self, self_ty: Option<&Ty>, method: &Symbol) -> bool {
        let Some(Ty::Class { id, .. }) = self_ty else { return false };
        let mut stack = vec![id];
        let mut seen = crate::det::HashSet::default();
        while let Some(cid) = stack.pop() {
            if !seen.insert(cid) {
                continue;
            }
            let Some(cls) = self.classes().get(cid) else { continue };
            if cls.has_instance_method(method) || cls.has_class_method(method) {
                return true;
            }
            stack.extend(cls.includes().iter());
            stack.extend(cls.parent());
        }
        false
    }

    pub fn new(classes: &'a HashMap<ClassId, ClassInfo>) -> Self {
        Self {
            classes,
            deps: None,
            const_resolver: None,
            typed_constants: None,
            data_factories: None,
            inquirers: None,
            load_hook_bases: None,
            gem_sources: None,
        }
    }

    /// Decide declared types against the loaded gems' installed sources.
    pub(crate) fn with_gem_sources(mut self, sources: std::sync::Arc<crate::gem_boundary::GemSourceConstants>) -> Self {
        self.gem_sources = Some(sources).filter(|s| !s.is_empty());
        self
    }

    /// The app-wide bare-name value of `name`, noted as a read.
    fn global_constant<'c>(&self, ctx: &'c Ctx, name: &Symbol) -> Option<&'c Ty> {
        if let Some(deps) = self.deps {
            deps.note(DepKey::ConstName(name));
        }
        ctx.constants.get_global(name)
    }

    /// Note every registry read in `deps`.
    pub(crate) fn with_deps(mut self, deps: &'a DepRecorder) -> Self {
        self.deps = Some(deps);
        self
    }

    /// Share the analyzer's immutable source index across typing passes.
    pub(crate) fn with_const_resolver(mut self, resolver: std::sync::Arc<ConstResolver>) -> Self {
        self.const_resolver = Some(resolver);
        self
    }

    /// Roundhouse infers values; Rubydex supplies their declaration IDs.
    pub(super) fn with_typed_constants(
        mut self,
        values: &'a IdentityHashMap<DeclarationId, Ty>,
    ) -> Self {
        self.typed_constants = Some(values);
        self
    }

    pub(super) fn with_data_factories(mut self, factories: &'a HashMap<crate::span::Span, Ty>) -> Self {
        self.data_factories = Some(factories);
        self
    }

    /// Name the app's inquirer-returning methods, so their predicates
    /// type as Bool instead of failing dispatch on `Str`.
    pub fn with_load_hook_bases(mut self, bases: &'a std::collections::BTreeMap<String, ClassId>) -> Self {
        self.load_hook_bases = Some(bases);
        self
    }

    pub fn with_inquirers(
        mut self,
        inquirers: &'a crate::det::HashSet<Symbol>,
    ) -> Self {
        self.inquirers = Some(inquirers);
        self
    }

    /// Type each parameter default under `ctx`, then bind the locals the
    /// defaults assign (see [`bind_default_assigned_locals`]). For the
    /// paths that seed a body from a signature and never typed the
    /// defaults: `runtime_src` and the probes that mirror it.
    pub fn seed_param_default_locals(&self, params: &mut [crate::dialect::Param], ctx: &mut Ctx) {
        for p in params.iter_mut() {
            if let Some(default) = &mut p.default {
                self.analyze_expr(default, ctx);
            }
        }
        bind_default_assigned_locals(params, ctx);
    }

    /// Analyze an expression: compute its type, populate `expr.ty`,
    /// return the computed type. Recurses into sub-expressions, which
    /// in turn get their `ty` populated. After typing, runs a
    /// diagnostic-detection pass on the node to flag sites the body-
    /// typer recognizes as user errors (Incompatible `+`, …). The
    /// annotation rides with the IR so emitters can render a runtime
    /// raise-equivalent without re-classifying.
    pub fn analyze_expr(&self, expr: &mut Expr, ctx: &Ctx) -> Ty {
        let ty = self.compute(expr, ctx);
        expr.ty = Some(ty.clone());
        expr.decisions &= !crate::expr::CLASS_OBJECT_VALUE;
        if self.is_class_object(expr, ctx) || self.is_class_of_value(expr, ctx) {
            expr.decisions |= crate::expr::CLASS_OBJECT_VALUE;
        }
        diagnostic::detect_diagnostic(expr);
        ty
    }

    /// Pattern-side expressions use ordinary expression/constant resolution;
    /// recognizing pattern syntax does not grant unknown classes new support.
    fn analyze_match_pattern_constants(&self, pattern: &mut MatchPattern, ctx: &Ctx) {
        pattern.for_each_expr_mut(&mut |expr| { self.analyze_expr(expr, ctx); });
    }

    /// The locals a `MatchPattern` binds when it matches, typed against
    /// `subject_ty` — the static type of the value being tested AT
    /// THIS POSITION (the `CaseMatch` scrutinee at the top level, `None`
    /// for anything nested inside an `Array`/`Find` pattern, whose
    /// `deconstruct` return isn't modeled positionally).
    ///
    /// Only a *plain* `Hash` pattern (no `constant` narrowing) against a
    /// statically `Hash[Symbol, V]`-typed subject gets typed key
    /// bindings, and every key gets the SAME `V` — `Ty::Hash` has no
    /// per-key shape, so `{status:, data:}` against `Hash[Sym, String]`
    /// types both `status` and `data` as `String`, which is exactly
    /// right for a value-omission bind (`data:`) and merely
    /// conservative for one with an explicit sub-pattern. A
    /// `constant`-narrowed `Hash`/`Array` pattern (`Success(value:)`,
    /// `Success(page)`) always types its bindings `Untyped`: reading a
    /// narrowing class's own attribute types back out (real
    /// `deconstruct_keys` fidelity) is future work, and every one of
    /// those constants in the Procore survey is a gem class the
    /// registry doesn't know regardless, so today the two cases collapse
    /// to the same answer.
    fn match_pattern_bindings(
        &self,
        pattern: &MatchPattern,
        subject_ty: Option<&Ty>,
    ) -> Vec<(Symbol, Ty)> {
        match pattern {
            MatchPattern::Value { .. } | MatchPattern::Nil => Vec::new(),
            MatchPattern::Bind { name } => {
                vec![(name.clone(), subject_ty.cloned().unwrap_or(Ty::Untyped))]
            }
            MatchPattern::Capture { pattern, name } => {
                let mut out = self.match_pattern_bindings(pattern, subject_ty);
                out.push((name.clone(), subject_ty.cloned().unwrap_or(Ty::Untyped)));
                out
            }
            // Ruby permits `_`-prefixed bindings in alternatives. A
            // different alternative can succeed without that binding.
            MatchPattern::Alt { alternatives } => alternatives.iter()
                .flat_map(|p| self.match_pattern_bindings(p, subject_ty))
                .map(|(name, ty)| (name, union_of(ty, Ty::Nil))).collect(),
            MatchPattern::Array { pre, rest, post, .. } => {
                let mut out = Vec::new();
                for p in pre.iter().chain(post.iter()) {
                    out.extend(self.match_pattern_bindings(p, None));
                }
                if let Some(Some(name)) = rest {
                    out.push((name.clone(), Ty::Untyped));
                }
                out
            }
            MatchPattern::Find { middle, pre_rest, post_rest, .. } => {
                let mut out = Vec::new();
                for p in middle {
                    out.extend(self.match_pattern_bindings(p, None));
                }
                if let Some(name) = pre_rest {
                    out.push((name.clone(), Ty::Untyped));
                }
                if let Some(name) = post_rest {
                    out.push((name.clone(), Ty::Untyped));
                }
                out
            }
            MatchPattern::Hash { constant, pairs, rest } => {
                let value_ty = if constant.is_none() {
                    match subject_ty {
                        Some(Ty::Hash { key, value }) if matches!(**key, Ty::Sym) => {
                            Some((**value).clone())
                        }
                        _ => None,
                    }
                } else {
                    None
                };
                let mut out = Vec::new();
                for (key, sub) in pairs {
                    match sub {
                        Some(p) => out.extend(self.match_pattern_bindings(p, value_ty.as_ref())),
                        None => out.push((key.clone(), value_ty.clone().unwrap_or(Ty::Untyped))),
                    }
                }
                if let Some(HashRest::Collect { name }) = rest {
                    // The rest is a Hash returned by deconstruct_keys,
                    // never the deconstructed object. Unmatched keys
                    // need not be symbols, even though pattern keys are.
                    let rest_ty = match subject_ty {
                        Some(ty @ Ty::Hash { .. }) if constant.is_none() => ty.clone(),
                        _ => Ty::Hash { key: Box::new(Ty::Untyped), value: Box::new(Ty::Untyped) },
                    };
                    out.push((name.clone(), rest_ty));
                }
                out
            }
        }
    }

    /// Matches nested in expressions can bind locals too. Conditional
    /// matches may keep a prior/nil value; a required-match statement
    /// definitely binds on the continuing path.
    fn propagate_match_bindings(&self, expr: &Expr, ctx: &mut Ctx, definite: bool) {
        if matches!(&*expr.node, ExprNode::Lambda { .. } | ExprNode::Let { .. }) { return; }
        let mut bind = |pattern: &MatchPattern, subject_ty: Option<&Ty>| {
            for (name, ty) in self.match_pattern_bindings(pattern, subject_ty) {
                let ty = if definite && matches!(&*expr.node, ExprNode::MatchRequired { .. }) {
                    ty
                } else {
                    union_of(ctx.local_bindings.get(&name).cloned().unwrap_or(Ty::Nil), ty)
                };
                ctx.class_objects.remove(&name);
                ctx.local_bindings.insert(name, ty);
            }
        };
        match &*expr.node {
            ExprNode::CaseMatch { scrutinee, arms, .. } => {
                for arm in arms { bind(&arm.pattern, scrutinee.ty.as_ref()); }
            }
            ExprNode::MatchPredicate { value, pattern } | ExprNode::MatchRequired { value, pattern } => {
                bind(pattern, value.ty.as_ref());
            }
            _ => {}
        }
        expr.node.for_each_child(&mut |child| self.propagate_match_bindings(child, ctx, false));
    }

    /// The type of `name` when it is an attribute of the class `self` is an
    /// instance of: a column or an `attr_accessor`/`attr_reader`, its own or an
    /// ancestor's. Such a read is a plain state read, which is what lets a
    /// guard on it (`expires_at.present? && expires_at > now`) speak for
    /// the reads that follow it the way a guard on a local does.
    ///
    /// The type is the reader's, not the column's: `serialize :metadata`,
    /// an enum or a money column registers a reader that answers something
    /// other than the storage type, and `return if metadata.nil?` must not
    /// narrow the reads after it back to the column's `String`.
    fn self_attribute_ty(&self, name: &Symbol, ctx: &Ctx) -> Option<Ty> {
        let Some(Ty::Class { id, .. }) = &ctx.self_ty else {
            return None;
        };
        let mut cursor = Some(id.clone());
        // Bounded: a parent link that cycles must not hang the typer.
        for _ in 0..16 {
            let Some(cur) = cursor else { break };
            let Some(info) = self.classes().get(&cur) else { break };
            if let Some(ty) = info.attributes.fields.get(name) {
                return Some(info.instance_methods.get(name).unwrap_or(ty).clone());
            }
            cursor = info.parent.clone();
        }
        None
    }

    /// A mixin argument naming a module: a proven class object, or a
    /// bare constant at a file's top level that names a modeled module.
    /// Rubydex records no reference for the argument of an `include`
    /// outside any class or module body (rubydex d8ff475 ruby_indexer.rs
    /// `handle_mixin` returns before `add_constant_reference` without a
    /// lexical nesting), so the read was left unknown; Ruby reads it from
    /// the top-level cref, the exact written name, which is how the read
    /// is then typed.
    fn top_level_mixin_argument(&self, arg: &mut Expr, ctx: &Ctx) -> bool {
        let ExprNode::Const { path } = &mut *arg.node else { return false };
        if arg.diagnostic.is_none() {
            return self.is_class_object(arg, ctx);
        }
        let id = written_class_id(path);
        let unreferenced = !arg.span.is_synthetic()
            && self.const_resolver.as_ref().is_some_and(|r| r.has_source_file(arg.span.file) && r.reference(arg.span, path).is_none());
        let module = self.classes().get(&id).is_some_and(|info| info.is_module);
        let unknown_constant = matches!(&arg.diagnostic, Some(crate::diagnostic::DiagnosticKind::Unsupported { construct, detail, .. })
            if construct.as_str() == "constant" && *detail == id.0.as_str());
        if !(unreferenced && module && unknown_constant) {
            return false;
        }
        arg.diagnostic = None;
        arg.decisions |= crate::expr::RESOLVED_CLASS_REF;
        arg.ty = Some(Ty::Class { id, args: vec![] });
        true
    }

    /// Identity evidence for Module protocol, independent of nominal instance type.
    fn is_class_object(&self, expr: &Expr, ctx: &Ctx) -> bool {
        // `Class[top]`, `Module`, `T::Class[X]`: a declared type that is
        // the class object itself, carried by a local or a call. A
        // constant and `x.singleton_class` keep their own proofs below
        // (`1.singleton_class` is typed `Class` yet raises TypeError).
        let typed_class_value = || matches!(expr.ty.as_ref(), Some(Ty::Class { id, .. }) if matches!(id.0.as_str(), "Class" | "Module"));
        match &*expr.node {
            ExprNode::SelfRef => ctx.class_side,
            ExprNode::Var { name, .. } => ctx.class_objects.contains(name) || typed_class_value(),
            ExprNode::Const { path } => {
                if expr.decisions & crate::expr::RESOLVED_CLASS_REF != 0 { return true; }
                // A gem the tree loads natively opens it with `class` or
                // `module` in its installed source (`GemSourceConstants::
                // kinds`): i18n's `I18n::Config`, which worldwide asks
                // `method_defined?(:owned_by?)`. The read was resolved to
                // that declaration (GEM_CONSTANT_REF), so it is the class
                // object, not a value constant of the same name.
                if expr.decisions & crate::expr::GEM_CONSTANT_REF != 0
                    && self.gem_sources.as_deref().is_some_and(|sources| {
                        sources.kinds.contains_key(written_class_id(path).0.as_str().trim_start_matches("::"))
                    })
                {
                    return true;
                }
                if let Some(answer) = self.const_resolver.as_ref().and_then(|r| r.reference(expr.span, path)) {
                    if let Some(resolved) = answer {
                        return matches!(resolved, ResolvedConstant::Namespace { .. });
                    }
                }
                // An exact modeled namespace is valid without a source document.
                // A typed VALUE constant, even with the same nominal type, is not.
                let id = written_class_id(path);
                let value = if let [name] = path.as_slice() {
                    ctx.constants.get_own(name).or_else(|| self.global_constant(ctx, name))
                } else { None };
                if value.is_some() {
                    return false;
                }
                // Lowerers retype with a partial registry (no stdlib seed).
                // A Const whose computed type is the written class itself is
                // still the class object — `RecordNotFound < StandardError`
                // must stay Module#<, not Incompatible value comparison.
                self.classes().contains_key(&id)
                    || matches!(
                        expr.ty.as_ref(),
                        Some(Ty::Class { id: ty_id, .. }) if *ty_id == id
                    )
            }
            // Kernel#singleton_class answers the receiver's own singleton
            // class, one class object unique to that receiver: no subclass
            // can stand in, unlike `x.class` on a nominal type. Only for a
            // proven class/module object or an instance of a known
            // non-immediate class (`1.singleton_class` raises TypeError),
            // and not where an app method overrides it.
            ExprNode::Send { recv: Some(r), method, args, block: None, .. }
                if method.as_str() == "singleton_class" && args.is_empty() =>
            {
                let class_object = self.is_class_object(r, ctx);
                let instance = matches!(r.ty.as_ref(), Some(Ty::Class { id, .. })
                    if self.classes().contains_key(id)
                        && !matches!(id.0.as_str(), "Integer" | "Float" | "Symbol" | "Rational" | "Complex" | "NilClass" | "TrueClass" | "FalseClass"));
                (class_object || instance) && !self.owns_operator(r.ty.as_ref(), method, class_object)
            }
            // `Module.new` / `Class.new(Parent)` answer a fresh module or
            // class, unless the app overrides `new` on them.
            ExprNode::Send { recv: Some(r), method, .. }
                if method.as_str() == "new"
                    && matches!(&*r.node, ExprNode::Const { path } if matches!(path.as_slice(), [n] if matches!(n.as_str(), "Module" | "Class")))
                    && self.is_class_object(r, ctx) =>
            {
                !self.owns_operator(r.ty.as_ref(), method, true)
            }
            _ => typed_class_value(),
        }
    }

    /// `x.class` is a class object as a VALUE: `x.class < Base` is
    /// Module#<, answered on whatever class `x` has at run time. Not
    /// identity evidence: `is_class_object` still refuses Module protocol
    /// (`Probe.new.class.define_method`) on it.
    fn is_class_of_value(&self, expr: &Expr, ctx: &Ctx) -> bool {
        match &*expr.node {
            ExprNode::Send { recv: Some(r), method, args, block: None, .. }
                if method.as_str() == "class" && args.is_empty() =>
            {
                !matches!(r.ty, None | Some(Ty::Var { .. }))
                    && !self.owns_operator(r.ty.as_ref(), method, self.is_class_object(r, ctx))
            }
            _ => false,
        }
    }

    /// An instance method of an app module that a class or module body
    /// `extend`s runs as that extender's singleton method, with the class
    /// object as self: ApplicationRecord's `extend AcceleratedBelongsTo`
    /// makes `accelerated_belongs_to :shop` a class macro whose
    /// `class_eval <<~RUBY` and `alias_method` work on the model class.
    /// `extension_modules` types such a module class side only when that
    /// is its only use. Core's `Denormalize` also runs `include
    /// AcceleratedBelongsTo` from its `included do` block, so the module
    /// stays on the instance side and each Module-protocol call was
    /// refused, raising at boot in every model.
    ///
    /// A Module-only method sent to implicit self there still has one
    /// meaning: no instance answers `class_eval`, `alias_method`,
    /// `define_method` or the other names `is_module_protocol` lists
    /// unless its class defines them, so on an instance that got the
    /// module by `include` Ruby raises NoMethodError at the call. The
    /// emitted call is the same send and raises in the same way. So
    /// when no app class declares the name as an instance method (an
    /// includer's instance would answer it), the call's only answer is
    /// Module's on the extender. `include?` stays out, because Array,
    /// String, Hash and Set answer it. A module nothing extends keeps the
    /// refusal: there, self is never a class.
    ///
    /// The module's code types with the module as self, or, when one
    /// class includes it (`App::sole_includer_of_modules`), with that
    /// class's instance. The second case is the module's own instance
    /// method when the call sits in a file that declares the module, the
    /// enclosing `def` is one of the module's instance methods, and the
    /// typed class mixes the module in.
    fn self_runs_as_extender(&self, method: &Symbol, ctx: &Ctx, span: crate::span::Span) -> bool {
        if ctx.class_side || method.as_str() == "include?" { return false; }
        let Some(Ty::Class { id: self_id, .. }) = ctx.self_ty.as_ref() else { return false };
        let app_module = |id: &ClassId| self.classes().get(id).is_some_and(|info| info.is_module && info.app_declared);
        let modules: Vec<ClassId> = if app_module(self_id) {
            vec![self_id.clone()]
        } else {
            let (Some((def, false)), Some(resolver)) = (ctx.enclosing_def.as_ref(), self.const_resolver.as_ref()) else {
                return false;
            };
            resolver.receivers_declared_in(span.file)
                .map(|name| ClassId(Symbol::from(name)))
                .filter(|id| {
                    app_module(id)
                        && self.classes().get(id).is_some_and(|info| info.has_instance_method(def))
                        && self.mixes_in(self_id, id)
                })
                .collect()
        };
        if modules.is_empty() {
            return false;
        }
        // A scan of the whole registry, noted as `instance_method_definers`
        // notes its own: the class set and each visited entry.
        if let Some(deps) = self.deps {
            deps.note(DepKey::Class(&class_set_key()));
        }
        let extended = self.classes.iter().any(|(id, info)| {
            if let Some(deps) = self.deps {
                deps.note(DepKey::Class(id));
            }
            modules.iter().any(|module| info.extends.contains(module))
        });
        extended
            && !self.classes().instance_method_definers(method)
                .any(|id| self.classes.get(id).is_some_and(|info| info.app_declared))
    }

    /// Whether `module` is among `class`'s mixins: its includes, theirs,
    /// and its superclasses'.
    fn mixes_in(&self, class: &ClassId, module: &ClassId) -> bool {
        let mut stack = vec![class];
        let mut seen = crate::det::HashSet::default();
        while let Some(id) = stack.pop() {
            if !seen.insert(id) {
                continue;
            }
            let Some(info) = self.classes().get(id) else { continue };
            if info.includes().contains(module) {
                return true;
            }
            stack.extend(info.includes().iter());
            stack.extend(info.parent());
        }
        false
    }

    /// An instance method of a class that descends from `Module` (Core's
    /// `class MethodTimingRecorder < Module`) runs with a module as self:
    /// every instance of such a class is a module, whichever subclass.
    fn self_is_module_instance(&self, ctx: &Ctx) -> bool {
        if ctx.class_side { return false; }
        let Some(Ty::Class { id, .. }) = ctx.self_ty.as_ref() else { return false };
        let mut current = self.classes().get(id).and_then(|c| c.parent_id().cloned());
        for _ in 0..32 {
            let Some(id) = current else { break };
            if matches!(id.0.as_str(), "Module" | "Class") { return true; }
            current = self.classes().get(&id).and_then(|c| c.parent_id().cloned());
        }
        false
    }

    // `x.class` is not a class reference to `is_class_object`, yet types as the same flat `Ty::Class` as an instance.
    fn is_instance(&self, expr: &Expr, ctx: &Ctx) -> bool {
        !self.is_class_object(expr, ctx)
            && !matches!(&*expr.node, ExprNode::Send { method, args, .. } if method.as_str() == "class" && args.is_empty())
    }

    fn is_module_callback(&self, recv_ty: Option<&Ty>, method: &Symbol) -> bool {
        matches!(method.as_str(), "included" | "prepended" | "append_features" | "prepend_features")
            && matches!(recv_ty, Some(Ty::Class { id, .. }) if self.classes().get(id).is_some_and(|c| c.is_module && c.has_class_method(method)))
    }

    /// Whether the app emits the PostgreSQL CRuby tree, whose
    /// `ActiveRecord::Base` [`runtime_base`] reads: the registry seeds
    /// `ActiveRecord::DatabaseConfigurations` exactly then
    /// (`registry::ar::carries_database_configurations`).
    fn carries_postgresql_base(&self) -> bool {
        self.classes().contains_key(&ClassId(Symbol::from("ActiveRecord::DatabaseConfigurations")))
    }

    fn owns_operator(&self, ty: Option<&Ty>, method: &Symbol, class_object: bool) -> bool {
        match ty {
            Some(Ty::Union { variants }) => !variants.is_empty()
                && variants.iter().all(|ty| self.owns_operator(Some(ty), method, class_object)),
            Some(Ty::Class { id, .. }) => {
                let mut current = Some(id);
                // Comparable defines the four orderings on `<=>`: an app
                // value class, or a vendored one (`Gem::Version`) the
                // registry does not model.
                let ordering = !class_object && matches!(method.as_str(), "<" | "<=" | ">" | ">=");
                for _ in 0..32 {
                    let Some(id) = current else { break };
                    let Some(class) = self.classes().get(id) else {
                        return ordering && crate::stdlib_rbs::declares_include(id.0.as_str(), "Comparable");
                    };
                    let own = if class_object { class.has_class_method(method) } else { class.has_instance_method(method) };
                    if own { return true; }
                    if !class_object && class.includes.iter().any(|m| self.lookup_in_module(m, method).is_some()) { return true; }
                    if ordering && class.includes.iter().any(|m| m.0.as_str() == "Comparable") { return true; }
                    current = class.parent_id();
                }
                false
            }
            _ => false,
        }
    }

    /// Whether `ty` may answer `method` with a definition the registry does
    /// not show, so a binop on it is undecidable rather than a mismatch.
    /// A class object always has `Module#<` and its siblings; anything
    /// [`Self::owns_operator`] proves (an own or mixed-in definition, or
    /// `Comparable`'s orderings, app or vendored) answers it; and so may
    /// a receiver whose class, an ancestor or a mixin is not modeled (a
    /// gem the app does not ingest, a sig-only name). Only a modeled
    /// class lacking the operator through its whole known ancestry is
    /// proven to raise.
    fn may_own_operator(&self, ty: Option<&Ty>, method: &Symbol, class_object: bool) -> bool {
        if class_object && matches!(method.as_str(), "<" | "<=" | ">" | ">=") {
            return true;
        }
        match ty {
            Some(Ty::Union { variants }) => !variants.is_empty()
                && variants.iter().all(|ty| self.may_own_operator(Some(ty), method, class_object)),
            Some(Ty::Class { id, .. }) => {
                if self.owns_operator(ty, method, class_object) {
                    return true;
                }
                let mut current = Some(id);
                for _ in 0..32 {
                    let Some(id) = current else { return false };
                    let Some(class) = self.classes().get(id) else {
                        return !matches!(id.0.as_str(), "Object" | "BasicObject");
                    };
                    if !class_object && class.includes.iter().any(|m| !self.classes().contains_key(m)) {
                        return true;
                    }
                    current = class.parent_id();
                }
                true
            }
            _ => false,
        }
    }

    fn compute(&self, expr: &mut Expr, ctx: &Ctx) -> Ty {
        let expr_span = expr.span;
        match &mut *expr.node {
            ExprNode::Lit { value } => lit_ty(value),

            ExprNode::Const { path } => {
                // A later typing pass may resolve a value constant whose
                // owner was unknown in an earlier constant fixpoint round.
                expr.diagnostic = None;
                expr.decisions &= !(crate::expr::GEM_CONSTANT_REF
                    | crate::expr::STDLIB_CONSTANT_REF
                    | crate::expr::DECLARED_TYPE_RUNTIME_CONSTANT
                    | crate::expr::RUNTIME_VALUE_REF);
                let generated = expr_span.file.0 == 0
                    || expr.decisions & crate::expr::GENERATED_CONST_REF != 0;
                // A real FileId outside the snapshot is an indexing
                // defect, not a generated expression or an ERB fallback.
                if !generated && self.const_resolver.as_ref()
                    .is_some_and(|resolver| !resolver.has_registered_source(expr_span.file))
                {
                    expr.diagnostic = Some(crate::diagnostic::DiagnosticKind::Unsupported {
                        target: None,
                        construct: Symbol::from("constant"),
                        detail: format!("{}: missing source document for FileId({})",
                            written_class_id(path).0, expr_span.file.0),
                    });
                    return unknown();
                }
                // Never search by suffix or borrow another scope's
                // same-named declaration: only the exact written class.
                let exact_modeled_class = |path: &[Symbol]| {
                    let id = written_class_id(path);
                    if let Some((name, owner)) = path.split_last() {
                        if !owner.is_empty() {
                            // Only external declarations carry `constants`:
                            // gem RBI/RBS and the modeled stdlib (`Process`).
                            if let Some(info) = self.classes().get(&written_class_id(owner)) {
                                if let Some(ty) = info.constants.get(name) { return ty.clone(); }
                            }
                        }
                    }
                    if self.classes().contains_key(&id) {
                        Ty::Class { id, args: vec![] }
                    } else {
                        unknown()
                    }
                };
                let indexed_source = !expr_span.is_synthetic()
                    && self
                        .const_resolver
                        .as_ref()
                        .is_some_and(|resolver| resolver.has_source_file(expr_span.file));
                let source_reference = self
                    .const_resolver
                    .as_ref()
                    .filter(|_| !generated)
                    .and_then(|resolver| resolver.reference(expr_span, path));
                let app_value = matches!(source_reference, Some(Some(ResolvedConstant::Value { stdlib: false, .. })));
                let unindexed_singleton = indexed_source
                    && source_reference.is_none()
                    && self.const_resolver.as_ref().is_some_and(|resolver| resolver.in_unindexed_singleton(expr_span));
                let ty = match source_reference {
                    Some(Some(ResolvedConstant::Namespace { class, runtime, stdlib, alias })) => {
                        // A modeled class keeps the name it is modeled
                        // under (`Mutex`, not its target `Thread::Mutex`).
                        let id = match alias {
                            Some(alias) if self.classes().contains_key(alias.as_ref()) => alias.as_ref().clone(),
                            _ => class.as_ref().clone(),
                        };
                        // A source module that only nests other
                        // declarations (`module AuditTrail` holding
                        // `Trail` and `Step`) models no class of its own,
                        // yet is the module the read returns: emitted,
                        // its modeled nested classes open it. One whose
                        // nested declarations are all unmodeled (a class
                        // an ingest gap dropped, with its contents) stays
                        // unknown.
                        let namespace_only = !*stdlib
                            && self.const_resolver.as_ref().is_some_and(|r| {
                                r.nested_source_namespaces(id.0.as_str())
                                    .iter()
                                    .any(|nested| self.classes().contains_key(&ClassId(Symbol::from(nested.as_ref()))))
                            });
                        if self.classes().contains_key(&id) || *runtime || namespace_only {
                            if *stdlib && !self.classes().contains_key(&id) {
                                expr.decisions |= crate::expr::STDLIB_CONSTANT_REF;
                            }
                            expr.decisions |= crate::expr::RESOLVED_CLASS_REF;
                            qualify_resolved_path(path, &id);
                            Ty::Class { id, args: vec![] }
                        } else {
                            unknown()
                        }
                    }
                    // A value only Ruby's signatures declare: a modeled
                    // constant (`Process::CLOCK_MONOTONIC`) keeps its
                    // model; otherwise the declared type, gradual when
                    // the signature's type does not map. A class modeled
                    // with a constant table (the four clock ids Spinel
                    // lowers) does not close the module: Ruby's
                    // signatures still declare the rest of it
                    // (`Process::WNOHANG: Integer`), which the CRuby and
                    // JRuby trees read natively and `project` refuses by
                    // name elsewhere.
                    Some(Some(ResolvedConstant::Value { stdlib: true, runtime, .. })) => {
                        match exact_modeled_class(path) {
                            Ty::Var { .. } => {
                                expr.decisions |= crate::expr::STDLIB_CONSTANT_REF;
                                runtime.as_ref().map_or(Ty::Untyped, |ty| (**ty).clone())
                            }
                            modeled => modeled,
                        }
                    }
                    Some(Some(ResolvedConstant::Value { declaration, name, runtime, singleton, .. })) => {
                        // Concern splices retain the resolved value owner.
                        // Test-helper splice lifts a foreign module's
                        // constant onto the test class as a bare name
                        // (`DnsTestHelper::WEB_PUSH_PUBLIC_TEST_IP` →
                        // `WEB_PUSH_PUBLIC_TEST_IP`) and never emits the
                        // helper module — re-qualifying that bare path is a
                        // NameError at load. Leave bare paths bare when the
                        // class owns the leaf and the declaration's owner
                        // is foreign. Do not strip an already-qualified
                        // path (`FactoryModel::Result`) just because the
                        // leaf name also exists locally — that collision
                        // is a different constant (`data_factory_constants`).
                        let leaf = path.last();
                        let locally_owned = leaf
                            .is_some_and(|n| ctx.constants.get_own(n).is_some());
                        let enclosing = match &ctx.self_ty {
                            Some(Ty::Class { id, .. }) => Some(id.0.as_str()),
                            _ => None,
                        };
                        let decl_owner = name.0.as_str().rsplit_once("::").map(|(o, _)| o);
                        let keep_bare_splice = path.len() == 1
                            && locally_owned
                            && enclosing
                                .zip(decl_owner)
                                .is_some_and(|(enc, owner)| enc != owner);
                        if !keep_bare_splice {
                            qualify_resolved_path(path, name);
                            // A class ingest built from it (`X =
                            // Struct.new do … end`, `factory_class`) is
                            // emitted hoisted, as its own class file.
                            if *singleton && !self.classes().contains_key(name.as_ref()) {
                                expr.decisions |= crate::expr::SINGLETON_CONSTANT_REF;
                            }
                        }
                        // A runtime port's own value (`Rails::VERSION::STRING`)
                        // runs only where the finished tree loads the port.
                        let typed = self.typed_constant(declaration).cloned();
                        if typed.is_none() && runtime.is_some() && !keep_bare_splice {
                            expr.decisions |= crate::expr::RUNTIME_VALUE_REF;
                        }
                        let value = typed.or_else(|| runtime.as_ref().map(|ty| (**ty).clone()));
                        match value {
                            Some(ty) => ty,
                            // `Err = Class.new(StandardError)` is a constant to the source
                            // index and a class to ingest (`library_class::
                            // class_new_declaration`), which leaves no constant value
                            // behind: the exact declared name is that class.
                            None if self.classes().contains_key(name.as_ref()) => {
                                expr.decisions |= crate::expr::RESOLVED_CLASS_REF;
                                Ty::Class { id: name.as_ref().clone(), args: vec![] }
                            }
                            None => unknown(),
                        }
                    }
                    // An unresolved source reference may still name an
                    // exact modeled external class (for example Time), or
                    // a declaration the source index cannot see, found as
                    // Ruby finds it: in an enclosing scope (`KeyUtilities`
                    // inside a reopened `TokenProtocol::Crypto`, whose
                    // gem RBI declares it), then on an ancestor
                    // (`REQUEST_TIME` in a subclass of shopify_metrics'
                    // `RequestMetrics`, `Result` beside `include
                    // Shopify::Adt`). Read at its full name.
                    Some(None) => {
                        let lexical = self.lexical_external_constant(expr_span, path);
                        let extended = if lexical.is_none() { self.extended_constant(expr_span, path) } else { None };
                        // A gem's private constant (google-protobuf's
                        // `private_constant :AbstractMessage`) is read
                        // only as written: qualified, it is NameError.
                        let private = |full: &ClassId| self.gem_sources.as_deref().is_some_and(|s| s.private.contains(full.0.as_str()));
                        if let Some((full, declaration)) = extended {
                            let full_path: Vec<Symbol> = full.0.as_str().split("::").map(Symbol::from).collect();
                            let modeled_at = if private(&full) { full_path.as_slice() } else {
                                qualify_resolved_path(path, &full);
                                path.as_slice()
                            };
                            match declaration {
                                Some(declaration) => self.typed_constant(&declaration).cloned().unwrap_or_else(unknown),
                                None => exact_modeled_class(modeled_at),
                            }
                        } else {
                        let full = lexical.or_else(|| {
                            let written = path.iter().map(Symbol::as_str).collect::<Vec<_>>().join("::");
                            self.inherited_constant_owner(ctx, path.first()?)
                                .map(|owner| ClassId(Symbol::from(format!("{}::{written}", owner.0).as_str())))
                        });
                        match full {
                            Some(full) if private(&full) => {
                                let full_path: Vec<Symbol> = full.0.as_str().split("::").map(Symbol::from).collect();
                                exact_modeled_class(&full_path)
                            }
                            Some(full) => {
                                qualify_resolved_path(path, &full);
                                exact_modeled_class(path)
                            }
                            None => match self.aliased_constant_path(expr_span, path) {
                                // `DECISION::DROP`, where the enclosing
                                // class wrote `DECISION =
                                // OpenTelemetry::SDK::Trace::Samplers::Decision`
                                // (opentelemetry-shopify's verbosity
                                // sampler): Ruby reads `DROP` in the
                                // aliased module, which only the gem's RBI
                                // declares. The read stays as written.
                                Some(target) => {
                                    let ty = exact_modeled_class(&target);
                                    if matches!(ty, Ty::Var { .. }) { exact_modeled_class(path) } else { ty }
                                }
                                None => exact_modeled_class(path),
                            },
                        }
                        }
                    }
                    // A borrowed span is not evidence of a written constant
                    // only when the producer marked its generated origin.
                    // A stored class-body local (ingest `BodyLocal`) is
                    // read by its unique hidden name, which only the
                    // app-wide registry answers.
                    None if indexed_source && generated => match path.as_slice() {
                        [name] if name.as_str().starts_with(crate::ingest::library_class::BODY_LOCAL_CONSTANT_PREFIX) => {
                            self.global_constant(ctx, name).cloned().unwrap_or_else(unknown)
                        }
                        _ => exact_modeled_class(path),
                    },
                    None if indexed_source && !unindexed_singleton => unknown(),
                    // Views and generated IR have no Rubydex answer. A
                    // bare name can use the app's bare-name values. A
                    // qualified name `A::B` names its owner, so only the
                    // value declared at that full name answers it.
                    //
                    // Nor does a `class << self` Rubydex left unindexed
                    // (`ConstResolver::in_unindexed_singleton`). Its cref
                    // is the singleton, whose constants ingest carries on
                    // the class (`factory_class::factory_class_body`), so
                    // a bare name is that class's constant first, read at
                    // its full name, as the emitted body (a block, whose
                    // own cref is the scope around it) must write it.
                    None => {
                        let own_singleton = match (path.as_slice(), unindexed_singleton) {
                            ([name], true) => match ctx.lexical_self.as_ref().or(ctx.self_ty.as_ref()) {
                                Some(Ty::Class { id, .. }) => {
                                    let full = ClassId(Symbol::from(format!("{}::{name}", id.0).as_str()));
                                    self.typed_constant(&declaration_id_from_lookup_name(full.0.as_str()))
                                        .cloned()
                                        .map(|ty| (full, ty))
                                }
                                _ => None,
                            },
                            _ => None,
                        };
                        let value = if let Some((full, ty)) = own_singleton {
                            qualify_resolved_path(path, &full);
                            Some(ty)
                        } else if unindexed_singleton {
                            // Then the top level, at the exact written name:
                            // no bare-name value of another scope.
                            let ty = exact_modeled_class(path);
                            (!matches!(ty, Ty::Var { .. })).then_some(ty).or_else(|| {
                                self.typed_constant(&declaration_id_from_lookup_name(written_class_id(path).0.as_str()))
                                    .cloned()
                            })
                        } else if let [name] = path.as_slice() {
                            ctx.constants.get_own(name).cloned()
                                .or_else(|| {
                                    let ty = exact_modeled_class(path);
                                    (!matches!(ty, Ty::Var { .. })).then_some(ty)
                                })
                                .or_else(|| self.global_constant(ctx, name).cloned())
                        } else {
                            let name = written_class_id(path);
                            self.typed_constant(&declaration_id_from_lookup_name(name.0.as_str()))
                                .cloned()
                        };
                        // Otherwise retain the written class path, but
                        // never guess another class by its suffix. Not in
                        // source Rubydex skipped: there it is unknown, and
                        // reported.
                        value.unwrap_or_else(|| {
                            if unindexed_singleton {
                                unknown()
                            } else {
                                Ty::Class { id: written_class_id(path), args: vec![] }
                            }
                        })
                    }
                };
                // A constant nothing indexes that a gem the CRuby tree
                // loads natively declares in its installed source
                // (`GemBoundary::source_constants`): the read runs as the
                // gem's, gradually typed, and other trees refuse it
                // (`GEM_CONSTANT_REF`).
                let ty = match (&ty, self.gem_sources.as_deref()) {
                    // A gem's runtime port declares it exactly (globalid's
                    // `URI::GID` in runtime/global_id.rb): the read is
                    // that gem's, and `project` checks each tree for the
                    // gem or the port's file, before any namespace a
                    // native gem fills under computed names (uri's
                    // `const_set` in URI) is taken to provide it.
                    (Ty::Var { .. }, _)
                        if indexed_source
                            && !generated
                            && crate::project::gem_port_declaring(written_class_id(path).0.as_str()).is_some() =>
                    {
                        expr.decisions |= crate::expr::GEM_CONSTANT_REF;
                        Ty::Untyped
                    }
                    (Ty::Var { .. }, Some(sources)) if indexed_source && !generated => {
                        let written = written_class_id(path);
                        let scope = match ctx.lexical_self.as_ref().or(ctx.self_ty.as_ref()) {
                            Some(Ty::Class { id, .. }) => id.0.as_str(),
                            _ => "",
                        };
                        match sources.provider(written.0.as_str(), scope) {
                            Some(crate::gem_boundary::SourceProvider::Declared { name, .. }) => {
                                // A private constant (`private_constant
                                // :AbstractMessage` in `Google::Protobuf`)
                                // reached from a lexical scope is readable
                                // only so: qualified, it raises NameError.
                                if !sources.private.contains(name.as_str()) {
                                    qualify_resolved_path(path, &ClassId(Symbol::from(name.as_str())));
                                }
                                expr.decisions |= crate::expr::GEM_CONSTANT_REF;
                                Ty::Untyped
                            }
                            // Namespace ownership: the read runs on CRuby,
                            // and stays named with how the gem was taken
                            // to provide it.
                            Some(provider) => {
                                expr.decisions |= crate::expr::DECLARED_TYPE_RUNTIME_CONSTANT;
                                expr.diagnostic = Some(crate::diagnostic::DiagnosticKind::Unsupported {
                                    target: None,
                                    construct: Symbol::from("constant"),
                                    detail: format!(
                                        "{} has no indexed declaration; {}; the CRuby tree loads {} natively, so the read runs there",
                                        written.0,
                                        provider.describe(written.0.as_str(), sources),
                                        provider.gem(),
                                    ),
                                });
                                Ty::Untyped
                            }
                            None => ty,
                        }
                    }
                    _ => ty,
                };
                if indexed_source && matches!(ty, Ty::Var { .. }) {
                    // A stored class-body local is reported by the name the
                    // source wrote, not the hidden constant's.
                    let local = match path.as_slice() {
                        [name] => crate::ingest::library_class::stored_body_local_name(name.as_str()),
                        _ => None,
                    };
                    expr.diagnostic = Some(crate::diagnostic::DiagnosticKind::Unsupported {
                        target: None,
                        construct: Symbol::from("constant"),
                        detail: match local {
                            Some(local) => format!("class-body local `{local}` holds a value with no inferred type"),
                            // A Rails class no tree carries says why
                            // (`registry::refused_constants`).
                            None => {
                                let name = written_class_id(path).0.as_str().to_string();
                                match crate::analyze::registry::refused_constants::refusal(&name) {
                                    Some(reason) => format!("{name} — {reason}"),
                                    None => name,
                                }
                            }
                        },
                    });
                }
                // A gem's RBI types the constant; only a tree that loads
                // the gem can run it.
                if expr.diagnostic.is_none() && !app_value && expr.decisions & crate::expr::GEM_CONSTANT_REF == 0 {
                    if let Some((gem, loaded)) = self.declaring_gem(path) {
                        let name = written_class_id(path);
                        if loaded || crate::project::gem_port_file(&gem, name.0.as_str()).is_some() {
                            // The gem, or its runtime port where the
                            // tree carries the port's file (`project`
                            // checks each tree).
                            expr.decisions |= crate::expr::GEM_CONSTANT_REF;
                        } else if let Some(ports) = crate::project::gem_port_files(&gem) {
                            expr.diagnostic = Some(crate::diagnostic::DiagnosticKind::Unsupported {
                                target: None,
                                construct: Symbol::from("constant"),
                                detail: format!(
                                    "{} is declared by the {gem} gem, which runs as its runtime port ({}), and the port does not declare it",
                                    name.0,
                                    ports.join(", ")
                                ),
                            });
                        } else if let Some(reason) = crate::project::gem_requires_rails(&gem) {
                            // Named apart from "no tree loads it": the
                            // gem's source has to be compiled as app code.
                            expr.diagnostic = Some(crate::diagnostic::DiagnosticKind::Unsupported {
                                target: None,
                                construct: Symbol::from("gem_requires_rails"),
                                detail: format!(
                                    "{} is declared by the {gem} gem, whose load closure requires Rails ({reason}), so no emitted tree loads it natively",
                                    written_class_id(path).0
                                ),
                            });
                        } else {
                            expr.diagnostic = Some(crate::diagnostic::DiagnosticKind::Unsupported {
                                target: None,
                                construct: Symbol::from("constant"),
                                detail: format!(
                                    "{} is declared by the {gem} gem, which no emitted tree loads",
                                    written_class_id(path).0
                                ),
                            });
                        }
                    }
                }
                ty
            }

            ExprNode::Var { name, .. } => ctx
                .local_bindings
                .get(name)
                .cloned()
                .unwrap_or_else(unknown),

            ExprNode::Ivar { name } => {
                ctx.ivar_bindings.get(name).cloned().unwrap_or_else(unknown)
            }

            ExprNode::SelfRef => ctx.self_ty.clone().unwrap_or_else(unknown),

            ExprNode::Return { value } => {
                self.analyze_expr(value, ctx);
                // `return x` diverges at the source position — control
                // jumps out of the method, so the expression itself
                // produces no value here. Type as `Bottom` so it drops
                // out of joins (`if cond then return x else y end`
                // types as `typeof(y)`, not `typeof(x) | typeof(y)`).
                // The value's own type was already captured via
                // analyze_expr above and contributes to the method's
                // declared return-type reconciliation elsewhere.
                Ty::Bottom
            }

            ExprNode::Super { args, block } => {
                if let Some(args) = args {
                    for a in args.iter_mut() {
                        self.analyze_expr(a, ctx);
                    }
                }
                // The parent's yield signature is not known here, so a
                // literal block's params bind `Untyped`, as for any call
                // on an untyped receiver.
                if let Some(b) = block {
                    let method = crate::ident::Symbol::from("super");
                    let block_ctx = self.block_ctx_for(
                        ctx, Some(&Ty::Untyped), &method, args.as_deref().unwrap_or(&[]), false, b);
                    self.analyze_expr(b, &block_ctx);
                }
                // `super` invokes the parent's same-name method. In an
                // instance-side `initialize` that method is an
                // `initialize`, whose value `new` discards: RBS declares
                // every one `-> void` (core's Object and Exception
                // family, and each runtime signature), and a declared
                // `void` reads as `Nil` (`rbs::ty_from_node`). Any other
                // method's parent is not looked up in the registry yet,
                // so its `super` stays `Untyped`: an intentional jump
                // out of the typed envelope, distinct from `Var`
                // (analyzer gap) so the gradual diagnostic shape is
                // right.
                match &ctx.enclosing_def {
                    Some((name, false)) if name.as_str() == "initialize" => Ty::Nil,
                    _ => Ty::Untyped,
                }
            }

            ExprNode::BeginRescue { body, rescues, else_branch, ensure, .. } => {
                self.analyze_expr(body, ctx);
                for rc in rescues.iter_mut() {
                    for c in rc.classes.iter_mut() {
                        self.analyze_expr(c, ctx);
                    }
                    if let Some(name) = &rc.binding {
                        // `rescue A, B => e` binds the union: `e` is one of them. A class
                        // that failed constant resolution has already produced a
                        // blocking diagnostic. Its binding is unknown, rather than
                        // StandardError: inventing that class causes unrelated method
                        // errors and hides the original resolution failure in cascades.
                        let mut rescued: Option<Ty> = None;
                        for c in rc.classes.iter() {
                            match &c.ty {
                                Some(ty @ Ty::Class { .. }) => {
                                    rescued = Some(match rescued.take() {
                                        Some(prev) => union_of(prev, ty.clone()),
                                        None => ty.clone(),
                                    });
                                }
                                _ => {
                                    rescued = Some(Ty::Untyped);
                                    break;
                                }
                            }
                        }
                        let mut inner = ctx.clone();
                        inner.class_objects.remove(name);
                        inner.local_bindings.insert(
                            name.clone(),
                            rescued.unwrap_or_else(|| Ty::Class {
                                id: crate::ident::ClassId(Symbol::from("StandardError")),
                                args: vec![],
                            }),
                        );
                        self.analyze_expr(&mut rc.body, &inner);
                        continue;
                    }
                    self.analyze_expr(&mut rc.body, ctx);
                }
                if let Some(e) = else_branch {
                    self.analyze_expr(e, ctx);
                }
                if let Some(e) = ensure {
                    self.analyze_expr(e, ctx);
                }
                // Ruby's value here is the ELSE branch when there is
                // one (it runs on the no-exception path and its value
                // wins over the begin body's), otherwise the begin
                // body's — unioned with every rescue body, which is the
                // value on the handled path. `ensure` never contributes
                // a value.
                //
                // Dropping the rescue arms — what this did before —
                // makes `uri.to_s … rescue nil` infer `String` where it
                // answers `String?`, and the emitted RBS then
                // OVER-CLAIMS. campfire's
                // `Metadata.replace_twitter_domain_for_opengraph_support`
                // is exactly that shape: signed `-> String`, returning
                // nil from `rescue URI::InvalidURIError`, and the C
                // build stopped on `returning 'sp_RbVal' from a
                // function with incompatible result type 'const char *'`.
                //
                // `Bottom` arms are SKIPPED rather than unioned: a
                // `rescue` that only re-raises diverges and must not
                // drag the whole expression to Bottom. Same rule
                // `effective_return_ty` applies to a diverging tail.
                let mut arms: Vec<Ty> = Vec::new();
                let primary = else_branch
                    .as_ref()
                    .and_then(|e| e.ty.clone())
                    .or_else(|| body.ty.clone());
                if let Some(t) = primary {
                    if !matches!(t, Ty::Bottom) {
                        arms.push(t);
                    }
                }
                for rc in rescues.iter() {
                    if let Some(t) = &rc.body.ty {
                        if !matches!(t, Ty::Bottom) {
                            arms.push(t.clone());
                        }
                    }
                }
                if arms.is_empty() { unknown() } else { union_many(arms) }
            }

            ExprNode::Hash { entries, .. } => {
                // Empty literals can carry a pre-stamped expected type
                // (set by `Assign`'s `propagate_expected_to_empty_container`
                // when the LHS is an Ivar/Var with a declared Hash type).
                // Honor it instead of falling back to `Hash<Var, Var>`
                // — strict targets need the concrete shape, and the
                // surrounding declaration already constrains the type.
                if entries.is_empty() {
                    if let Some(t @ Ty::Hash { .. }) = expr.ty.as_ref() {
                        return t.clone();
                    }
                }
                let mut key_ty: Option<Ty> = None;
                let mut value_ty: Option<Ty> = None;
                for (k, v) in entries.iter_mut() {
                    let kt = self.analyze_expr(k, ctx);
                    let vt = self.analyze_expr(v, ctx);
                    key_ty = Some(match key_ty.take() {
                        Some(prev) => union_of(prev, kt),
                        None => kt,
                    });
                    value_ty = Some(match value_ty.take() {
                        Some(prev) => union_of(prev, vt),
                        None => vt,
                    });
                }
                Ty::Hash {
                    key: Box::new(key_ty.unwrap_or_else(unknown)),
                    value: Box::new(value_ty.unwrap_or_else(unknown)),
                }
            }

            ExprNode::Array { elements, .. } => {
                // Mirror the empty-Hash branch above: a pre-stamped
                // `Ty::Array<T>` from `with_ty` (or from
                // `propagate_expected_to_empty_container`) takes
                // precedence over the `Var`-defaulted inference.
                if elements.is_empty() {
                    if let Some(t @ Ty::Array { .. }) = expr.ty.as_ref() {
                        return t.clone();
                    }
                }
                let mut elem_ty: Option<Ty> = None;
                for e in elements.iter_mut() {
                    let et = self.analyze_expr(e, ctx);
                    // `[*a, b]` holds `a`'s elements, not `a`.
                    let et = if matches!(&*e.node, ExprNode::Splat { .. }) {
                        match splat_elements(et) {
                            Some(et) => et,
                            None => continue,
                        }
                    } else {
                        et
                    };
                    elem_ty = Some(match elem_ty.take() {
                        Some(prev) => union_of(prev, et),
                        None => et,
                    });
                }
                Ty::Array { elem: Box::new(elem_ty.unwrap_or_else(unknown)) }
            }

            ExprNode::StringInterp { parts } => {
                for p in parts.iter_mut() {
                    if let crate::expr::InterpPart::Expr { expr } = p {
                        self.analyze_expr(expr, ctx);
                    }
                }
                Ty::Str
            }

            ExprNode::BoolOp { left, op, right, .. } => {
                let lt = self.analyze_expr(left, ctx);
                // Short-circuit narrowing: when the right arm runs,
                // the left arm has already produced a value that
                // determined the short-circuit. For `&&` the right
                // arm runs only if left was truthy; for `||` only if
                // left was falsy. If the left arm was a recognized
                // narrowing predicate (`x.is_a?(T)`, `x.nil?`, etc.),
                // type the right arm under the corresponding
                // narrowed Ctx so subsequent reads of the same var
                // see the refined type. Mirrors how the `If` arm
                // threads narrowing into its then/else branches.
                // Var assignments inside `left` (the canonical case is
                // `(x = find_by(...)) && x.foo`) need to flow into
                // `right`'s scope. Without this, `x` reads as Var
                // because the Seq-level Assign-to-Var handler only
                // catches statement-position assignments, not nested
                // ones inside expressions. For `&&` the assignment
                // executed (left was truthy); for `||` it executed
                // because left was falsy — in either case the binding
                // is observable on the right side. Seeded BEFORE the
                // narrowing so that a left that is itself the
                // assignment narrows what it just bound (`x` is `T`
                // on the right of `(x = find_by(…)) &&`, `T?` after
                // `||`).
                let mut seeded = ctx.clone();
                collect_var_assignments_into(left, &mut seeded.local_bindings);
                self.propagate_match_bindings(left, &mut seeded, true);
                let pred = narrowing::extract_narrowing_with(left, &|n| self.self_attribute_ty(n, ctx));
                let right_ctx = match (&pred, &*op) {
                    (Some(p), crate::expr::BoolOpKind::And) => {
                        narrowing::apply_narrowing(&seeded, p, true)
                    }
                    (Some(p), crate::expr::BoolOpKind::Or) => {
                        narrowing::apply_narrowing(&seeded, p, false)
                    }
                    _ => seeded,
                };
                let rt = self.analyze_expr(right, &right_ctx);
                // Short-circuit: the result is either left (if it
                // determined the short-circuit) or right — a union
                // of the two operand types. For `||`, Nil never wins
                // — if left was Nil/false, control moves to right —
                // so peel Nil from the left side before unioning.
                // Closes the common `Option<T> || default_T` shape
                // (BoolOp::Or in `content_for_get(:title) || "Real
                // Blog"`) where the result type should be `T`, not
                // `Union<Option<T>, T>`. The lt-narrowed form lets
                // the rust emit's Family 4 (String → &str) fire.
                let lt_peeled = if matches!(op, crate::expr::BoolOpKind::Or) {
                    // Two `||` shapes are not unions at all, and saying
                    // union anyway is what cost campfire's spinel BINARY
                    // a link. `ActionText::ContentHelper.allowed_attributes
                    // || (…).to_a` has a left arm that can only be one
                    // thing; the union dragged the right arm's
                    // `Array[untyped]` into a list declared
                    // `Array[String]` further down.
                    //
                    //   * a left that can NEVER be falsy short-circuits
                    //     every time, so the result is exactly its type.
                    //     Ruby's falsy set is `nil` and `false` and
                    //     nothing else, so this reads off the type.
                    //   * a left that is exactly `Nil` never wins, so the
                    //     result is exactly the RIGHT arm's type —
                    //     `remove_nil` leaves a bare `Nil` alone (it peels
                    //     from a union), and the union then re-added the
                    //     nil the operator had just ruled out.
                    if never_falsy(&lt) {
                        return lt;
                    }
                    if matches!(lt, Ty::Nil) {
                        return rt;
                    }
                    let peeled = narrowing::remove_nil(&lt);
                    // `(@registry || {})`: an empty literal holds no
                    // element, so it is a value of whatever Hash (or
                    // Array) type the left arm has. Joining its own
                    // `Hash[_, _]` instead left every element a union
                    // with an unknown, and `.each do |n, (a, b)|` could
                    // not destructure the tuple values. Only a literal
                    // written empty: an unknown container of the same
                    // spine may hold anything.
                    if is_empty_container_literal(right) && same_container_spine(&peeled, &rt) {
                        right.ty = Some(peeled.clone());
                        return peeled;
                    }
                    peeled
                } else {
                    // `a && b` answers `a` only when `a` was falsy, so only
                    // the arms of `a` that CAN be falsy belong to the result:
                    // `params[:id] && params[:id].to_i` (which is how `&.`
                    // reads) is `Integer | nil`, not `String | Integer | nil`
                    // -- a String is never the falsy value that stopped it.
                    match falsy_part(&lt) {
                        Some(f) => f,
                        None => return rt,
                    }
                };
                union_of(lt_peeled, rt)
            }

            ExprNode::RescueModifier { expr, fallback } => {
                let et = self.analyze_expr(expr, ctx);
                let ft = self.analyze_expr(fallback, ctx);
                union_of(et, ft)
            }

            ExprNode::Let { name, value, body, .. } => {
                let v_ty = self.analyze_expr(value, ctx);
                let mut inner = ctx.clone();
                inner.local_bindings.insert(name.clone(), v_ty);
                inner.class_objects.remove(name);
                if self.is_class_object(value, ctx) { inner.class_objects.insert(name.clone()); }
                self.analyze_expr(body, &inner)
            }

            ExprNode::Lambda { params, rest_param, block_param, body, signature, .. } => {
                let mut inner = ctx.clone();
                for name in params.iter().chain(rest_param.iter()).chain(block_param.iter()) {
                    inner.class_objects.remove(name);
                }
                // `->(&blk)` binds the block the call passes (a Proc, or
                // nil), shadowing any outer local of that name.
                if let Some(name) = block_param {
                    inner.local_bindings.insert(name.clone(), Ty::Untyped);
                }
                // `|b = 1, item:, context: 1, **opts|`: what a caller passes
                // is not known here, so each is Untyped, except that a
                // non-nil literal default types its parameter. A default
                // is evaluated in the block's scope, after the parameters
                // before it are bound.
                for (name, default) in signature.optionals.iter_mut() {
                    let ty = self.analyze_expr(default, &inner);
                    let ty = block_default_ty(default, ty);
                    inner.class_objects.remove(name);
                    inner.local_bindings.insert(name.clone(), ty);
                }
                for (name, default) in signature.keywords.iter_mut() {
                    let ty = match default {
                        Some(d) => {
                            let ty = self.analyze_expr(d, &inner);
                            block_default_ty(d, ty)
                        }
                        None => Ty::Untyped,
                    };
                    inner.class_objects.remove(name);
                    inner.local_bindings.insert(name.clone(), ty);
                }
                if let Some(name) = &signature.kwrest {
                    inner.class_objects.remove(name);
                    inner.local_bindings.insert(name.clone(), Ty::Untyped);
                }
                let body_ty = self.analyze_expr(body, &inner);
                // Synthesize a `Fn` type from the body's type. Param
                // types aren't tracked here (they were seeded into the
                // outer Ctx by block_ctx_for from the receiver
                // signature); the lambda's *own* type just records the
                // body's return type so callers can walk past the
                // Lambda node without seeing Var. Effects default to
                // pure; full effect inference is future work.
                Ty::Fn {
                    params: Vec::new(),
                    block: None,
                    ret: Box::new(body_ty),
                    effects: crate::effect::EffectSet::pure(),
                }
            }

            // `def` at expression position (inside `Class.new do … end`):
            // a scope gate, so none of the enclosing locals or ivar facts
            // reach in, and `self` is the anonymous class's instance,
            // which nothing types. Params are Untyped except where a
            // non-nil literal default types one. Its value is the name.
            // The ivars it reads are that untyped instance's, so they are
            // Untyped too (Core's `fo.instance_exec do def destroyed?;
            // @destroyed; end end`), unless the body assigns one first.
            ExprNode::MethodDefExpr { def } => {
                let mut inner = ctx.clone();
                inner.local_bindings.clear();
                inner.class_objects.clear();
                inner.ivar_bindings.clear();
                let mut ivars = Vec::new();
                ivars_of_untyped_self(&def.body, &mut ivars);
                for name in ivars {
                    inner.ivar_bindings.insert(name, Ty::Untyped);
                }
                inner.self_ty = Some(Ty::Untyped);
                inner.lexical_self = None;
                inner.annotate_self_dispatch = false;
                for p in def.params.iter_mut() {
                    let ty = match p.default.as_mut() {
                        Some(d) => {
                            let ty = self.analyze_expr(d, &inner);
                            block_default_ty(d, ty)
                        }
                        None => Ty::Untyped,
                    };
                    if !p.name.as_str().is_empty() {
                        inner.local_bindings.insert(p.name.clone(), ty);
                    }
                }
                if let Some(bp) = &def.block_param {
                    inner.local_bindings.insert(bp.name.clone(), Ty::Untyped);
                }
                self.analyze_expr(&mut def.body, &inner);
                Ty::Sym
            }

            // `method(:name)` / `recv.method(:name)`. Unlike `&:sym`
            // (desugared to a real `Lambda` calling through
            // `block_ctx_for`'s elem-type binding), there is no body
            // here to give per-block-arg types to — the callee's arity
            // is a fact about `name`'s *definition*, not this call
            // site. So type it the same way ordinary dispatch types
            // any other call: resolve `name` on the receiver (explicit,
            // or `self` when elided) through the SAME registry lookup
            // `Send` uses, with no argument evidence (`&[]`) since none
            // is available yet — the block's yielded values become
            // ARGUMENTS to `name` at runtime, not evidence about ITS
            // return type. `block_ret` in the `Send` arm below reads
            // this node's type as the element type for map/select/etc.
            //
            // Known gap: `name`'s OWN param types come from its
            // inferred/declared signature (call sites elsewhere,
            // an RBS sig, …) — never from THIS site, since
            // `collect_send_sites`'s evidence-gathering walk only
            // reads `Send` nodes, and `&method(:name)` isn't one. A
            // helper referenced ONLY via `&method(:name)` therefore
            // types its params `Untyped` rather than the block's
            // element type — a gradual fallback (not an unresolved
            // `Var`, so invariant 1's zero-unresolved-type gate still
            // holds), just a weaker inference than `&:sym` gets. See
            // `tests/analyze.rs`'s `method_ref_block_arg_types_map_
            // result_by_referenced_method_return_ty` for the case this
            // does resolve (the common one — a helper also called
            // directly somewhere feeds its own param types).
            ExprNode::MethodRef { recv, name } => {
                let recv_ty = match recv {
                    Some(r) => Some(self.analyze_expr(r, ctx)),
                    None => ctx.self_ty.clone(),
                };
                self.dispatch(recv_ty.as_ref(), name, None, &[])
            }

            ExprNode::Apply { fun, args, block } => {
                self.analyze_expr(fun, ctx);
                for a in args.iter_mut() { self.analyze_expr(a, ctx); }
                if let Some(b) = block { self.analyze_expr(b, ctx); }
                unknown()
            }

            ExprNode::Defined { .. } => union_of(Ty::Str, Ty::Nil),

            ExprNode::Send { recv, method, args, block, parenthesized } => {
                if matches!(&expr.diagnostic, Some(crate::diagnostic::DiagnosticKind::Unsupported { construct, .. })
                    if matches!(construct.as_str(), "Module protocol" | "ActiveModel::Errors" | "ActiveModel::Error" | "Rails" | "ActiveRecord::Base" | "String extension" | "Hash extension" | "not_nil!" | "Object extension" | "Module callback argument"))
                { expr.diagnostic = None; }
                expr.decisions &= !crate::expr::RESOLVED_DATA_FACTORY;
                if let Some(ty) = self.data_factories.and_then(|factories| factories.get(&expr_span)) {
                    // Only admitted declarations establish a Data class identity.
                    expr.decisions |= crate::expr::RESOLVED_DATA_FACTORY;
                    expr.diagnostic = None;
                    if let Some(recv) = recv { self.analyze_expr(recv, ctx); }
                    for arg in args.iter_mut() { self.analyze_expr(arg, ctx); }
                    return ty.clone();
                }
                // Bare-name implicit-self Send (no receiver, no args, no
                // block) resolves to a local binding when one exists. Ruby
                // parses `x` as `self.x()` when `x` wasn't assigned earlier
                // in scope; partials receive locals at render time (not via
                // syntactic assignment), so we disambiguate here instead of
                // at ingest. Block params and let-bindings end up on the
                // same code path.
                if recv.is_none() && args.is_empty() && block.is_none() {
                    if let Some(ty) = ctx.local_bindings.get(method) {
                        return ty.clone();
                    }
                }
                // Bare-name Kernel calls (no explicit receiver). `require`,
                // `require_relative`, etc. — Ruby treats these as private
                // module methods, but the body-typer otherwise resolves
                // `recv=None` against `self_ty`, which would dispatch
                // them as instance methods on the enclosing class (and
                // miss). Catch them here before the recv_ty fall-through.
                if recv.is_none() {
                    // A method of the enclosing class by that name (frozen_record's
                    // `class << self; alias_method :load, :new`) shadows the
                    // Kernel one, as Ruby's lookup does: self's class comes first.
                    let shadowed = matches!(
                        ctx.self_ty.as_ref(),
                        Some(Ty::Class { id, .. })
                            if self.classes().get(id).is_some_and(|cls| if ctx.class_side {
                                cls.has_class_method(method)
                            } else {
                                cls.has_instance_method(method)
                            })
                    );
                    if !shadowed && matches!(
                        method.as_str(),
                        "require" | "require_relative" | "load" | "autoload"
                    ) {
                        for a in args.iter_mut() { self.analyze_expr(a, ctx); }
                        return Ty::Bool;
                    }
                    // Kernel IO — `puts`/`print`/`warn` return nil.
                    if matches!(method.as_str(), "puts" | "print" | "warn") {
                        for a in args.iter_mut() { self.analyze_expr(a, ctx); }
                        return Ty::Nil;
                    }
                    // Kernel#format / #sprintf — a String, whatever the
                    // args (the runtime's number_with_precision builds
                    // its format string dynamically).
                    if matches!(method.as_str(), "format" | "sprintf") {
                        for a in args.iter_mut() { self.analyze_expr(a, ctx); }
                        return Ty::Str;
                    }
                }

                // Self-dispatch annotation. When `recv == None` and the
                // typer's dispatch through `self_ty` finds the method on
                // the enclosing class (instance or class methods), write
                // back `recv = Some(SelfRef)` so emitters consume already-
                // explicit self-receivers without a per-emitter rewrite
                // pass. Bare calls that don't resolve (Kernel methods,
                // undefined names, names not in the class registry) stay
                // `recv = None` — those route through each target's
                // free-function code path. Targets that don't model self
                // as an instance (e.g., Crystal's module-shape controller
                // emit) render `SelfRef` as appropriate for their shape;
                // see the per-target SelfRef arms in each emit_expr.
                let resolves_through_self = ctx.annotate_self_dispatch
                    && recv.is_none()
                    && matches!(
                        ctx.self_ty.as_ref(),
                        Some(Ty::Class { id, .. })
                            if self.classes().get(id).is_some_and(|cls|
                                cls.has_class_method(method)
                                    || cls.has_instance_method(method))
                    );
                if resolves_through_self {
                    *recv = Some(crate::expr::Expr {
                        span: expr_span,
                        node: Box::new(ExprNode::SelfRef),
                        ty: ctx.self_ty.clone(),
                        effects: crate::effect::EffectSet::default(),
                        leading_blank_line: false,
                        diagnostic: None,
                        hint: None,
                        decisions: 0,
                        keyword_arg: None,
                    });
                }

                let recv_ty = match recv.as_mut() {
                    Some(r) => Some(self.analyze_expr(r, ctx)),
                    None => ctx.self_ty.clone(),
                };
                // Inside the class-side method, the instance a class-side
                // `new` built (`Ty::SelfInstance`, see below) answers the
                // methods of the class the `def` sits in: the receiving
                // subclass is only known at the call site, and what the
                // body can call on it is the base's surface. The value
                // itself stays `SelfInstance`, so the method still answers
                // the receiving class.
                let recv_ty = match (recv_ty, &ctx.self_ty) {
                    (Some(t), Some(self_ty @ Ty::Class { .. })) if ctx.class_side => {
                        Some(t.subst_self(self_ty))
                    }
                    (other, _) => other,
                };
                // `Parameters` is a Hash-shaped bag: what its own class does
                // not answer (`fetch`, `each`, `map`, `count`, ...) is the
                // Hash reading, over Symbol -> param value.
                let mut recv_ty = match recv_ty {
                    Some(Ty::Class { id, .. })
                        if id.0.as_str() == "ActionController::Parameters"
                            && method.as_str() != "new"
                            && !self.classes().get(&id).is_some_and(|c|
                                c.has_instance_method(method)
                                    || c.has_class_method(method)) =>
                    {
                        Some(send::params_as_hash())
                    }
                    other => other,
                };
                // `new` on the implicit or explicit `self` of a class-side
                // method builds an instance of the class that RECEIVED the
                // call: `CurrencyDb.load` runs `YamlDb.load` with `self ==
                // CurrencyDb`. Typing it as the class the `def` sits in made
                // every subclass factory answer the base class.
                // So does a class-side alias of Class#new (frozen_record's
                // `alias_method :load, :new`, which its own `new` calls).
                if ctx.class_side
                    && recv.as_ref().map_or(true, |r| matches!(&*r.node, ExprNode::SelfRef))
                    && matches!(recv_ty, Some(Ty::Class { .. }))
                    && if method.as_str() == "new" {
                        !self.has_declared_constructor(recv_ty.as_ref())
                    } else {
                        self.class_method_builds_receiver(recv_ty.as_ref(), method)
                    }
                {
                    for a in args.iter_mut() {
                        self.analyze_expr(a, ctx);
                    }
                    return Ty::SelfInstance;
                }
                for a in args.iter_mut() {
                    self.analyze_expr(a, ctx);
                }
                if let Some(r) = recv.as_mut() {
                    if promotes_to_param_value(r, recv_ty.as_ref(), method, args, &ctx.local_bindings) {
                        r.ty = Some(send::param_value_ty());
                        recv_ty = r.ty.clone();
                    }
                }
                // Ruby's Module callback ABI supplies a class/module object. Direct
                // source calls must satisfy the same identity contract.
                if self.is_module_callback(recv_ty.as_ref(), method) {
                    if let Some(arg) = args.first() {
                        if !self.is_class_object(arg, ctx) {
                            expr.diagnostic = Some(crate::diagnostic::DiagnosticKind::Unsupported {
                                target: None, construct: Symbol::from("Module callback argument"),
                                detail: format!("{} requires a proven class/module object argument", method),
                            });
                        }
                    }
                }
                let class_object_receiver = recv.as_ref().map_or(ctx.class_side, |r| self.is_class_object(r, ctx))
                    || (recv.as_ref().is_none_or(|r| matches!(&*r.node, ExprNode::SelfRef))
                        && send::is_module_protocol(method)
                        && (self.self_is_module_instance(ctx) || self.self_runs_as_extender(method, ctx, expr.span)))
                    || send::is_module_query_on_module_value(method, recv_ty.as_ref())
                    // `x.class` is a class object, though not a known one:
                    // enough for a query, not for `define_method`.
                    || (send::is_module_query(method) && recv.as_ref().is_some_and(|r| self.is_class_of_value(r, ctx)));
                let block_ret = if let Some(b) = block {
                    let mut block_ctx = self.block_ctx_for(
                        ctx,
                        recv_ty.as_ref(),
                        method,
                        args,
                        class_object_receiver,
                        b,
                    );
                    if matches!(method.as_str(), "instance_eval" | "instance_exec" | "class_eval" | "class_exec" | "module_eval" | "module_exec") {
                        if let Some(receiver) = recv.as_ref() {
                            block_ctx.self_ty = recv_ty.clone();
                            block_ctx.class_side = self.is_class_object(receiver, ctx);
                            block_ctx.instance_body = matches!(method.as_str(), "instance_eval" | "instance_exec")
                                && self.is_instance(receiver, ctx);
                        }
                    }
                    // `ActiveSupport.on_load(:hook) { … }`: lazy_load_hooks
                    // `class_eval`s the block on the base the hook fires with.
                    if method.as_str() == "on_load"
                        && recv.as_ref().is_some_and(|r| matches!(&*r.node, ExprNode::Const { path } if path.len() == 1 && path[0].as_str() == "ActiveSupport"))
                        && let Some(ExprNode::Lit { value: crate::expr::Literal::Sym { value: hook } }) = args.first().map(|a| &*a.node)
                        && let Some(base) = self.load_hook_bases.and_then(|bases| bases.get(hook.as_str()))
                    {
                        block_ctx.self_ty = Some(Ty::Class { id: base.clone(), args: vec![] });
                        block_ctx.class_side = true;
                    }
                    let method_ref_ty = self.analyze_expr(b, &block_ctx);
                    // The Lambda walker stores the analyzed body's type
                    // on the body expr itself. `map`/`collect`/similar
                    // use that to determine the output element type.
                    // `MethodRef` (`&method(:name)`) has no body to
                    // read from — its own computed type already IS
                    // the referenced method's return type.
                    match &*b.node {
                        ExprNode::Lambda { body, .. } => body.ty.clone(),
                        ExprNode::MethodRef { .. } => Some(method_ref_ty),
                        // Forwarded proc (`&callback`): Var in the block
                        // slot, no body to type. Presence must still reach
                        // dispatch — `PTY.spawn` with a block answers nil.
                        // A nil local (`callback = nil; …(&callback)`) is
                        // Ruby's no-block path, as is a literal `&nil`.
                        // A nilable local is refined for `PTY.spawn` after
                        // dispatch (Tuple | Nil); leave presence absent
                        // here so `String#bytes(&maybe)` stays the array.
                        ExprNode::Var { name, .. } => match ctx.local_bindings.get(name) {
                            Some(Ty::Nil) => None,
                            Some(ty) if forwarded_block_may_be_nil(ty) => None,
                            _ => Some(Ty::Untyped),
                        },
                        _ => None,
                    }
                } else {
                    None
                };
                if recv_ty.as_ref() == Some(&Ty::Str) && method.as_str() == "bytes"
                    && let Some(b) = block.as_ref()
                    && let ExprNode::Lambda { body, .. } = &*b.node
                    && bytes_block_has_escaping_break(body)
                {
                    expr.diagnostic = Some(crate::diagnostic::DiagnosticKind::Unsupported {
                        target: None,
                        construct: Symbol::from("String#bytes block break"),
                        detail: "an escaping break can replace the receiver result; its return type is not modeled".into(),
                    });
                }
                if method.as_str() == "new"
                    && matches!(&recv_ty, Some(Ty::Class { id, .. }) if id.0.as_str() == "Data")
                    && self.const_resolver.as_ref().is_some_and(|resolver| !resolver.has_source_namespace("Data"))
                {
                    expr.diagnostic = Some(crate::diagnostic::DiagnosticKind::Unsupported {
                        target: None,
                        construct: Symbol::from("Data.new"),
                        detail: "Data is abstract; construct a class returned by Data.define".into(),
                    });
                    return unknown();
                }
                // Force `parenthesized: true` when dispatch resolves
                // to a `Method`-kind on a registered class. The TS
                // emitter's bare-recv-Send fallback omits parens when
                // `parenthesized` is false (matching Ruby's reader
                // idiom); for real methods we want `obj.save()` not
                // `obj.save`. AttributeReader/Writer dispatches keep
                // the ingested value (already false for source-style
                // reads, true if source had parens). Unresolved
                // dispatches (recv unknown, method not in registry)
                // also keep the ingested value to preserve source
                // form for round-trip-through-emit.
                if !*parenthesized {
                    if let Some(Ty::Class { id, .. }) = &recv_ty {
                        use crate::dialect::AccessorKind;
                        // Walk the parent chain so methods inherited
                        // from a base class (e.g. `save` on
                        // `ActiveRecord::Base` reached via Article →
                        // ApplicationRecord → Base) resolve. The
                        // first kind found wins; cycles are
                        // impossible by construction (Ruby class
                        // hierarchy is a DAG).
                        let mut current_id: Option<&crate::ident::ClassId> = Some(id);
                        let mut seen = 0usize;
                        let mut resolved: Option<AccessorKind> = None;
                        while let Some(cid) = current_id {
                            seen += 1;
                            if seen > 32 {
                                break; // defensive cycle break
                            }
                            let Some(cls) = self.classes().get(cid) else {
                                break;
                            };
                            if let Some(k) = cls
                                .instance_method_kinds
                                .get(method)
                                .or_else(|| cls.class_method_kinds.get(method))
                            {
                                resolved = Some(*k);
                                break;
                            }
                            current_id = cls.parent_id();
                        }
                        if matches!(resolved, Some(AccessorKind::Method)) {
                            *parenthesized = true;
                        }
                    }
                }
                // Source-level kwargs (`f(a: 1, b: 2)`) parse as a
                // trailing `KeywordHash` (`kwargs: true`). Ruby
                // implicitly forwards them to either an `**opts`/named
                // parameter OR an `opts = {}` positional Hash. Strict-
                // typed targets need to commit. Resolve the method's
                // declared signature: when the last param is positional
                // (`Required`/`Optional`) with Hash type, flip the
                // kwargs Hash to `kwargs: false` so emit renders as an
                // explicit `{ ... }` literal at the positional slot.
                // Methods whose last param is `Keyword`/`KeywordRest`
                // keep `kwargs: true` so emit renders bare named args
                // (Crystal NamedTuple-friendly, TS object-spread-
                // friendly via the named-param shape).
                self.normalize_trailing_kwargs(recv_ty.as_ref(), method, args);
                if let Some(t) = self.assoc_extension_ty(recv.as_ref(), method) {
                    return t;
                }
                if let Some(t) = self.assoc_loaded_ty(recv.as_ref(), method) {
                    return t;
                }
                // `x.attr = v` evaluates to `v` — Ruby's rule for an
                // attribute assignment, whatever the writer's body
                // returns. Same fact the harvest declares for a setter's
                // return; typed here too so a body that ends in one
                // (`Current.instance.session = value`) agrees with it.
                // An argument the typer could not resolve (Var) or only
                // resolved gradually (Untyped) says nothing the writer's
                // registered answer does not say better — campfire's
                // `self.join_code = generate_join_code` keeps the
                // column's String rather than the generator's untyped.
                if recv.is_some()
                    && args.len() == 1
                    && crate::analyze::is_setter_name(method)
                {
                    if let Some(t) = args[0]
                        .ty
                        .clone()
                        .filter(|t| !matches!(t, Ty::Var { .. } | Ty::Untyped))
                    {
                        return t;
                    }
                }
                // `authenticated_by.bot_key?` / `content_type.attachment?`
                // — an inquirer predicate, decided on the receiver
                // EXPRESSION (the value types as the Str it is).
                if let Some(recv) = recv.as_ref() {
                    if let Some(inquirers) = self.inquirers {
                        if crate::analyze::inquiry::is_inquiry_predicate(
                            recv, method, args, inquirers,
                        ) {
                            return Ty::Bool;
                        }
                    }
                }
                // `group(:col).count` answers a Hash, not an Int. Resolved
                // here rather than in `dispatch` because the discriminator
                // is the receiver EXPRESSION (`Ty::Relation` carries no
                // grouped state) and `dispatch` sees only its type.
                if let Some(t) = self.grouped_count_ty(
                    recv.as_ref(),
                    recv_ty.as_ref(),
                    method,
                    args,
                    block.as_ref(),
                ) {
                    return t;
                }
                if let Some(t) =
                    self.column_attribute_access_ty(recv_ty.as_ref(), method, args)
                {
                    return t;
                }
                if let Some(t) = recv.as_ref().and_then(|r| time_parse_ty(r, method, args)) {
                    return t;
                }
                if let Some(t) = expect_hash_arg_ty(recv_ty.as_ref(), method.as_str(), args) {
                    return t;
                }
                // Not `Integer?`: `nil.to_fs` raises in Rails, so only a number that cannot be nil types.
                if matches!(recv_ty, Some(Ty::Int) | Some(Ty::Float))
                    && crate::lower::number_to_fs::is_delimited_to_fs(method, args)
                {
                    return Ty::Str;
                }
                if let Some(t) = recv_ty
                    .as_ref()
                    .and_then(|t| literal_extremum_ty(recv.as_ref(), t, method, args))
                {
                    return t;
                }
                // Blockless, `n.times` is the Enumerator over 0...n, read
                // as the Array of its Integers as `upto` is (`MAX.times.map`).
                if recv_ty == Some(Ty::Int) && method.as_str() == "times" && block.is_none() && args.is_empty() {
                    return Ty::Array { elem: Box::new(Ty::Int) };
                }
                // Enumerable over Rails' collection (`expr::errors_enumerable`):
                // it runs over the Errors, an Array of ActiveModel::Error on
                // the trees that build them (`expr::ERRORS_PORT_CALL`).
                // A view's `errors.each do |error| ... end` is the view
                // lowering's to rewrite onto the messages on every target
                // (`IrHint::ErrorMessagesEach`), before or after it has.
                let view_messages_each = expr.hint == Some(crate::expr::IrHint::ErrorMessagesEach)
                    || (ctx.in_view
                        && method.as_str() == "each"
                        && args.is_empty()
                        && recv.as_ref().is_some_and(|r| matches!(&*r.node,
                            ExprNode::Send { method: m, args: a, block: None, .. } if m.as_str() == "errors" && a.is_empty())));
                if crate::expr::errors_enumerable(method.as_str(), args.len(), block.is_some())
                    && !view_messages_each
                    && matches!(recv_ty.as_ref(), Some(Ty::Class { id, .. }) if id.0.as_str() == "ActiveModel::Errors")
                {
                    expr.decisions |= crate::expr::ERRORS_PORT_CALL;
                    let objects = send::active_model_error_objects();
                    if matches!(method.as_str(), "errors" | "objects") {
                        return objects;
                    }
                    return self.dispatch(Some(&objects), method, block_ret.as_ref(), args);
                }
                let instance_receiver = recv.as_ref().map_or(ctx.instance_body, |r| self.is_instance(r, ctx))
                    && recv_ty.as_ref().is_some_and(instance_shaped);
                let dispatched =
                    self.dispatch_on(recv_ty.as_ref(), method, block_ret.as_ref(), args, instance_receiver);
                // `PTY.spawn(..., &maybe)` when `maybe` is nilable: Ruby
                // may take the block (nil) or not (tuple). Presence was
                // left absent above so other methods keep their no-block
                // answer; widen the spawn result here.
                if method.as_str() == "spawn"
                    && matches!(&recv_ty, Some(Ty::Class { id, .. }) if id.0.as_str() == "PTY")
                    && let Some(b) = block.as_ref()
                    && let ExprNode::Var { name, .. } = &*b.node
                    && ctx
                        .local_bindings
                        .get(name)
                        .is_some_and(forwarded_block_may_be_nil)
                {
                    let file = Ty::Class { id: ClassId(Symbol::from("File")), args: vec![] };
                    return union_of(
                        Ty::Tuple { elems: vec![file.clone(), file, Ty::Int] },
                        Ty::Nil,
                    );
                }
                if let Some(receiver) = recv.as_mut() {
                    receiver.decisions &= !crate::expr::RESOLVED_OPERATOR_RECEIVER;
                    if matches!(method.as_str(), "+" | "-" | "*" | "/" | "**" | "%" | "<" | "<=" | ">" | ">=")
                        && self.may_own_operator(
                            recv_ty.as_ref(),
                            method,
                            receiver.decisions & crate::expr::CLASS_OBJECT_VALUE != 0 || self.is_class_object(receiver, ctx),
                        )
                    {
                        receiver.decisions |= crate::expr::RESOLVED_OPERATOR_RECEIVER;
                    }
                }
                // Kernel.Array is a container even for scalar params.
                // App methods (including inherited/included overrides)
                // have already dispatched above and must win.
                // RBS declares it `(untyped) -> Array[untyped]`; the
                // argument says more.
                if recv.is_none() && method.as_str() == "Array" && args.len() == 1
                    && block.is_none()
                    && (matches!(dispatched, Ty::Var { .. })
                        || (matches!(dispatched, Ty::Untyped)
                            && !self.app_defines(ctx.self_ty.as_ref(), method)))
                {
                    let elem = args[0].ty.as_ref().and_then(kernel_array_elem);
                    return Ty::Array { elem: Box::new(elem.unwrap_or_else(unknown)) };
                }
                // Kernel's conversion functions answer their class, or
                // nil under `exception: false` (core `kernel.rbs`, stdlib
                // `bigdecimal.rbs`). As for `Array`, an app method of the
                // name has already dispatched and wins.
                if recv.is_none() && block.is_none() && matches!(dispatched, Ty::Var { .. }) {
                    if let Some(ty) = kernel_conversion_ty(method, args) {
                        return ty;
                    }
                }
                // Kernel#proc / Kernel#lambda with a literal block answer
                // the Proc that block becomes: the value `->(x) { … }` is,
                // typed the same way, by the block (core `kernel.rbs`:
                // `proc: () { (?) -> untyped } -> Proc`). A module-body
                // `Formatter = proc do |upload| … end` was a constant with
                // no inferred value, so every read of it (app_profiler's
                // `mattr_reader :profile_url_formatter, default:
                // DefaultProfileFormatter`) was "constant not supported".
                // As for `Array`, an app method of the name has already
                // dispatched and wins.
                if recv.is_none() && args.is_empty() && matches!(method.as_str(), "proc" | "lambda")
                    && (matches!(dispatched, Ty::Var { .. })
                        || (matches!(dispatched, Ty::Untyped) && !self.app_defines(ctx.self_ty.as_ref(), method)))
                {
                    if let Some(block_ty @ Ty::Fn { .. }) = block
                        .as_ref()
                        .filter(|b| matches!(&*b.node, ExprNode::Lambda { .. }))
                        .and_then(|b| b.ty.clone())
                    {
                        return block_ty;
                    }
                }
                // What every object and every module answers, when the
                // receiver's own table did not. App analyzer only.
                // `class_object_receiver` was resolved above for block binding
                // so it matches the same class/instance table preference.
                if matches!(dispatched, Ty::Var { .. } | Ty::Untyped) && self.inquirers.is_some()
                    && (recv.is_some() || (ctx.self_ty.is_some() && send::is_module_protocol(method)))
                    && !self.owns_operator(recv_ty.as_ref(), method, class_object_receiver) {
                    let class_object = class_object_receiver;
                    // `include?` is Module's ancestor check AND String/Enumerable
                    // membership. Refuse Module protocol only for true Module-only
                    // names, or for `include?` on a nominal class *instance* (where
                    // Module#include? would be the wrong answer). Untyped /
                    // String / Array receivers fall through to membership → Bool.
                    let module_only = send::is_module_protocol(method)
                        && (method.as_str() != "include?"
                            || matches!(recv_ty, Some(Ty::Class { .. })));
                    if module_only && !class_object
                        && !self.owns_operator(recv_ty.as_ref(), method, false)
                        && !send::is_query_any_receiver_may_answer(method, recv_ty.as_ref(), self.classes()) {
                        expr.diagnostic = Some(crate::diagnostic::DiagnosticKind::Unsupported {
                            target: None,
                            construct: Symbol::from("Module protocol"),
                            detail: format!("{} requires a proven class/module object receiver", method),
                        });
                        return dispatched;
                    }
                    if let Some(t) = send::object_protocol_method(recv_ty.as_ref(), method, block_ret.as_ref(), class_object) {
                        return t;
                    }
                }
                // Module#include / #prepend on a proven class object, of
                // modules named by constant: a load hook's `include M` on
                // the base it `class_eval`s (Core's `ActiveSupport.on_load(
                // :active_record) { include SerializationOptions }`). The
                // runtime's class answers it as Ruby does, and it returns
                // the receiver.
                if matches!(dispatched, Ty::Var { .. } | Ty::Untyped) && self.inquirers.is_some()
                    && matches!(method.as_str(), "include" | "prepend") && block.is_none() && !args.is_empty()
                    && class_object_receiver && expr.diagnostic.is_none()
                    && args.iter_mut().all(|arg| self.top_level_mixin_argument(arg, ctx))
                {
                    return recv_ty.clone().unwrap_or(Ty::Untyped);
                }
                // type_toolkit's `x.not_nil!` (Core's `T.must`): Kernel's
                // answers the receiver, NilClass's raises
                // `TypeToolkit::UnexpectedNilError`, so the value is the
                // receiver with nil removed. The CRuby overlay carries the
                // gem's definition (`ruby_overlay/runtime/type_toolkit.rb`);
                // `project` refuses it where no tree does. The assertion
                // ingest synthesizes for `#: as !nil` keeps its own refusal.
                if matches!(dispatched, Ty::Var { .. } | Ty::Untyped) && self.inquirers.is_some()
                    && method.as_str() == "not_nil!" && args.is_empty() && block.is_none()
                    && expr.diagnostic.is_none()
                {
                    if let Some(ty) = recv_ty.as_ref().filter(|_| recv.is_some()) {
                        // An un-inferred arm stays gradual, as union
                        // dispatch treats it, rather than a second error.
                        return match ty.clone().strip_nil() {
                            Ty::Var { .. } => Ty::Untyped,
                            other => other,
                        };
                    }
                }
                // `errors.to_hash([full])`, `merge!(other)` and the like on
                // Rails' collection: the CRuby and JRuby trees answer them
                // from what each grounded push records
                // (`expr::ERRORS_PORT_CALL`).
                if crate::expr::errors_port_call(method.as_str(), args.len(), block.is_some())
                    && matches!(recv_ty.as_ref(), Some(Ty::Class { id, .. }) if id.0.as_str() == "ActiveModel::Errors")
                {
                    expr.decisions |= crate::expr::ERRORS_PORT_CALL;
                }
                // A record's serialization beyond `only:`: gated to the
                // tree whose ActiveRecord::Base carries it (`send::
                // record_serialization`, `analyze::runtime_reads`).
                if matches!(method.as_str(), "as_json" | "serializable_hash") && self.inquirers.is_some()
                    && self.record_serialization(recv.as_ref(), recv_ty.as_ref(), method.as_str(), args, class_object_receiver, ctx)
                {
                    expr.decisions |= crate::expr::RECORD_SERIALIZATION;
                }
                // String / Array / Hash `as_json` the core tables answer
                // (send.rs) is ActiveSupport's core_ext too, which no tree
                // reopens: the same route, with the table's type.
                if method.as_str() == "as_json" && self.inquirers.is_some() && expr.diagnostic.is_none()
                    && recv.is_some() && !class_object_receiver && send::as_json_core_receiver(recv_ty.as_ref())
                    && matches!(self.as_json_residue(recv.as_ref(), recv_ty.as_ref(), args, block.is_some()),
                        send::AsJsonResidue::Route)
                {
                    expr.decisions |= crate::expr::OBJECT_AS_JSON;
                }
                if matches!(dispatched, Ty::Var { .. } | Ty::Untyped) && self.inquirers.is_some()
                    && !self.owns_operator(recv_ty.as_ref(), method, class_object_receiver) {
                    // ActiveSupport's core_ext `as_json` on a value no static
                    // method answers: `lower::as_json_residue` routes it to
                    // the port's `ActiveSupport::JSON::Encoding.as_json`,
                    // which asks an object with an `as_json` of its own and
                    // answers core_ext/object/json.rb's table for the rest.
                    if method.as_str() == "as_json" && expr.diagnostic.is_none() {
                        match self.as_json_residue(recv.as_ref(), recv_ty.as_ref(), args, block.is_some()) {
                            send::AsJsonResidue::Route => {
                                expr.decisions |= crate::expr::OBJECT_AS_JSON;
                                return send::object_as_json_ty(recv_ty.as_ref(), class_object_receiver);
                            }
                            send::AsJsonResidue::Refuse(construct, detail) => {
                                expr.diagnostic = Some(crate::diagnostic::DiagnosticKind::Unsupported {
                                    target: None,
                                    construct: Symbol::from(construct),
                                    detail: detail.to_string(),
                                });
                                return dispatched;
                            }
                            send::AsJsonResidue::Gap => {}
                        }
                    }
                    // A receiver typed as `ActiveRecord::Base` itself
                    // (`record = self #: as ActiveRecord::Base`, a
                    // `singleton(ActiveRecord::Base)` parameter) is a
                    // model or a model class, whose ancestry reaches the
                    // runtime's Base: a method the registry models there,
                    // or the PostgreSQL CRuby tree's Base defines, is a
                    // plain send Ruby dispatches. Only a method neither
                    // answers stays a named gap.
                    //
                    // The side is the receiver's where it is proven: a
                    // constant, `x.class` or `self` in a class body is
                    // the class; `self` in an instance body an instance.
                    // A value read from a local, an ivar or a call has
                    // one flat `Ty::Class` for both (`rbs::ty_from_node` types
                    // `singleton(C)` as `C`), which dispatch answers
                    // from either side (`send::dispatch_on`), so here too.
                    if let Some(Ty::Class { id, .. }) = recv_ty.as_ref()
                        && id.0.as_str() == "ActiveRecord::Base"
                        && !ctx.claimed_macro_template
                        && expr.diagnostic.is_none()
                    {
                        let on_self = recv.as_ref().is_none_or(|r| matches!(&*r.node, ExprNode::SelfRef));
                        let class_side = class_object_receiver
                            || recv.as_ref().is_some_and(|r| self.is_class_of_value(r, ctx));
                        let sides: &[bool] = if class_side {
                            &[true]
                        } else if on_self {
                            &[false]
                        } else {
                            &[false, true]
                        };
                        let pg = self.carries_postgresql_base();
                        let answered = sides.iter().any(|&class_side| {
                            self.owns_operator(recv_ty.as_ref(), method, class_side)
                                || (pg && runtime_base::answers(method.as_str(), class_side, !on_self))
                        });
                        if answered {
                            // Typed as the registry does a Base method it
                            // seeds without a signature: the gradual escape.
                            return if matches!(dispatched, Ty::Var { .. }) { Ty::Untyped } else { dispatched };
                        }
                    }
                    let gap = match recv_ty.as_ref() {
                        Some(Ty::Class { id, .. }) if matches!(id.0.as_str(), "ActiveModel::Errors" | "ActiveModel::Error") => Some(id.0.as_str()),
                        Some(Ty::Class { id, .. })
                            if matches!(id.0.as_str(), "Rails" | "ActiveRecord::Base")
                                && !ctx.claimed_macro_template =>
                        {
                            Some(id.0.as_str())
                        }
                        Some(Ty::Str) if matches!(method.as_str(), "to_date" | "to_time" | "to_datetime" | "in_time_zone" | "to_d") => Some("String extension"),
                        Some(Ty::Hash { .. }) if method.as_str() == "to_sentence" => Some("Hash extension"),
                        _ if method.as_str() == "not_nil!" => Some("not_nil!"),
                        // `as_json` with more than one options argument, a block or
                        // a splat: the port's `Encoding.as_json(value, options)`
                        // takes neither.
                        _ if method.as_str() == "as_json" => Some("Object extension"),
                        // `to_param` on Untyped goes through ActiveSupport.to_param
                        // (residue lowering + runtime). Refuse only on a nominal
                        // class with no modeled/synthesized reader (e.g. a plain
                        // PORO). Untyped helper params (campfire AvatarsHelper)
                        // must not hard-fail.
                        _ if method.as_str() == "to_param"
                            && matches!(recv_ty, Some(Ty::Class { .. })) =>
                        {
                            Some("Object extension")
                        }
                        _ if matches!(method.as_str(), "to_query" | "instance_values" | "acts_like?" | "presence_in" | "with_options" | "pretty_inspect") => Some("Object extension"),
                        _ => None,
                    };
                    if let Some(owner) = gap.filter(|_| expr.diagnostic.is_none()) {
                        expr.diagnostic = Some(crate::diagnostic::DiagnosticKind::Unsupported {
                            target: None,
                            construct: Symbol::from(owner),
                            detail: format!("{} lacks a shared runtime/lowering implementation", method),
                        });
                    }
                }
                dispatched
            }

            ExprNode::If { cond, then_branch, else_branch } => {
                self.analyze_expr(cond, ctx);
                let pred = narrowing::extract_narrowing_with(cond, &|n| self.self_attribute_ty(n, ctx));
                // Var assignments inside `cond` (e.g.
                // `if (user = User.find_by(...)) && user.is_active?`)
                // must flow into both branches: the assignment
                // executed during cond evaluation regardless of
                // branch taken. Then-branch may further narrow via
                // truthiness; else-branch sees the raw assigned type.
                // The assignment binds first and the predicate narrows
                // what it bound: `if user = User.authenticate_by(…)`
                // reads `user` as `User` in the then-branch, `User?`
                // in the else-branch. (Narrowing before seeding would
                // find no binding to narrow and leave the nil arm.)
                let mut cond_assigns: HashMap<Symbol, Ty> = HashMap::default();
                collect_var_assignments_into(cond, &mut cond_assigns);
                let mut base = ctx.clone();
                for (k, v) in &cond_assigns {
                    base.local_bindings.insert(k.clone(), v.clone());
                }
                self.propagate_match_bindings(cond, &mut base, true);
                let then_ctx = match &pred {
                    Some(p) => narrowing::apply_narrowing(&base, p, true),
                    None => base.clone(),
                };
                let t = self.analyze_expr(then_branch, &then_ctx);
                let else_ctx = match &pred {
                    Some(p) => narrowing::apply_narrowing(&base, p, false),
                    None => base,
                };
                let e = self.analyze_expr(else_branch, &else_ctx);
                union_of(t, e)
            }

            ExprNode::Case { scrutinee, arms } => {
                self.analyze_expr(scrutinee, ctx);
                // `when C` narrows the scrutinee to `C` in its arm, and
                // every later arm runs only because the earlier
                // patterns failed, so it sees those classes removed.
                // Without it a structural recursion over `case value
                // when Hash … when Array …` typed each arm against the
                // whole union: `value.map { |v| … }` in the Array arm
                // dispatched on the Hash variant, its block saw the
                // keys, and the recursive call's observed argument
                // alternated round to round — the param never settled.
                let mut failed: Vec<narrowing::NarrowPred> = Vec::new();
                let mut branch_tys = Vec::new();
                for arm in arms.iter_mut() {
                    let mut arm_ctx = failed
                        .iter()
                        .fold(ctx.clone(), |c, p| narrowing::apply_narrowing(&c, p, false));
                    let pred = match &arm.pattern {
                        Pattern::Expr { expr: pat } if arm.guard.is_none() => narrowing::case_when_class(
                            scrutinee,
                            pat,
                            &|n| self.self_attribute_ty(n, ctx),
                            &|e| self.is_class_object(e, ctx),
                        ),
                        _ => None,
                    };
                    if let Some(p) = &pred {
                        arm_ctx = narrowing::apply_narrowing(&arm_ctx, p, true);
                    }
                    if let Some(g) = &mut arm.guard { self.analyze_expr(g, &arm_ctx); }
                    branch_tys.push(self.analyze_expr(&mut arm.body, &arm_ctx));
                    failed.extend(pred);
                }
                union_many(branch_tys)
            }

            // Pattern bindings are lexical locals. Failed guards do not
            // roll them back, and failed patterns can bind a prefix.
            // Sibling arms see possible earlier bindings joined with
            // their old value (nil for a newly introduced local).
            ExprNode::CaseMatch { scrutinee, arms, else_body } => {
                let scrutinee_ty = self.analyze_expr(scrutinee, ctx);
                let mut branch_tys = Vec::new();
                let mut fallthrough = ctx.clone();
                for arm in arms.iter_mut() {
                    let bindings =
                        self.match_pattern_bindings(&arm.pattern, Some(&scrutinee_ty));
                    let mut inner = fallthrough.clone();
                    for (name, ty) in &bindings {
                        inner.local_bindings.insert(name.clone(), ty.clone());
                    }
                    self.analyze_match_pattern_constants(&mut arm.pattern, &inner);
                    if let Some((_, g)) = &mut arm.guard {
                        self.analyze_expr(g, &inner);
                    }
                    branch_tys.push(self.analyze_expr(&mut arm.body, &inner));
                    for (name, ty) in bindings {
                        let old = fallthrough.local_bindings.get(&name).cloned().unwrap_or(Ty::Nil);
                        fallthrough.local_bindings.insert(name, union_of(old, ty));
                    }
                }
                if let Some(eb) = else_body {
                    branch_tys.push(self.analyze_expr(eb, &fallthrough));
                }
                union_many(branch_tys)
            }

            // The predicate returns Bool; user deconstruction/pin methods
            // can still raise. Bindings escape, including partial matches.
            ExprNode::MatchPredicate { value, pattern } => {
                self.analyze_expr(value, ctx);
                self.analyze_match_pattern_constants(pattern, ctx);
                Ty::Bool
            }

            // `value => pattern` — binds on match, raises
            // `NoMatchingPatternError` otherwise; evaluates to `nil`.
            // Statement-position bindings ARE propagated forward — see
            // the `Seq` walk below, which special-cases this the same
            // way it special-cases `Assign`.
            ExprNode::MatchRequired { value, pattern } => {
                self.analyze_expr(value, ctx);
                self.analyze_match_pattern_constants(pattern, ctx);
                Ty::Nil
            }

            ExprNode::Seq { exprs } => {
                // Within a Seq, walk statements in order and thread
                // bindings forward: `@post = Post.find(...)` in stmt i
                // lets stmt i+1 resolve `@post`. Same for local vars —
                // `x = post.title` binds `x` for subsequent statements.
                let mut local_ctx = ctx.clone();
                let mut last = Ty::Nil;
                // Statement index of each live empty-array accumulator
                // seed (`parts = []`), keyed by (is_ivar, name). The push
                // refinement below updates the BINDING when the element
                // type is revealed, but emitters that must commit a decl
                // type at the seed (Go's `parts := []string{}`, Crystal's
                // `[] of String`) read the literal's stamped ty — leaving
                // it at the `Var` placeholder makes the decl and the
                // refined uses disagree (`[]interface{}` handed to
                // `strings.Join`). Recording the index lets the
                // refinement retro-stamp the seed. A later reassignment
                // of the name retires its entry.
                let mut array_seed_idx: HashMap<(bool, Symbol), usize> = HashMap::default();
                // Statement index + accumulated type of each live
                // nil-seeded LOCAL (`size = nil`). The scalar analog of
                // `array_seed_idx`, and the same decl-site hazard: a
                // later `size = v` (typically nested in a block, so the
                // Seq walk never sees it as a statement) refines the
                // binding, but the seed keeps `Ty::Nil` — and a stale
                // `Ty::Nil` is worse here than a stale element type,
                // because `nil?` const-folds on it. Go's `nil?` arm
                // maps `Some(Ty::Nil) => "true"`, so `if !size.nil?`
                // emitted `if !(true)` and the guarded branch became
                // DEAD CODE — a silent miscompile, not a type error.
                // Accumulating the union here keeps the seed, the decl
                // and every `.nil?` read in agreement.
                //
                // Locals only: the hazard is Go's `var x T = nil` decl
                // path (`emit/go/expr.rs`, LValue::Var), and ivars emit
                // through package vars / struct fields that never take a
                // seed-derived decl type.
                let mut nil_seed: HashMap<Symbol, (usize, Ty)> = HashMap::default();
                // Statement index of each live empty-hash seed (`attrs =
                // {}`) — the Hash counterpart of `array_seed_idx`.
                let mut hash_seed_idx: HashMap<(bool, Symbol), usize> = HashMap::default();
                for i in 0..exprs.len() {
                    let read_structurally_later = match &*exprs[i].node {
                        ExprNode::Assign { target: LValue::Var { name, .. }, .. } => {
                            exprs[i + 1..].iter().any(|later| reads_structurally(later, name))
                        }
                        _ => false,
                    };
                    let e = &mut exprs[i];
                    if !matches!(&*e.node, ExprNode::Assign { target: LValue::Var { .. }, .. }) {
                        forget_class_object_writes(e, &mut local_ctx);
                    }
                    last = self.analyze_expr(e, &local_ctx);
                    // A self binding gives `self` its declared type from
                    // the next statement to the end of this body, blocks
                    // inside included. It changes only how `self` types:
                    // which class a method defined below belongs to is
                    // settled at ingest, lexically. A class-object binding
                    // puts `self` on the class side, as in a `def self.x`.
                    if crate::expr::is_self_binding(e) {
                        if local_ctx.lexical_self.is_none() {
                            local_ctx.lexical_self = local_ctx.self_ty.clone();
                        }
                        local_ctx.self_ty = e.ty.clone();
                        local_ctx.class_side = e.decisions & crate::expr::SELF_BINDING_CLASS_OBJECT != 0;
                    }
                    if let ExprNode::Assign { target: LValue::Var { name, .. }, value } = &*e.node {
                        let proven = self.is_class_object(value, &local_ctx);
                        forget_class_object_writes(value, &mut local_ctx);
                        local_ctx.class_objects.remove(name);
                        if proven { local_ctx.class_objects.insert(name.clone()); }
                    }
                    if let ExprNode::Assign { target, value } = &*e.node {
                        let slot = match target {
                            LValue::Ivar { name } => Some((true, name.clone())),
                            LValue::Var { name, .. } => Some((false, name.clone())),
                            _ => None,
                        };
                        if let Some((is_ivar, name)) = slot {
                            if let Some(ty) = e.ty.clone() {
                                if is_ivar {
                                    local_ctx.ivar_bindings.insert(name.clone(), ty);
                                } else {
                                    local_ctx.local_bindings.insert(name.clone(), ty);
                                }
                            }
                            if !is_ivar {
                                let mark = param_local_mark(&name);
                                if is_params_rooted(value, &local_ctx.local_bindings)
                                    || is_ivar_params_rooted(value, &local_ctx.local_bindings)
                                {
                                    local_ctx.local_bindings.insert(mark, Ty::Nil);
                                    // Not per use: a local the rest of the body reads as a hash or an array is one from its assignment on, so its `present?` grounds for that too.
                                    if read_structurally_later {
                                        local_ctx.local_bindings.insert(name.clone(), send::param_value_ty());
                                    }
                                } else {
                                    local_ctx.local_bindings.remove(&mark);
                                }
                            }
                            let empty_seed = matches!(&*value.node,
                                    ExprNode::Array { elements, .. } if elements.is_empty())
                                && matches!(e.ty.as_ref(),
                                    Some(Ty::Array { elem }) if matches!(&**elem, Ty::Var { .. }));
                            if empty_seed {
                                array_seed_idx.insert((is_ivar, name.clone()), i);
                            } else {
                                array_seed_idx.remove(&(is_ivar, name.clone()));
                            }
                            // Same bookkeeping for the `h = {}` seed.
                            let empty_hash_seed = matches!(&*value.node,
                                    ExprNode::Hash { entries, .. } if entries.is_empty())
                                && matches!(e.ty.as_ref(),
                                    Some(Ty::Hash { value, .. })
                                        if matches!(&**value, Ty::Var { .. }));
                            if empty_hash_seed {
                                hash_seed_idx.insert((is_ivar, name.clone()), i);
                            } else {
                                hash_seed_idx.remove(&(is_ivar, name.clone()));
                            }
                            // `x = nil` opens a scalar accumulator.
                            // Unlike the array seed, a later top-level
                            // reassignment does NOT retire it: `x = nil;
                            // x = "s"` still needs a decl type spanning
                            // both, and the refinement below unions the
                            // reassignment in (it walks this statement's
                            // subtree, which includes the statement
                            // itself). Only locals participate.
                            if !is_ivar
                                && matches!(&*value.node,
                                    ExprNode::Lit { value: Literal::Nil })
                                && matches!(e.ty.as_ref(), Some(Ty::Nil))
                                && !nil_seed.contains_key(&name)
                            {
                                nil_seed.insert(name, (i, Ty::Nil));
                            }
                        }
                    }
                    // A `begin … rescue … end` STATEMENT exports the
                    // locals its body and rescue arms assign at their own
                    // top level — Ruby's `begin` opens no scope — typed
                    // as the assigned value OR what the name held before
                    // (nil when it was unbound): the exception may come
                    // before the assignment runs. Without this a local
                    // assigned in one `begin` and read after it (`user =
                    // User.find(id)` in a begin, `user.messages.create!`
                    // below) read as unresolved, and everything keyed on
                    // its type — the association constructor rewrite —
                    // silently declined. Found by the model scenario
                    // scripts/campfire-db-differential runs statement by
                    // statement.
                    if let ExprNode::BeginRescue { body, rescues, .. } = &*e.node {
                        let mut exported: Vec<(Symbol, Ty)> = Vec::new();
                        top_level_local_assigns(body, &mut exported);
                        for rc in rescues.iter() {
                            top_level_local_assigns(&rc.body, &mut exported);
                        }
                        for (name, ty) in exported {
                            let prior = local_ctx.local_bindings.get(&name).cloned().unwrap_or(Ty::Nil);
                            local_ctx.local_bindings.insert(name, union_of(prior, ty));
                        }
                    }
                    // Short-circuit compound assignment (`@x ||= y`,
                    // `@x &&= y`) threads its binding forward too — the
                    // memoization idiom `@story ||= Story.find(...)` is
                    // pervasive in controllers, and without this the
                    // following `@story.title` reads `@story` as unknown.
                    // `OpAssign`'s node type is the value-expression's
                    // type (the naive desugar), so reuse it — but union
                    // with any prior binding and skip an unknown RHS so a
                    // before_action-seeded type isn't clobbered.
                    // `instance_variable_set(:@article, record)` is Kernel,
                    // not `Assign`, so the arm above misses it. Fold a
                    // statically resolvable name so a later statement in
                    // the same method can read the ivar.
                    if let Some((name, ty)) =
                        super::ivar_set::binding_from_send(e, local_ctx.self_ty.as_ref())
                    {
                        local_ctx.ivar_bindings.insert(name, ty);
                    }
                    if let ExprNode::OpAssign { target, .. } = &*e.node {
                        if let Some(ty) = e.ty.clone() {
                            if !matches!(ty, Ty::Var { .. }) {
                                match target {
                                    LValue::Ivar { name } => {
                                        let merged = match local_ctx
                                            .ivar_bindings
                                            .remove(name)
                                        {
                                            Some(prev) => union_of(prev, ty),
                                            None => ty,
                                        };
                                        local_ctx.ivar_bindings.insert(name.clone(), merged);
                                    }
                                    LValue::Var { name, .. } => {
                                        let merged = match local_ctx
                                            .local_bindings
                                            .remove(name)
                                        {
                                            Some(prev) => union_of(prev, ty),
                                            None => ty,
                                        };
                                        local_ctx.local_bindings.insert(name.clone(), merged);
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                    // Destructuring assignment threads each target
                    // forward: `@stories, @show_more = paginate(...)`
                    // in stmt i lets `render json: @stories` in stmt
                    // i+1 (or a nested `respond_to` block) resolve.
                    // Per-position typing via `multiassign_target_ty`;
                    // targets with no usable RHS signal are left as-is.
                    if let ExprNode::MultiAssign { targets, value } = &*e.node {
                        let rhs = value.ty.clone();                        for (i, target) in targets.iter().enumerate() {
                            let Some(ty) = multiassign_target_ty(&rhs, i) else {
                                continue;
                            };
                            match target {
                                LValue::Ivar { name } => {
                                    local_ctx.ivar_bindings.insert(name.clone(), ty);
                                }
                                LValue::Var { name, .. } => {
                                    local_ctx.local_bindings.insert(name.clone(), ty);
                                }
                                _ => {}
                            }
                        }
                    }
                    self.propagate_match_bindings(e, &mut local_ctx, true);
                    // Container element write — `hash[k] ||= []` /
                    // `hash[k] = v`. Widen the container's value type so
                    // a following `hash[k].push x` / `.join` in the same
                    // scope reads `Array|Nil` (resolvable via union
                    // dispatch) instead of `Var|Nil`. An empty `{}` seeds
                    // the value as Var; the accumulator idiom only
                    // reveals the element type at the write. The `{}` is
                    // bound on the recv ivar/local; a computed recv
                    // (`a.b[k]`) has no binding to refine, so skip it.
                    //
                    // Subtree-wide, for the reason spelled out on the
                    // array block below: the accumulator's writes live
                    // in a nested block (`opts.each { |k, v| attrs[k] =
                    // v }`) while its `{}` seed sits at statement level.
                    // A same-level match saw only the top-level writes,
                    // which is worse than seeing none — the visible
                    // `attrs[:src] = <Str>` monomorphized the hash while
                    // the invisible `attrs[k] = <Untyped>` still emitted
                    // against it (`interface{}` into `map[string]string`).
                    // Partial write visibility is unsound; whole-subtree
                    // visibility is what makes the refinement a fact.
                    let mut hash_writes: Vec<(bool, Symbol, Ty)> = Vec::new();
                    collect_hash_index_writes(e, &mut hash_writes);
                    // Array accumulator writes anywhere in this statement —
                    // `arr << x` / `arr.push(x)`, including nested in a loop,
                    // branch, or block (`while { acc << x }`, `items.each {
                    // acc << x }`). The Array analog of the Hash `h[k]=v`
                    // block above, but subtree-wide: the accumulator idiom
                    // fills a nested loop/block while the `arr = []` seed sits
                    // at the top level, so a same-level-only match (like the
                    // Hash one) would miss it — `analyze_expr(e)` above has
                    // already stamped every pushed arg's `.ty`, so a subtree
                    // scan can read them. Widen the outer-bound array's
                    // element from its empty-literal `Var` placeholder (or
                    // union the observed element into an existing one). Only
                    // bindings that already exist as `Array[_]` are touched.
                    let mut pushes: Vec<(bool, Symbol, Ty)> = Vec::new();
                    collect_array_pushes(e, &mut pushes);
                    // Apply the hash writes collected above. Deferred to
                    // here so both collections finish while the `&mut
                    // exprs[i]` borrow is live, leaving it dead before
                    // the seed retro-stamps reach back into `exprs`.
                    for (is_ivar, name, rhs) in hash_writes {
                        let bindings = if is_ivar {
                            &mut local_ctx.ivar_bindings
                        } else {
                            &mut local_ctx.local_bindings
                        };
                        if let Some(Ty::Hash { key, value }) = bindings.get(&name) {
                            // A Var value is the empty-literal
                            // placeholder — replace it; otherwise
                            // union the observed element in.
                            let new_value = if matches!(**value, Ty::Var { .. }) {
                                rhs
                            } else {
                                union_of((**value).clone(), rhs)
                            };
                            // Key stays as-is: the observed index
                            // expressions mix `:src` (Sym) with a block
                            // param (Untyped), and unioning those buys
                            // nothing — every target renders Ruby hash
                            // keys as strings regardless.
                            let refined = Ty::Hash {
                                key: key.clone(),
                                value: Box::new(new_value),
                            };
                            bindings.insert(name.clone(), refined.clone());
                            // Retro-stamp the `{}` seed, same as the
                            // array case below. This is the gap the
                            // array fix (b79e7fd5) called out as still
                            // latent for hashes: without it the seed
                            // keeps `Hash[Var, Var]`, and Go's empty-
                            // literal fallback guesses `map[string]
                            // string` — a guess that silently disagrees
                            // with every non-string write.
                            //
                            // …but only for a NON-nilable observation.
                            // `params[name] = path_parts[i]` observes
                            // `Str | Nil` because Ruby's `Array#[]` can
                            // return nil — a fact the emitted code does
                            // not share (Crystal's `Array(String)#[]`
                            // raises rather than returning nil, and the
                            // other targets index the same way). Stamping
                            // that onto the seed contradicts the emit
                            // instead of describing it, and outranks the
                            // author's declared `Hash[String, String]`
                            // return: Crystal rejected `match_parts` for
                            // returning `Hash(String, String | Nil)`.
                            // The placeholder is the better answer there
                            // — it lets each target's own declaration or
                            // default stand.
                            let nilable = match &refined {
                                Ty::Hash { value, .. } => match &**value {
                                    Ty::Nil => true,
                                    Ty::Union { variants } => {
                                        variants.iter().any(|t| matches!(t, Ty::Nil))
                                    }
                                    _ => false,
                                },
                                _ => false,
                            };
                            if let Some(&si) = hash_seed_idx.get(&(is_ivar, name)) {
                                if !nilable {
                                    let seed = &mut exprs[si];
                                    if let ExprNode::Assign { value, .. } = &mut *seed.node {
                                        value.ty = Some(refined.clone());
                                    }
                                    seed.ty = Some(refined);
                                }
                            }
                        }
                    }
                    for (is_ivar, name, elem) in pushes {
                        let bindings = if is_ivar {
                            &mut local_ctx.ivar_bindings
                        } else {
                            &mut local_ctx.local_bindings
                        };
                        if let Some(Ty::Array { elem: cur }) = bindings.get(&name) {
                            let new_elem = if matches!(**cur, Ty::Var { .. }) {
                                elem
                            } else {
                                union_of((**cur).clone(), elem)
                            };
                            let refined = Ty::Array { elem: Box::new(new_elem) };
                            bindings.insert(name.clone(), refined.clone());
                            // Retro-stamp the seed literal so decl-site
                            // emitters agree with the refined uses: the
                            // binding refinement alone left Go declaring
                            // `[]interface{}{}` while a later `.join`
                            // (typed off the refined binding) emitted
                            // `strings.Join` over it. Each subsequent
                            // push re-stamps with the widened union.
                            if let Some(&si) = array_seed_idx.get(&(is_ivar, name)) {
                                let seed = &mut exprs[si];
                                if let ExprNode::Assign { value, .. } = &mut *seed.node {
                                    value.ty = Some(refined.clone());
                                }
                                seed.ty = Some(refined);
                            }
                        }
                    }
                    // Nil-seeded scalar refinement — the `size = nil` …
                    // `size = v` pair, where the write is usually nested
                    // in a block and so invisible to this statement walk.
                    // Subtree-wide for the same reason `collect_array_
                    // pushes` is. Each observed write unions into the
                    // seed's running type and retro-stamps the seed
                    // (Assign + literal), so the decl, the binding and
                    // every later `.nil?` read all resolve to the same
                    // `Union[Nil, T]` instead of the seed's bare `Nil`.
                    if !nil_seed.is_empty() {
                        let mut writes: Vec<(Symbol, Ty)> = Vec::new();
                        collect_local_assignment_tys(&exprs[i], &mut writes);
                        for (name, ty) in writes {
                            let Some((si, acc)) = nil_seed.get(&name) else {
                                continue;
                            };
                            let (si, merged) = (*si, union_of(acc.clone(), ty));
                            if &merged == acc {
                                continue;
                            }
                            nil_seed.insert(name.clone(), (si, merged.clone()));
                            local_ctx
                                .local_bindings
                                .insert(name.clone(), merged.clone());
                            let seed = &mut exprs[si];
                            if let ExprNode::Assign { value, .. } = &mut *seed.node {
                                value.ty = Some(merged.clone());
                            }
                            seed.ty = Some(merged);
                        }
                    }
                    let e = &exprs[i];
                    // A conditional/loop whose CONDITION assigns a local
                    // (`if !(story = find) || story.gone?`, `while (line =
                    // gets)`) binds that local for the statements that
                    // FOLLOW the construct: the condition always
                    // evaluates, so the assignment ran regardless of which
                    // branch/iteration executed. The If/While handlers
                    // thread these into their own branches only — without
                    // this the pervasive find-or-return guard idiom leaves
                    // the local `Var` for every later read (and its
                    // `.build`/attribute cascade). Threaded BEFORE the
                    // diverging-then narrowing below so that block can
                    // further tighten the now-bound local to non-nil on
                    // the fall-through path. A Var/Bottom RHS is skipped so
                    // a good prior binding isn't clobbered.
                    if let ExprNode::If { cond, .. } | ExprNode::While { cond, .. } =
                        &*e.node
                    {
                        let mut cond_assigns: HashMap<Symbol, Ty> = HashMap::default();
                        collect_var_assignments_into(cond, &mut cond_assigns);
                        for (name, ty) in cond_assigns {
                            if ty.is_open() {
                                continue;
                            }
                            local_ctx.local_bindings.insert(name, ty);
                        }
                    }
                    // Diverging-then narrowing: `raise X if m.nil?` —
                    // when the then-branch always diverges (Bottom),
                    // control proceeds only via the else, so the
                    // negated narrowing applies to subsequent stmts.
                    // The TypeScript flow-narrowing analog Crystal
                    // gets natively; this gives the body-typer the
                    // same per-statement type tightening so the
                    // emitter can dispatch on the narrowed type.
                    if let ExprNode::If { cond, then_branch, else_branch } = &*e.node {
                        let then_diverges = matches!(then_branch.ty.as_ref(), Some(Ty::Bottom));
                        let else_empty = match &*else_branch.node {
                            ExprNode::Lit { value: crate::expr::Literal::Nil } => true,
                            ExprNode::Seq { exprs } => exprs.is_empty(),
                            _ => false,
                        };
                        if then_diverges && else_empty {
                            if let Some(pred) = narrowing::extract_narrowing_with(cond, &|n| self.self_attribute_ty(n, ctx)) {
                                local_ctx = narrowing::apply_narrowing(&local_ctx, &pred, false);
                            }
                        }
                        // The mirror image is `unless`, which the ingest
                        // spells `if cond; <nothing>; else <diverge>; end`:
                        // `return false unless current` continues only when
                        // `current` was truthy.
                        let then_empty = match &*then_branch.node {
                            ExprNode::Lit { value: crate::expr::Literal::Nil } => true,
                            ExprNode::Seq { exprs } => exprs.is_empty(),
                            _ => false,
                        };
                        let else_diverges = matches!(else_branch.ty.as_ref(), Some(Ty::Bottom));
                        if else_diverges && then_empty {
                            if let Some(pred) = narrowing::extract_narrowing_with(cond, &|n| self.self_attribute_ty(n, ctx)) {
                                local_ctx = narrowing::apply_narrowing(&local_ctx, &pred, true);
                            }
                        }
                    }
                }
                last
            }

            ExprNode::Assign { target, value } => {
                // Pre-propagate expected type into empty-container
                // literals: `@params = {}` where `@params : Hash[K, V]`
                // should type the empty Hash as `Hash<K, V>` rather
                // than `Hash<Var, Var>`. Without this, the strict
                // Crystal emit's `{} of K => V` default falls back to
                // `String => String` and trips against the declared
                // property type. Only fires for empty literals — non-
                // empty container literals already infer from their
                // contents.
                if let LValue::Ivar { name } = target {
                    if let Some(expected) = ctx.ivar_bindings.get(name).cloned() {
                        propagate_expected_to_empty_container(value, &expected);
                    }
                }
                let value_ty = self.analyze_expr(value, ctx);
                if let LValue::Attr { recv, .. } = target {
                    self.analyze_expr(recv, ctx);
                }
                if let LValue::Index { recv, index } = target {
                    self.analyze_expr(recv, ctx);
                    self.analyze_expr(index, ctx);
                }
                value_ty
            }

            ExprNode::OpAssign { target, value, .. } => {
                // Same recursion shape as Assign: walk target's child
                // Exprs + walk value. Short-circuit-aware type
                // narrowing (suppressing the union-widen on `||=` when
                // the target is already non-Option) is a follow-on;
                // for now the analyzer treats the result as the value
                // expression's type, which matches naive desugar.
                let value_ty = self.analyze_expr(value, ctx);
                if let LValue::Attr { recv, .. } = target {
                    self.analyze_expr(recv, ctx);
                }
                if let LValue::Index { recv, index } = target {
                    self.analyze_expr(recv, ctx);
                    self.analyze_expr(index, ctx);
                }
                value_ty
            }

            ExprNode::Yield { args } => {
                for a in args.iter_mut() { self.analyze_expr(a, ctx); }
                // In a view/layout, `yield` renders content → a String (the
                // lowering turns it into the `body` String param / a
                // `ViewHelpers.get_slot` String call). Elsewhere the block's
                // return type isn't tracked through the method's signature
                // today (would require generics — `def f<T> { () -> T } -> ...`),
                // so type as Untyped: the call site signed for an opaque block
                // return, and propagating Untyped lets downstream dispatch
                // resolve cleanly instead of bottoming out at Var.
                if ctx.in_view { Ty::Str } else { Ty::Untyped }
            }

            ExprNode::Raise { value } => {
                self.analyze_expr(value, ctx);
                // `raise` diverges — control transfers up the stack,
                // the expression produces no value. `Ty::Bottom`
                // drops out of unions so `if cond then raise else x
                // end` types as `typeof(x)`.
                Ty::Bottom
            }

            ExprNode::Next { value } | ExprNode::Break { value } => {
                if let Some(v) = value { self.analyze_expr(v, ctx); }
                // `next` / `break` are divergent at the source position
                // — `next` skips to the next iteration, `break` exits
                // the enclosing iterator. `Bottom` drops out of joins
                // so `if cond then next else x end` types cleanly as
                // `typeof(x)`.
                Ty::Bottom
            }

            ExprNode::Retry | ExprNode::Redo => {
                // `retry` re-runs the enclosing begin/rescue; `redo`
                // re-runs the current loop/block iteration. Both carry no
                // value and divert control, so they type as `Bottom` and
                // drop out of joins just like `next` / `break`.
                Ty::Bottom
            }

            ExprNode::ForwardArgs | ExprNode::ForwardKeywords => Ty::Untyped,
            ExprNode::ForwardKeywordsWithPairs { entries } => {
                for (key, value) in entries.iter_mut() {
                    self.analyze_expr(key, ctx);
                    self.analyze_expr(value, ctx);
                }
                Ty::Untyped
            }

            ExprNode::Splat { value } | ExprNode::KeywordSplat { value } => {
                // Splat propagates the inner expression's type
                // unchanged; the splat itself is a structural marker
                // for the surrounding Send/Array, not a transform.
                // The Send's argument-typing logic decides whether
                // the splatted Array's element type satisfies the
                // formal parameter's variadic/rest signature.
                self.analyze_expr(value, ctx)
            }

            ExprNode::MultiAssign { targets, value } => {
                self.analyze_expr(value, ctx);
                for target in targets.iter_mut() {
                    if let LValue::Attr { recv, .. } = target {
                        self.analyze_expr(recv, ctx);
                    }
                    if let LValue::Index { recv, index } = target {
                        self.analyze_expr(recv, ctx);
                        self.analyze_expr(index, ctx);
                    }
                }
                value.ty.clone().unwrap_or_else(unknown)
            }

            ExprNode::While { cond, body, .. } => {
                self.analyze_expr(cond, ctx);
                self.analyze_expr(body, ctx);
                Ty::Nil
            }

            ExprNode::Range { begin, end, .. } => {
                let begin_ty = begin.as_mut().map(|b| self.analyze_expr(b, ctx));
                let end_ty = end.as_mut().map(|e| self.analyze_expr(e, ctx));
                // Type as Class { Range } parameterized by the
                // bound's element type. Both endpoints share a type
                // in well-formed Ruby (`1..10`, `"a".."z"`); when
                // either endpoint is missing (`1..` / `..10`), use
                // the available one. Falls back to Untyped if neither
                // endpoint is known — a beginless+endless range has
                // no element-type signal.
                let elem = begin_ty
                    .or(end_ty)
                    .unwrap_or(Ty::Untyped);
                Ty::Class {
                    id: ClassId(Symbol::from("Range")),
                    args: vec![elem],
                }
            }
            ExprNode::Cast { value, target_ty } => {
                // Visit the value so its inner sub-exprs get typed,
                // then return the explicit cast target — that's the
                // whole point of Cast: assert this expression has
                // `target_ty` regardless of what the value's flow
                // type computed to.
                let _ = self.analyze_expr(value, ctx);
                // A class named inside a namespace means the lexically
                // nearest one (`Capabilities::Charge` in
                // `ShopifyPayments::Capability` is
                // `ShopifyPayments::Capabilities::Charge`).
                // A sorbet type alias is the type it names.
                let target_ty = &match ctx.lexical_self.as_ref().or(ctx.self_ty.as_ref()) {
                    Some(Ty::Class { id: scope, .. }) => expand_type_aliases(target_ty, scope.0.as_str(), self.classes()),
                    _ => expand_type_aliases(target_ty, "", self.classes()),
                };
                let resolved = match ctx.lexical_self.as_ref().or(ctx.self_ty.as_ref()) {
                    // As a signature reads it: `Set[String]` stays the
                    // stdlib generic (`is_stdlib_generic`).
                    Some(Ty::Class { id: scope, .. }) => expand_type_aliases_by(
                        target_ty,
                        scope.0.as_str(),
                        &|_, _| None,
                        &|id, scope| cref_class(id, scope, self.classes()).unwrap_or_else(|| id.clone()),
                        false,
                    ),
                _ => target_ty.clone(),
                };
                let missing = std::cell::RefCell::new(Vec::new());
                // Lowerers also synthesize casts whose identities were proven
                // against the full app before a reduced view/runtime registry
                // retypes them. Resolve source declarations at their boundary.
                // A declaration a registry already resolved
                // (DECLARED_TYPE_INDEXED) is not re-decided by a reduced one:
                // the model lowerer retypes `klass = self #: as
                // singleton(ActiveRecord::Base)` in a registry of the models
                // and runtime stubs, which names neither ActiveRecord::Base
                // nor Module, and check never sees that registry.
                if expr.decisions & crate::expr::SOURCE_TYPE_ASCRIPTION != 0
                    && expr.decisions & crate::expr::DECLARED_TYPE_INDEXED == 0
                {
                    resolved.map_class_ids(&|id| {
                        if !self.classes().contains_key(id) && !RUBY_TOP_LEVEL.contains(&id.0.as_str())
                            && !self.const_resolver.as_ref().is_some_and(|r| {
                                r.has_source_namespace(id.0.as_str()) || r.has_stdlib_namespace(id.0.as_str())
                            })
                            // A Ruby-family runtime port's class
                            // (`ActiveSupport::MessageEncryptor`); its
                            // reads are gated per target (runtime_reads).
                            && !crate::analyze::runtime_reads::is_runtime_declared_namespace(id.0.as_str()) {
                            missing.borrow_mut().push(id.0.as_str().to_owned());
                        }
                        id.clone()
                    });
                }
                let missing = missing.into_inner();
                if expr.decisions & crate::expr::DECLARED_TYPE_RUNTIME_CONSTANT != 0 {
                    // Decided by the registry that saw the gems' namespaces
                    // (the analyzer's); a lowerer's reduced registry does
                    // not re-decide it.
                } else if !missing.is_empty() {
                    // Ruby never evaluates the annotation. Where each name
                    // is a constant a gem the CRuby tree loads natively
                    // provides, that tree runs the authored value, typed as
                    // the opaque declared class; anything else stays refused.
                    let scope = match ctx.lexical_self.as_ref().or(ctx.self_ty.as_ref()) {
                        Some(Ty::Class { id, .. }) => id.0.as_str().to_string(),
                        _ => String::new(),
                    };
                    let owners: Vec<Result<(String, String), String>> =
                        missing.iter().map(|name| self.runtime_constant_provider(name, &scope)).collect();
                    let detail = if owners.iter().all(Result::is_ok) {
                        expr.decisions |= crate::expr::DECLARED_TYPE_RUNTIME_CONSTANT;
                        let (facts, gems): (Vec<String>, std::collections::BTreeSet<String>) =
                            owners.into_iter().flatten().unzip();
                        let gems: Vec<String> = gems.into_iter().collect();
                        format!(
                            "{} has no indexed declaration; {}; the CRuby tree loads {} natively, so the value runs and is typed as opaque",
                            missing.join(", "),
                            facts.join("; "),
                            gems.join(", "),
                        )
                    } else if let Some((name, gem, reason)) = missing.iter().find_map(|name| {
                        let gem = self.namespace_gem(name)?;
                        crate::project::gem_requires_rails(&gem).map(|reason| (name.clone(), gem, reason))
                    }) {
                        format!(
                            "{name} is in a namespace of the {gem} gem, whose load closure requires Rails ({reason}), so no emitted tree has it"
                        )
                    } else if let Some(refusal) = owners.iter().find_map(|o| o.as_ref().err().filter(|r| !r.is_empty())) {
                        refusal.clone()
                    } else {
                        format!("{} has no indexed declaration or modeled dependency", missing.join(", "))
                    };
                    expr.diagnostic = Some(crate::diagnostic::DiagnosticKind::Unsupported {
                        target: None,
                        construct: Symbol::from("declared type"),
                        detail,
                    });
                } else {
                    if expr.decisions & crate::expr::SOURCE_TYPE_ASCRIPTION != 0 {
                        expr.decisions |= crate::expr::DECLARED_TYPE_INDEXED;
                    }
                    if matches!(&expr.diagnostic, Some(crate::diagnostic::DiagnosticKind::Unsupported { construct, .. }) if construct.as_str() == "declared type") {
                        expr.diagnostic = None;
                    }
                }
                resolved
            }
        }
    }

}

/// Conditional writes, block writes and parameter rebinding invalidate identity.
fn forget_class_object_writes(expr: &Expr, ctx: &mut Ctx) {
    let mut names = Vec::new();
    match &*expr.node {
        ExprNode::Assign { target: LValue::Var { name, .. }, .. }
        | ExprNode::OpAssign { target: LValue::Var { name, .. }, .. } => names.push(name.clone()),
        ExprNode::MultiAssign { targets, .. } => {
            for target in targets {
                if let LValue::Var { name, .. } = target { names.push(name.clone()); }
            }
        }
        ExprNode::Lambda { params, rest_param, block_param, signature, .. } => {
            names.extend(params.iter().cloned());
            names.extend(rest_param.iter().cloned());
            names.extend(signature.names().cloned());
            names.extend(block_param.iter().cloned());
        }
        ExprNode::Let { name, .. } => names.push(name.clone()),
        ExprNode::CaseMatch { arms, .. } => {
            for arm in arms { arm.pattern.bound_names(&mut names); }
        }
        ExprNode::MatchRequired { pattern, .. } | ExprNode::MatchPredicate { pattern, .. } => pattern.bound_names(&mut names),
        ExprNode::BeginRescue { rescues, .. } => {
            for rescue in rescues { names.extend(rescue.binding.iter().cloned()); }
        }
        _ => {}
    }
    for name in names { ctx.class_objects.remove(&name); }
    expr.node.for_each_child(&mut |child| forget_class_object_writes(child, ctx));
}

// Literal / primitive types ---------------------------------------------

pub(super) fn lit_ty(lit: &Literal) -> Ty {
    match lit {
        Literal::Nil => Ty::Nil,
        Literal::Bool { .. } => Ty::Bool,
        Literal::Int { .. } | Literal::BigInt { .. } => Ty::Int,
        Literal::Float { .. } => Ty::Float,
        Literal::Str { .. } => Ty::Str,
        Literal::Sym { .. } => Ty::Sym,
        // A regex literal is a `Regexp`, and saying so is not a
        // refinement — it is the difference between `/Android/` being a
        // known object and being a gradual hole. Every `match?(/x/)` in
        // `runtime/ruby/` passed an untyped argument until now, and
        // `runtime/ruby/` prices every target.
        //
        // `Regexp` is the same kind of name `Time` is: the class exists
        // in the registry (`stdlib`), so a call ON the literal resolves
        // through the ordinary class walk rather than through a
        // per-literal table.
        Literal::Regex { .. } => {
            Ty::Class { id: ClassId(Symbol::from("Regexp")), args: vec![] }
        }
    }
}

/// The class path as written, joined with `::`.
fn written_class_id(path: &[Symbol]) -> ClassId {
    if let [name] = path {
        return ClassId(name.clone());
    }
    let mut name = String::new();
    for part in path {
        if !name.is_empty() {
            name.push_str("::");
        }
        name.push_str(part.as_str());
    }
    ClassId(Symbol::from(name))
}

/// Rubydex can resolve a relative path to a name with a lexical prefix:
/// `RateCalculator` inside `module PriceSupport` is
/// `PriceSupport::RateCalculator`. Ingest copies a concern's methods
/// into each class that includes it, and an emitter can nest a
/// declaration in another way. Thus the IR keeps the qualified path,
/// which names the same class from every lexical scope. A rooted path
/// (`::A`) already names its class. A path that the resolved name does
/// not end with (a constant alias) stays as written.
/// What `*value` adds to an array literal's elements: an array's or a
/// tuple's elements, a hash's pairs, nothing for `nil`. Ruby `to_a`s any
/// other value; one with no modeled element is kept as the one element
/// `[*object]` gives for an object that is not a collection.
fn splat_elements(ty: Ty) -> Option<Ty> {
    match ty {
        Ty::Nil => None,
        Ty::Tuple { elems } => elems.into_iter().reduce(union_of),
        Ty::Hash { key, value } => Some(Ty::Tuple { elems: vec![*key, *value] }),
        Ty::Union { variants } => variants.into_iter().filter_map(splat_elements).reduce(union_of),
        other => Some(other.collection_elem().unwrap_or(other)),
    }
}

fn qualify_resolved_path(path: &mut Vec<Symbol>, resolved: &ClassId) {
    if path.first().is_none_or(|head| head.as_str().starts_with("::")) {
        return;
    }
    let mut prefix = resolved.0.as_str();
    for segment in path.iter().rev() {
        let Some(rest) = prefix
            .strip_suffix(segment.as_str())
            .and_then(|rest| rest.strip_suffix("::"))
        else {
            return;
        };
        prefix = rest;
    }
    if !prefix.is_empty() {
        *path = resolved.0.as_str().split("::").map(Symbol::from).collect();
    }
}

/// The element type of `Kernel#Array(arg)`: an Array stays itself, nil
/// is empty, anything else is wrapped. None when the argument is not
/// known well enough to say (a Hash becomes pairs; not modeled).
fn kernel_array_elem(arg: &Ty) -> Option<Ty> {
    match arg {
        Ty::Array { elem } => Some((**elem).clone()),
        Ty::Tuple { elems } => Some(elems.iter().cloned().reduce(union_of).unwrap_or(Ty::Bottom)),
        // `to_a` of a relation is its records; of a range, its elements.
        Ty::Relation { of } => Some(Ty::Class { id: of.clone(), args: vec![] }),
        Ty::Class { id, args } if id.0.as_str() == "Range" => args.first().cloned(),
        Ty::Nil => Some(Ty::Bottom),
        Ty::Union { variants } => variants
            .iter()
            .map(kernel_array_elem)
            .collect::<Option<Vec<_>>>()
            .map(|elems| elems.into_iter().reduce(union_of).unwrap_or(Ty::Bottom)),
        // Any other class may answer `to_ary`/`to_a` (a Struct, a Set,
        // anything Enumerable), which `Array()` then unpacks.
        Ty::Var { .. } | Ty::Untyped | Ty::Hash { .. } | Ty::Record { .. } | Ty::Class { .. } => None,
        scalar => Some(scalar.clone()),
    }
}

pub(super) fn unknown() -> Ty {
    Ty::Var { var: TyVar(0) }
}

/// Stamp the expected type onto an empty Hash/Array literal so the
/// body-typer's normal inference (which would otherwise leave the
/// inner type as `Var`) carries the surrounding context. Used by
/// `Assign` to forward an ivar/var's declared type into a bare `{}`
/// or `[]` RHS — strict targets need the concrete shape (Crystal's
/// `{} of K => V`, TS's `Record<string, V>`) and `Var` doesn't
/// render. Non-empty literals are left alone; their inferred element
/// types already drive the result.
///
/// `expected` may arrive as `T | Nil` because pass-B reseeded ivars
/// union the assignment type with Nil to model first-read-before-
/// initialize; peel that wrapper before matching the container shape.
fn propagate_expected_to_empty_container(value: &mut Expr, expected: &Ty) {
    let peeled = expected.peel_nilable();
    match &mut *value.node {
        ExprNode::Hash { entries, .. } if entries.is_empty() => {
            if matches!(peeled, Ty::Hash { .. }) {
                value.ty = Some(peeled.clone());
            }
        }
        ExprNode::Array { elements, .. } if elements.is_empty() => {
            if matches!(peeled, Ty::Array { .. }) {
                value.ty = Some(peeled.clone());
            }
        }
        _ => {}
    }
}

/// Type a single destructuring target of `a, b, ... = rhs` at
/// position `index`. Three RHS shapes carry a per-position type:
///   - `Array<T>` — every scalar target gets the element type `T`
///     (`@a, @b = list` where `list : Array<Story>`).
///   - `Tuple` — each target takes its positional element type.
///   - `Untyped` — the gradual escape flows to every target (the
///     pervasive `@x, @y = get_from_cache { ... }` /
///     `yield`-returning idiom, whose return type the analyzer can't
///     pin). Untyped reads surface as a gradual warning, not a hard
///     `ivar_unresolved` error.
/// Anything else yields `None` (no usable signal — leave the target
/// unbound rather than guess).
pub(crate) fn multiassign_target_ty(rhs: &Option<Ty>, index: usize) -> Option<Ty> {
    match rhs {
        Some(Ty::Array { elem }) => Some((**elem).clone()),
        // Destructuring a relation materializes it — each scalar
        // target gets the element model, same as `Array<of>`.
        Some(Ty::Relation { of }) => Some(Ty::Class { id: of.clone(), args: vec![] }),
        Some(Ty::Tuple { elems }) => elems.get(index).cloned(),
        Some(Ty::Untyped) => Some(Ty::Untyped),
        _ => None,
    }
}

/// Walk an Expr collecting every `Assign { target: LValue::Var, .. }`
/// it contains, recording `name → expr.ty`. Used to thread local
/// bindings produced by an embedded assignment (`(x = find_by(...))`)
/// from a `BoolOp::And` left-arm or `If::cond` into the subsequent
/// arm's Ctx. The Seq-statement-level handler covers the common case
/// of top-level `x = ...` statements; this covers the nested form.
///
/// Descent stops at scope-introducing nodes (`Lambda`, `Let`) so a
/// block parameter assignment doesn't leak out.
/// Collect array-accumulator pushes (`arr << x`, `arr.push/append/
/// unshift/prepend(x, …)`) anywhere in `expr`, as `(is_ivar, name,
/// element_ty)` per push whose receiver is a plain local or ivar.
/// Recurses through loop/branch bodies and blocks (via `for_each_child`)
/// so the accumulator idiom — seed `arr = []` at the top, fill it inside
/// a nested `while`/`each` — is caught. `concat` is excluded: it flattens
/// its argument, so the pushed element type isn't the argument's type.
/// A pushed arg typed `Var`/`Bottom` contributes nothing. Callers refine
/// only bindings that already exist as `Array[_]`, so an inner shadowing
/// local of the same name at worst adds imprecision, never a wrong write.
fn collect_array_pushes(expr: &Expr, out: &mut Vec<(bool, Symbol, Ty)>) {
    if let ExprNode::Send { recv: Some(recv), method, args, .. } = &*expr.node {
        if matches!(method.as_str(), "<<" | "push" | "append" | "unshift" | "prepend") {
            let slot = match &*recv.node {
                ExprNode::Ivar { name } => Some((true, name.clone())),
                ExprNode::Var { name, .. } => Some((false, name.clone())),
                _ => None,
            };
            if let Some((is_ivar, name)) = slot {
                let mut pushed: Option<Ty> = None;
                for a in args {
                    let Some(t) = a.ty.clone() else { continue };
                    if t.is_open() {
                        continue;
                    }
                    pushed = Some(match pushed.take() {
                        Some(p) => union_of(p, t),
                        None => t,
                    });
                }
                if let Some(t) = pushed {
                    out.push((is_ivar, name, t));
                }
            }
        }
    }
    expr.node.for_each_child(&mut |child| collect_array_pushes(child, out));
}

/// Every container index-write in `expr`'s subtree whose receiver is a
/// plain ivar/local binding, as `(is_ivar, name, written_ty)`. Covers
/// both `h[k] = v` and `h[k] ||= v`.
///
/// A computed receiver (`a.b[k] = v`) has no binding to refine and is
/// skipped. Open written types are skipped for the same reason as in
/// [`collect_array_pushes`] — they carry nothing to union in.
fn collect_hash_index_writes(expr: &Expr, out: &mut Vec<(bool, Symbol, Ty)>) {
    let write = match &*expr.node {
        ExprNode::Assign { target: LValue::Index { recv, .. }, value }
        | ExprNode::OpAssign { target: LValue::Index { recv, .. }, value, .. } => {
            value.ty.clone().map(|t| (recv, t))
        }
        // `h[k] = v` reaches the analyzer as a two-arg `[]=` send, NOT
        // as an `LValue::Index` assign — the LValue shape only survives
        // for the compound `h[k] ||= v`. Matching just the LValue (as
        // this refinement originally did) therefore missed the plain
        // write, which is the form the accumulator idiom actually uses.
        ExprNode::Send { recv: Some(r), method, args, .. }
            if matches!(method.as_str(), "[]=" | "store") && args.len() == 2 =>
        {
            args[1].ty.clone().map(|t| (r, t))
        }
        _ => None,
    };
    if let Some((recv, rhs)) = write {
        if !rhs.is_open() {
            let slot = match &*recv.node {
                ExprNode::Ivar { name } => Some((true, name.clone())),
                ExprNode::Var { name, .. } => Some((false, name.clone())),
                _ => None,
            };
            if let Some((is_ivar, name)) = slot {
                out.push((is_ivar, name, rhs));
            }
        }
    }
    expr.node.for_each_child(&mut |child| collect_hash_index_writes(child, out));
}

/// Every local-variable assignment in `expr`'s subtree that carries a
/// usable stamped type, as `(name, assigned_ty)`. The scalar counterpart
/// of [`collect_array_pushes`]: the nil-accumulator idiom writes from
/// inside a block (`opts.each { |k, v| size = v }`) while the `size =
/// nil` seed sits at the enclosing statement level, so a same-level
/// match would never see the write.
///
/// Distinct from [`collect_var_assignments_into`], which walks only the
/// condition-expression node shapes and keeps the LAST write per name;
/// this one is a full subtree walk and reports EVERY write, because the
/// caller unions them rather than overwriting.
///
/// Open types (`Var` / `Bottom`) are skipped — an unresolved write
/// carries no information to union in, and folding it would erase a
/// good type. `Untyped` is deliberately NOT skipped: it is an
/// author-signed "no constraint here", and a hash value flowing into
/// the accumulator must widen the seed rather than leave it narrow.
fn collect_local_assignment_tys(expr: &Expr, out: &mut Vec<(Symbol, Ty)>) {
    if let ExprNode::Assign { target: LValue::Var { name, .. }, value } = &*expr.node {
        if let Some(t) = value.ty.clone() {
            if !t.is_open() {
                out.push((name.clone(), t));
            }
        }
    }
    expr.node.for_each_child(&mut |child| collect_local_assignment_tys(child, out));
}

/// The local assignments a statement list makes at ITS OWN level — the
/// statement itself, or each statement of a `Seq` — with the value's
/// stamped type. Blocks and nested bodies are not entered: a local first
/// assigned inside a block does not outlive it. Open types are skipped,
/// as in [`collect_local_assignment_tys`].
fn top_level_local_assigns(expr: &Expr, out: &mut Vec<(Symbol, Ty)>) {
    let stmts: Vec<&Expr> = match &*expr.node {
        ExprNode::Seq { exprs } => exprs.iter().collect(),
        _ => vec![expr],
    };
    for s in stmts {
        if let ExprNode::Assign { target: LValue::Var { name, .. }, value } = &*s.node {
            if let Some(t) = value.ty.clone() {
                if !t.is_open() {
                    out.push((name.clone(), t));
                }
            }
        }
    }
}

fn collect_var_assignments_into(expr: &Expr, out: &mut HashMap<Symbol, Ty>) {
    match &*expr.node {
        ExprNode::Assign { target: LValue::Var { name, .. }, value } => {
            if let Some(ty) = value.ty.clone() {
                out.insert(name.clone(), ty);
            }
            collect_var_assignments_into(value, out);
        }
        ExprNode::BoolOp { left, right, .. } => {
            collect_var_assignments_into(left, out);
            collect_var_assignments_into(right, out);
        }
        ExprNode::Send { recv, args, .. } => {
            if let Some(r) = recv {
                collect_var_assignments_into(r, out);
            }
            for a in args {
                collect_var_assignments_into(a, out);
            }
        }
        ExprNode::Seq { exprs } => {
            for e in exprs {
                collect_var_assignments_into(e, out);
            }
        }
        // Scope-introducing forms: Lambda's body and Let's binding
        // belong to inner scopes; their assignments don't escape.
        ExprNode::Lambda { .. } | ExprNode::Let { .. } => {}
        _ => {}
    }
}

/// `default: (no_default = true)`: a local a parameter default assigns is
/// in scope for the whole body, and is nil on a call that passed the
/// argument (the default never ran), so it binds as `T | nil`. Ruby's
/// own idiom for "was this keyword given". Reads the defaults' types, so
/// they must be typed first. A parameter of the same name is left alone.
pub fn bind_default_assigned_locals(params: &[crate::dialect::Param], ctx: &mut Ctx) {
    for default in params.iter().filter_map(|p| p.default.as_ref()) {
        let mut assigned = HashMap::default();
        collect_var_assignments_into(default, &mut assigned);
        for (name, ty) in assigned {
            if !params.iter().any(|p| p.name == name) {
                ctx.local_bindings.insert(name, union_of(ty, Ty::Nil));
            }
        }
    }
}

/// `{}` or `[]` as written: a literal with no entries.
fn is_empty_container_literal(e: &Expr) -> bool {
    match &*e.node {
        ExprNode::Hash { entries, .. } => entries.is_empty(),
        ExprNode::Array { elements, .. } => elements.is_empty(),
        _ => false,
    }
}

/// Both Hashes or both Arrays.
fn same_container_spine(known: &Ty, literal: &Ty) -> bool {
    matches!(
        (known, literal),
        (Ty::Hash { .. }, Ty::Hash { .. }) | (Ty::Array { .. }, Ty::Array { .. })
    )
}

pub(crate) fn union_of(a: Ty, b: Ty) -> Ty {
    // Bottom is the divergent-expression type — the branch carrying
    // it doesn't contribute a value, so it drops out of joins.
    // Mirrors Crystal's `Type.merge` filter on `NoReturnType`.
    // `if cond then raise else x end` types as `typeof(x)` instead
    // of `typeof(x) | Nil`.
    if matches!(a, Ty::Bottom) {
        return b;
    }
    if matches!(b, Ty::Bottom) {
        return a;
    }
    if a == b {
        return a;
    }
    // Structural join for same-shape generic containers — `Hash<A,B>
    // | Hash<C,D>` is more usefully expressed as `Hash<A|C, B|D>`
    // than as a flat Union, because dispatch on `.fetch` / `.[]` /
    // etc. operates on the Hash spine regardless of inner types.
    // The flat Union form would push every consumer to fan out per
    // variant. The if-narrowing case (`raw.is_a?(Hash) ? raw : {}`)
    // is the canonical producer — narrowed branch types `Hash<K,V>`,
    // empty-literal else branch types `Hash<Var,Var>`; the join
    // should be `Hash<K|Var, V|Var>`, which collapses to `Hash<K,V>`
    // once inference resolves the Vars.
    match (&a, &b) {
        (Ty::Hash { key: k1, value: v1 }, Ty::Hash { key: k2, value: v2 }) => {
            return Ty::Hash {
                key: Box::new(union_of((**k1).clone(), (**k2).clone())),
                value: Box::new(union_of((**v1).clone(), (**v2).clone())),
            };
        }
        (Ty::Array { elem: e1 }, Ty::Array { elem: e2 }) => {
            return Ty::Array {
                elem: Box::new(union_of((**e1).clone(), (**e2).clone())),
            };
        }
        _ => {}
    }
    // Flatten nested unions and drop duplicate variants. Without this,
    // joining `Int` into an existing `Int` (or `Union[Int]`) produced
    // `Int | Int` / `Union[Union[Int], Int]` — a degenerate union that
    // breaks binop classification (`Int > (Int | Int)`) and, when an
    // ivar like `@page` accumulates across many actions into the
    // layout-ivar union, nests dozens of levels deep.
    //
    // The result is CANONICAL: at most one Hash and one Array spine
    // (merged pointwise, so the structural join above also fires when
    // a container arrives wrapped in a Union), variants in a fixed
    // sort order. Canonical form is what makes `union_of` commutative
    // and associative under derived `==` — fixpoint convergence checks
    // and iteration order both depend on that; the lattice-law tests
    // in this file's test module enforce it.
    let mut variants = Vec::new();
    push_union_variants(a, &mut variants);
    push_union_variants(b, &mut variants);
    Ty::canonicalize_variants(&mut variants);
    match variants.len() {
        0 => Ty::Bottom,
        1 => variants.into_iter().next().unwrap(),
        _ => Ty::Union { variants },
    }
}

/// Append `t`'s constituent variants to `out`, flattening nested
/// `Union`s and skipping any variant already present (structural
/// equality) so the result carries each distinct type once. A `Hash`
/// or `Array` merges pointwise into an existing same-spine variant
/// instead of appending, keeping the union to one spine per container
/// kind regardless of the order joins arrive in.
fn push_union_variants(t: Ty, out: &mut Vec<Ty>) {
    match t {
        Ty::Union { variants } => {
            for v in variants {
                push_union_variants(v, out);
            }
        }
        Ty::Hash { key, value } => {
            for existing in out.iter_mut() {
                if let Ty::Hash { key: k0, value: v0 } = existing {
                    *k0 = Box::new(union_of((**k0).clone(), *key));
                    *v0 = Box::new(union_of((**v0).clone(), *value));
                    return;
                }
            }
            out.push(Ty::Hash { key, value });
        }
        Ty::Array { elem } => {
            for existing in out.iter_mut() {
                if let Ty::Array { elem: e0 } = existing {
                    *e0 = Box::new(union_of((**e0).clone(), *elem));
                    return;
                }
            }
            out.push(Ty::Array { elem });
        }
        other => {
            if !out.contains(&other) {
                out.push(other);
            }
        }
    }
}

pub(super) fn union_many(tys: Vec<Ty>) -> Ty {
    // Fold through `union_of` so the same Bottom-filtering, structural
    // container join, and flatten/dedup normalization applies.
    let mut iter = tys.into_iter().filter(|t| !matches!(t, Ty::Bottom));
    match iter.next() {
        None => Ty::Bottom,
        Some(first) => iter.fold(first, union_of),
    }
}

/// What the PostgreSQL CRuby tree's `ActiveRecord::Base` answers, read
/// from the runtime files that tree composes it from, so a call on a
/// receiver typed as the base itself (`record = self #: as
/// ActiveRecord::Base`, Core's BumpsShopVersion) dispatches as Ruby does
/// when the runtime defines the method, and is refused by name when it
/// does not. A PostgreSQL app emits only that tree (`lower::try_guard::
/// runtime_tryable`), so its Base is the one the emitted models subclass.
///
/// The files:
/// - `runtime/ruby/active_record/*.rb`, the shared Ruby-family base
///   (`id`, `table_name`, `where`, `type_for_attribute`, `arel_table`);
/// - every `runtime/spinel/cruby/*.rb`, the Rails module ancestry the
///   PostgreSQL bootstrap carries (`project::apply_postgres_bootstrap`:
///   Core, Persistence, Callbacks, ConnectionHandling, Transactions'
///   `current_transaction`);
/// - the CRuby overlay's Active Record reopens the PostgreSQL boot loads
///   (`active_record_bang.rb`, `_serialization`, `_relation_ext`,
///   `_postgresql_adapter`, `_statement_errors`; not the SQLite adapter's);
/// - the ActiveModel ports Base includes there that the bootstrap always
///   carries (`active_model_naming.rb`'s Conversion,
///   `active_model_serialization.rb`'s Serializers::JSON), and
///   `active_support_tryable.rb`'s `Object#try`.
///
/// Read as Ruby builds the ancestry: a body's `def`s, accessors, aliases,
/// `define_method(:literal)` and `delegate`s on the side they define, with
/// `private`/`protected`; its `include`/`prepend` and `extend` (and
/// `singleton_class.prepend`, `class << self` mixins); and a hook's
/// `base.extend(X)` (`self.included(base)`, `self.append_features(base)`).
/// `remove_method :name` drops the body's own definition, so a module
/// below it answers (active_record_core.rb's `reload`). A name built
/// from a literal list (`[:save, :create].each do |event|
/// define_method(:"before_#{event}")`) is read once per element; one
/// computed any other way at load time is not, so the answer stays a
/// refusal: under-reading is the safe side.
mod runtime_base {
    use crate::det::{HashMap, HashSet};
    use std::sync::OnceLock;

    const INSTANCE: usize = 0;
    const SINGLETON: usize = 1;

    /// One class or module body, across every file that opens it.
    #[derive(Default)]
    struct Namespace {
        /// Per side: method name → whether it is public.
        own: [HashMap<String, bool>; 2],
        /// Per side: `remove_method` / `undef_method` names.
        removed: [HashSet<String>; 2],
        /// Per side: mixins as `(lexical scope, written name, prepended)`.
        mixins: [Vec<(String, String, bool)>; 2],
        /// Per side of the includer: what this module's `included` /
        /// `append_features` hook mixes into it (`base.extend(ClassMethods)`).
        includer_mixins: [Vec<(String, String, bool)>; 2],
    }

    #[derive(Default)]
    struct Walker {
        spaces: HashMap<String, Namespace>,
        /// The block variable of a literal `[:a, :b].each do |event|` the
        /// walk is inside, and the element it stands for this pass.
        each_binding: Option<(Vec<u8>, String)>,
    }

    /// The body a statement runs in: its namespace, the side a bare `def`
    /// defines on, and the default visibility so far.
    struct Body {
        owner: String,
        side: usize,
        public: bool,
    }

    fn text(bytes: &[u8]) -> String {
        String::from_utf8_lossy(bytes).into_owned()
    }

    fn symbol(node: &ruby_prism::Node<'_>) -> Option<String> {
        if let Some(sym) = node.as_symbol_node() {
            return Some(text(sym.unescaped()));
        }
        node.as_string_node().map(|s| text(s.unescaped()))
    }

    fn constant(node: &ruby_prism::Node<'_>) -> Option<String> {
        (node.as_constant_read_node().is_some() || node.as_constant_path_node().is_some())
            .then(|| text(node.location().as_slice()))
    }

    fn is_self(node: Option<ruby_prism::Node<'_>>) -> bool {
        node.is_none_or(|n| n.as_self_node().is_some())
    }

    impl Walker {
        /// A literal method name: a symbol or string, or an interpolated
        /// symbol whose only interpolation is the `each` block variable.
        fn name_of(&self, node: &ruby_prism::Node<'_>) -> Option<String> {
            if let Some(literal) = symbol(node) {
                return Some(literal);
            }
            let (var, value) = self.each_binding.as_ref()?;
            let mut out = String::new();
            for part in node.as_interpolated_symbol_node()?.parts().iter() {
                if let Some(text_part) = part.as_string_node() {
                    out.push_str(&text(text_part.unescaped()));
                    continue;
                }
                let embedded = part.as_embedded_statements_node()?.statements()?;
                let mut statements = embedded.body().iter();
                let (Some(only), None) = (statements.next(), statements.next()) else { return None };
                let read = only.as_local_variable_read_node()?;
                if read.name().as_slice() != var.as_slice() {
                    return None;
                }
                out.push_str(value);
            }
            Some(out)
        }

        /// `[:save, :create].each do |event| define_method(:"before_#{event}") ... end`
        /// at body level: the block body once per element, with the
        /// variable bound (active_record_callbacks.rb's model callbacks).
        fn literal_each(&mut self, call: &ruby_prism::CallNode<'_>, body: &mut Body) -> bool {
            let Some(array) = call.receiver().and_then(|r| r.as_array_node()) else { return false };
            let Some(block) = call.block().and_then(|b| b.as_block_node()) else { return false };
            let elements: Option<Vec<String>> = array.elements().iter().map(|e| symbol(&e)).collect();
            let Some(elements) = elements else { return false };
            let var = block
                .parameters()
                .and_then(|p| p.as_block_parameters_node())
                .and_then(|p| p.parameters())
                .filter(|p| p.requireds().iter().count() == 1 && p.optionals().iter().next().is_none() && p.rest().is_none())
                .and_then(|p| p.requireds().iter().next())
                .and_then(|p| p.as_required_parameter_node())
                .map(|p| p.name().as_slice().to_vec());
            let (Some(var), true) = (var, block.body().is_some()) else { return false };
            if self.each_binding.is_some() {
                return false;
            }
            for element in elements {
                self.each_binding = Some((var.clone(), element));
                self.statements(block.body(), body);
            }
            self.each_binding = None;
            true
        }

        fn space(&mut self, owner: &str) -> &mut Namespace {
            self.spaces.entry(owner.to_string()).or_default()
        }

        fn define(&mut self, body: &Body, side: usize, name: String, public: bool) {
            self.space(&body.owner).own[side].insert(name, public);
        }

        fn statements(&mut self, node: Option<ruby_prism::Node<'_>>, body: &mut Body) {
            let Some(node) = node else { return };
            match node.as_statements_node() {
                Some(list) => {
                    for statement in list.body().iter() {
                        self.statement(&statement, body);
                    }
                }
                None => self.statement(&node, body),
            }
        }

        fn nested(&mut self, body: &Body, path: &[u8], inner: Option<ruby_prism::Node<'_>>) {
            let written = text(path);
            let owner = match written.strip_prefix("::") {
                Some(top) => top.to_string(),
                None if body.owner.is_empty() => written,
                None => format!("{}::{written}", body.owner),
            };
            self.space(&owner);
            self.statements(inner, &mut Body { owner, side: INSTANCE, public: true });
        }

        fn statement(&mut self, node: &ruby_prism::Node<'_>, body: &mut Body) {
            if let Some(class) = node.as_class_node() {
                self.nested(body, class.constant_path().location().as_slice(), class.body());
            } else if let Some(module) = node.as_module_node() {
                self.nested(body, module.constant_path().location().as_slice(), module.body());
            } else if let Some(singleton) = node.as_singleton_class_node() {
                if singleton.expression().as_self_node().is_some() {
                    let mut inner = Body { owner: body.owner.clone(), side: SINGLETON, public: true };
                    self.statements(singleton.body(), &mut inner);
                }
            } else if let Some(def) = node.as_def_node() {
                self.def(&def, body);
            } else if let Some(alias) = node.as_alias_method_node() {
                if let (Some(new), Some(old)) = (symbol(&alias.new_name()), symbol(&alias.old_name())) {
                    let public = self.space(&body.owner).own[body.side].get(&old).copied().unwrap_or(body.public);
                    self.define(body, body.side, new, public);
                }
            } else if let Some(call) = node.as_call_node() {
                self.call(&call, body);
            } else if let Some(branch) = node.as_if_node() {
                self.statements(branch.statements().map(|s| s.as_node()), body);
                self.statements(branch.subsequent(), body);
            } else if let Some(other) = node.as_else_node() {
                self.statements(other.statements().map(|s| s.as_node()), body);
            } else if let Some(branch) = node.as_unless_node() {
                self.statements(branch.statements().map(|s| s.as_node()), body);
                self.statements(branch.else_clause().map(|e| e.as_node()), body);
            } else if let Some(block) = node.as_begin_node() {
                self.statements(block.statements().map(|s| s.as_node()), body);
                self.statements(block.ensure_clause().and_then(|e| e.statements()).map(|s| s.as_node()), body);
            }
        }

        fn def(&mut self, def: &ruby_prism::DefNode<'_>, body: &Body) -> String {
            let name = text(def.name().as_slice());
            let on_self = def.receiver().is_some_and(|r| r.as_self_node().is_some());
            if def.receiver().is_some() && !on_self {
                return name;
            }
            let (side, public) = if on_self { (SINGLETON, true) } else { (body.side, body.public) };
            // `initialize` is private wherever it is written.
            self.define(body, side, name.clone(), public && name != "initialize");
            if on_self && matches!(name.as_str(), "included" | "append_features" | "prepended") {
                self.hook(def, body);
            }
            name
        }

        /// `base.extend(X)` / `base.include(X)` / `base.prepend(X)` in a
        /// module's inclusion hook: what every includer gets.
        fn hook(&mut self, def: &ruby_prism::DefNode<'_>, body: &Body) {
            struct Mixins<'b>(&'b [u8], Vec<(usize, String, bool)>);
            impl<'pr> ruby_prism::Visit<'pr> for Mixins<'_> {
                fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
                    let on_base = node
                        .receiver()
                        .and_then(|r| r.as_local_variable_read_node())
                        .is_some_and(|r| r.name().as_slice() == self.0);
                    let side = match node.name().as_slice() {
                        b"extend" => Some((SINGLETON, false)),
                        b"include" => Some((INSTANCE, false)),
                        b"prepend" => Some((INSTANCE, true)),
                        _ => None,
                    };
                    if let (true, Some((side, prepended))) = (on_base, side) {
                        for arg in node.arguments().iter().flat_map(|a| a.arguments().iter()) {
                            if let Some(written) = constant(&arg) {
                                self.1.push((side, written, prepended));
                            }
                        }
                    }
                    ruby_prism::visit_call_node(self, node);
                }
            }
            let Some(base) = def
                .parameters()
                .and_then(|p| p.requireds().iter().next())
                .and_then(|p| p.as_required_parameter_node())
                .map(|p| p.name().as_slice().to_vec())
            else {
                return;
            };
            let Some(inner) = def.body() else { return };
            let mut found = Mixins(&base, Vec::new());
            ruby_prism::Visit::visit(&mut found, &inner);
            let owner = body.owner.clone();
            for (side, written, prepended) in found.1 {
                self.space(&owner).includer_mixins[side].push((owner.clone(), written, prepended));
            }
        }

        fn call(&mut self, call: &ruby_prism::CallNode<'_>, body: &mut Body) {
            let name = call.name().as_slice();
            let args: Vec<ruby_prism::Node<'_>> = call.arguments().iter().flat_map(|a| a.arguments().iter()).collect();
            let symbols = || args.iter().filter_map(symbol).collect::<Vec<_>>();
            if name == b"each" && self.literal_each(call, body) {
                return;
            }
            // `singleton_class.prepend(X)` / `.include(X)`.
            if let Some(receiver) = call.receiver() {
                let on_singleton = receiver.as_call_node().is_some_and(|r| {
                    r.name().as_slice() == b"singleton_class" && is_self(r.receiver()) && r.arguments().is_none()
                });
                if on_singleton && matches!(name, b"include" | b"prepend") {
                    for written in args.iter().filter_map(constant) {
                        self.space(&body.owner).mixins[SINGLETON].push((body.owner.clone(), written, name == b"prepend"));
                    }
                }
                if receiver.as_self_node().is_none() {
                    return;
                }
            }
            let side = body.side;
            match name {
                b"private" | b"protected" | b"public" => {
                    let public = name == b"public";
                    if args.is_empty() {
                        body.public = public;
                    }
                    for arg in &args {
                        let named = match arg.as_def_node() {
                            Some(def) => Some(self.def(&def, body)),
                            None => symbol(arg),
                        };
                        if let Some(named) = named {
                            self.define(body, side, named, public);
                        }
                    }
                }
                b"private_class_method" | b"public_class_method" => {
                    for named in symbols() {
                        self.define(body, SINGLETON, named, name == b"public_class_method");
                    }
                }
                b"include" | b"prepend" | b"extend" => {
                    let target = if name == b"extend" { SINGLETON } else { side };
                    for written in args.iter().filter_map(constant) {
                        self.space(&body.owner).mixins[target].push((body.owner.clone(), written, name == b"prepend"));
                    }
                }
                b"attr_reader" | b"attr_writer" | b"attr_accessor" => {
                    for named in symbols() {
                        if name != b"attr_writer" {
                            self.define(body, side, named.clone(), body.public);
                        }
                        if name != b"attr_reader" {
                            self.define(body, side, format!("{named}="), body.public);
                        }
                    }
                }
                b"alias_method" => {
                    if let [new, old, ..] = symbols().as_slice() {
                        let public = self.space(&body.owner).own[side].get(old).copied().unwrap_or(body.public);
                        self.define(body, side, new.clone(), public);
                    }
                }
                b"define_method" => {
                    if let Some(named) = args.first().and_then(|a| self.name_of(a)) {
                        self.define(body, side, named, body.public);
                    }
                }
                b"define_singleton_method" => {
                    if let Some(named) = args.first().and_then(|a| self.name_of(a)) {
                        self.define(body, SINGLETON, named, true);
                    }
                }
                // `delegate :a, :b, to: :x`; a `prefix:` renames them.
                b"delegate" => {
                    let prefixed = args.iter().filter_map(|a| a.as_keyword_hash_node()).any(|hash| {
                        hash.elements().iter().any(|e| e.as_assoc_node().is_some_and(|a| symbol(&a.key()).as_deref() == Some("prefix")))
                    });
                    if !prefixed {
                        for named in symbols() {
                            self.define(body, side, named, body.public);
                        }
                    }
                }
                b"class_attribute" | b"cattr_accessor" | b"mattr_accessor" | b"cattr_reader" | b"mattr_reader" => {
                    let writer = !matches!(name, b"cattr_reader" | b"mattr_reader");
                    for named in symbols() {
                        for side in [INSTANCE, SINGLETON] {
                            self.define(body, side, named.clone(), true);
                            if writer {
                                self.define(body, side, format!("{named}="), true);
                            }
                            if name == b"class_attribute" {
                                self.define(body, side, format!("{named}?"), true);
                            }
                        }
                    }
                }
                b"remove_method" | b"undef_method" => {
                    for named in symbols() {
                        self.space(&body.owner).removed[side].insert(named);
                    }
                }
                _ => {}
            }
        }

        /// `written` as Ruby resolves it from `scope`'s nesting: innermost
        /// first, then outward, then top level.
        fn resolve(&self, scope: &str, written: &str) -> Option<String> {
            if let Some(top) = written.strip_prefix("::") {
                return self.spaces.contains_key(top).then(|| top.to_string());
            }
            let mut at = Some(scope).filter(|s| !s.is_empty());
            while let Some(prefix) = at {
                let candidate = format!("{prefix}::{written}");
                if self.spaces.contains_key(&candidate) {
                    return Some(candidate);
                }
                at = prefix.rsplit_once("::").map(|(outer, _)| outer);
            }
            self.spaces.contains_key(written).then(|| written.to_string())
        }

        /// The methods `side` of `name` answers, by name → public, as its
        /// ancestry orders them: included modules, then its own (less what
        /// it removed), then prepended modules, each overriding the last.
        fn answers(&self, name: &str, side: usize, seen: &mut HashSet<(String, usize)>, out: &mut HashMap<String, bool>) {
            if !seen.insert((name.to_string(), side)) {
                return;
            }
            let Some(space) = self.spaces.get(name) else { return };
            let mut mixins: Vec<(String, bool, bool)> = Vec::new();
            for (scope, written, prepended) in &space.mixins[side] {
                if let Some(module) = self.resolve(scope, written) {
                    mixins.push((module, *prepended, false));
                }
            }
            // An included module's own hook mixes into this side too.
            for (scope, written, _) in &space.mixins[INSTANCE] {
                let Some(module) = self.resolve(scope, written) else { continue };
                let Some(included) = self.spaces.get(&module) else { continue };
                for (scope, written, prepended) in &included.includer_mixins[side] {
                    if let Some(hooked) = self.resolve(scope, written) {
                        mixins.push((hooked, *prepended, true));
                    }
                }
            }
            for (module, _, _) in mixins.iter().filter(|(_, prepended, _)| !prepended) {
                self.answers(module, INSTANCE, seen, out);
            }
            for (method, public) in &space.own[side] {
                if !space.removed[side].contains(method) {
                    out.insert(method.clone(), *public);
                }
            }
            for (module, _, _) in mixins.iter().filter(|(_, prepended, _)| *prepended) {
                self.answers(module, INSTANCE, seen, out);
            }
        }
    }

    const OVERLAY: &str = "runtime/spinel/scaffold/ruby_overlay/runtime/";
    const OVERLAY_FILES: &[&str] = &[
        "active_record_bang.rb",
        "active_record_serialization.rb",
        "active_record_relation_ext.rb",
        "active_record_postgresql_adapter.rb",
        "active_record_statement_errors.rb",
    ];
    const PORTS: &[&str] = &[
        "runtime/spinel/active_model_naming.rb",
        "runtime/spinel/active_model_serialization.rb",
        "runtime/spinel/active_support_tryable.rb",
    ];

    fn sources() -> Vec<(String, String)> {
        let mut files: Vec<(String, String)> = crate::runtime_files::ruby_sources()
            .filter(|(path, _)| {
                let flat = |dir: &str| path.strip_prefix(dir).is_some_and(|rest| !rest.contains('/'));
                flat("runtime/ruby/active_record/") || flat("runtime/spinel/cruby/") || PORTS.contains(path)
            })
            .map(|(path, text)| (path.to_string(), text.to_string()))
            .collect();
        for file in OVERLAY_FILES {
            let path = format!("{OVERLAY}{file}");
            // The browser analyzer embeds no emit scaffold.
            if let Some(text) = crate::runtime_files::read(&path) {
                files.push((path, text.to_string()));
            }
        }
        files
    }

    /// Per side: what `ActiveRecord::Base` answers, by name → public.
    fn surface() -> &'static [HashMap<String, bool>; 2] {
        static SURFACE: OnceLock<[HashMap<String, bool>; 2]> = OnceLock::new();
        SURFACE.get_or_init(|| {
            let mut walker = Walker::default();
            for (_, text) in sources() {
                let parsed = ruby_prism::parse(text.as_bytes());
                if parsed.errors().next().is_some() {
                    continue;
                }
                let root = parsed.node();
                let top = root.as_program_node().map(|p| p.statements().as_node());
                walker.statements(top, &mut Body { owner: String::new(), side: INSTANCE, public: true });
            }
            let mut sides: [HashMap<String, bool>; 2] = Default::default();
            for (side, out) in sides.iter_mut().enumerate() {
                walker.answers("ActiveRecord::Base", side, &mut HashSet::default(), out);
                // Every object answers Object's runtime reopens (`try`).
                walker.answers("Object", INSTANCE, &mut HashSet::default(), out);
            }
            sides
        })
    }

    /// Whether the PostgreSQL CRuby tree's `ActiveRecord::Base` answers
    /// `method` on its class side or on an instance: a method the runtime
    /// defines (public, unless `self` is the receiver), or one Ruby's
    /// Object (an instance) or Class (the class) answers by the vendored
    /// core signatures (`public_send`, `respond_to?`, `name`).
    pub(super) fn answers(method: &str, class_side: bool, explicit_receiver: bool) -> bool {
        let side = &surface()[usize::from(class_side)];
        if let Some(public) = side.get(method) {
            return *public || !explicit_receiver;
        }
        crate::stdlib_rbs::core_instance_method(if class_side { "Class" } else { "Object" }, method)
    }

    #[cfg(test)]
    mod tests {
        use super::answers;

        #[test]
        fn the_postgresql_base_answers_what_its_runtime_defines() {
            for (method, class_side) in [
                ("current_transaction", true),
                ("table_name", true),
                ("primary_key", true),
                ("where", true),
                ("unscoped", true),
                ("arel_table", true),
                ("type_for_attribute", true),
                ("connection_pool", true),
                ("connection_db_config", true),
                ("validators_on", true),
                ("public_send", true),
                ("name", true),
                ("id", false),
                ("has_attribute?", false),
                ("saved_changes", false),
                ("reload", false),
                ("try", false),
                ("public_send", false),
                ("attribute_names", false),
                // `[:save, :create, :update, :destroy].each` in
                // active_record_callbacks.rb's ClassMethods.
                ("after_create", true),
                ("around_save", true),
                ("before_destroy", true),
            ] {
                assert!(answers(method, class_side, true), "{method} (class side: {class_side})");
            }
        }

        #[test]
        fn the_postgresql_base_does_not_answer_what_no_runtime_defines() {
            for (method, class_side) in [
                ("some_unknown_ar_api", true),
                ("some_unknown_ar_api", false),
                ("sanitize_sql_like", true),
                ("base_class", true),
                // Class side only: an instance has no `table_name`.
                ("table_name", false),
                ("current_transaction", false),
                // Instance side only.
                ("id", true),
                // Private on the runtime base: not for an explicit receiver.
                ("initialize", false),
            ] {
                assert!(!answers(method, class_side, true), "{method} (class side: {class_side})");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::narrowing::{apply_narrowing, extract_narrowing};
    use crate::expr::ExprNode;
    use crate::ident::VarId;
    use crate::span::Span;

    fn synth(node: ExprNode) -> Expr {
        Expr::new(Span::synthetic(), node)
    }

    fn var(name: &str) -> Expr {
        synth(ExprNode::Var {
            id: VarId(0),
            name: Symbol::from(name),
        })
    }

    fn nil_lit() -> Expr {
        synth(ExprNode::Lit { value: Literal::Nil })
    }

    fn send(recv: Option<Expr>, method: &str, args: Vec<Expr>) -> Expr {
        synth(ExprNode::Send {
            recv,
            method: Symbol::from(method),
            args,
            block: None,
            parenthesized: true,
        })
    }

    fn empty_classes() -> HashMap<ClassId, ClassInfo> {
        HashMap::default()
    }

    fn ctx_with_local(name: &str, ty: Ty) -> Ctx {
        let mut ctx = Ctx::default();
        ctx.local_bindings.insert(Symbol::from(name), ty);
        ctx
    }

    #[test]
    fn const_class_ref_keeps_class_object_without_registry_entry() {
        // Lower retypes with a partial registry that omits stdlib. A Const
        // whose type is the written class must still stamp CLASS_OBJECT_VALUE
        // so `RecordNotFound < StandardError` classifies as Module#<.
        let classes = empty_classes();
        let typer = BodyTyper::new(&classes);
        let ctx = Ctx::default();
        let mut expr = synth(ExprNode::Const {
            path: vec![Symbol::from("StandardError")],
        });
        let ty = typer.analyze_expr(&mut expr, &ctx);
        assert_eq!(
            ty,
            Ty::Class {
                id: ClassId(Symbol::from("StandardError")),
                args: vec![],
            }
        );
        assert_ne!(
            expr.decisions & crate::expr::CLASS_OBJECT_VALUE,
            0,
            "Const class ref must remain a class object without a registry entry"
        );
    }

    fn optional_str() -> Ty {
        Ty::Union {
            variants: vec![Ty::Str, Ty::Nil],
        }
    }


    #[test]
    fn time_arithmetic_reads_the_argument() {
        let classes = empty_classes();
        let typer = BodyTyper::new(&classes);
        for (method, arg, want) in [
            ("+", Ty::Int, Ty::Time),
            ("+", Ty::Float, Ty::Time),
            ("-", Ty::Int, Ty::Time),
            ("-", Ty::Time, Ty::Float),
            // A Duration (`1.day`) is untyped here, and `Time + Time`
            // is a TypeError: neither is claimed.
            ("+", Ty::Untyped, Ty::Untyped),
            ("+", Ty::Time, Ty::Untyped),
        ] {
            let mut ctx = ctx_with_local("t", Ty::Time);
            ctx.local_bindings.insert(Symbol::from("x"), arg.clone());
            let mut expr = send(Some(var("t")), method, vec![var("x")]);
            assert_eq!(typer.analyze_expr(&mut expr, &ctx), want, "Time {method} {arg:?}");
        }
    }

    #[test]
    fn caller_locations_are_backtrace_locations() {
        // `caller_locations(depth, 1).first.path`, EventReporter#notify's
        // source location.
        let mut classes = empty_classes();
        crate::analyze::register_stdlib_classes(&mut classes);
        let typer = BodyTyper::new(&classes);
        let ctx = Ctx::default();
        let int = |n: i64| synth(ExprNode::Lit { value: Literal::Int { value: n } });
        let locations = send(None, "caller_locations", vec![int(1), int(1)]);
        let first = send(Some(locations), "first", vec![]);
        for (reader, want) in [("path", optional_str()), ("label", optional_str()), ("lineno", Ty::Int)] {
            let mut expr = send(Some(first.clone()), reader, vec![]);
            let ty = typer.analyze_expr(&mut expr, &ctx);
            assert_eq!(ty, want, "{reader}");
        }
    }

    #[test]
    fn invoking_a_proc_answers_its_block_return_or_the_declared_untyped() {
        // `call` and its aliases (`.()` parses as `call`). A function-typed
        // receiver answers its return; a bare `Proc` answers core's
        // `Proc#call: (?) -> untyped`, a declared gradual site rather
        // than an inference gap (`Var`).
        let classes = empty_classes();
        let typer = BodyTyper::new(&classes);
        let block = Ty::Fn {
            params: vec![],
            block: None,
            ret: Box::new(Ty::Str),
            effects: crate::effect::EffectSet::default(),
        };
        let proc_ty = Ty::Class { id: ClassId(Symbol::from("Proc")), args: vec![] };
        for method in ["call", "yield", "[]", "==="] {
            let ctx = ctx_with_local("blk", block.clone());
            let mut expr = send(Some(var("blk")), method, vec![]);
            assert_eq!(typer.analyze_expr(&mut expr, &ctx), Ty::Str, "{method} on a block");
            let ctx = ctx_with_local("value", proc_ty.clone());
            let mut expr = send(Some(var("value")), method, vec![var("value")]);
            assert_eq!(typer.analyze_expr(&mut expr, &ctx), Ty::Untyped, "{method} on a Proc");
        }
        // `value.is_a?(Proc) ? value.call : value`, the runtime's
        // `_attribute_default`: the guard narrows an untyped value to Proc.
        let ctx = ctx_with_local("value", Ty::Untyped);
        let mut expr = synth(ExprNode::If {
            cond: send(
                Some(var("value")),
                "is_a?",
                vec![synth(ExprNode::Const { path: vec![Symbol::from("Proc")] })],
            ),
            then_branch: send(Some(var("value")), "call", vec![]),
            else_branch: var("value"),
        });
        typer.analyze_expr(&mut expr, &ctx);
        let ExprNode::If { then_branch, .. } = &*expr.node else { unreachable!() };
        assert_eq!(then_branch.ty, Some(Ty::Untyped));
    }

    #[test]
    fn super_in_an_instance_initialize_is_its_void_parent_initialize() {
        // `super(message)` re-sends `initialize`, which RBS declares
        // `-> void` (read as Nil). A class-side `initialize`, any other
        // method, and a body typed with no enclosing def keep `Untyped`:
        // their parent method is not looked up.
        let classes = empty_classes();
        let typer = BodyTyper::new(&classes);
        let super_call = || synth(ExprNode::Super { args: Some(vec![var("message")]), block: None });
        for (def, want) in [
            (Some(("initialize", false)), Ty::Nil),
            (Some(("initialize", true)), Ty::Untyped),
            (Some(("save", false)), Ty::Untyped),
            (None, Ty::Untyped),
        ] {
            let mut ctx = ctx_with_local("message", Ty::Str);
            ctx.enclosing_def = def.map(|(name, class_side)| (Symbol::from(name), class_side));
            let mut expr = super_call();
            assert_eq!(typer.analyze_expr(&mut expr, &ctx), want, "{def:?}");
            let ExprNode::Super { args: Some(args), .. } = &*expr.node else { unreachable!() };
            assert_eq!(args[0].ty, Some(Ty::Str), "arguments are typed either way");
        }
        // A block inside `initialize` inherits the def: its `super` is
        // the method's.
        let mut ctx = Ctx::default();
        ctx.enclosing_def = Some((Symbol::from("initialize"), false));
        let mut block = synth(ExprNode::Lambda { signature: Default::default(),
            rest_param: None,
            params: vec![],
            block_param: None,
            body: synth(ExprNode::Super { args: None, block: None }),
            block_style: crate::expr::BlockStyle::Brace,
        });
        typer.analyze_expr(&mut block, &ctx);
        let ExprNode::Lambda { body, .. } = &*block.node else { unreachable!() };
        assert_eq!(body.ty, Some(Ty::Nil));
    }

    #[test]
    fn kernel_array_does_not_capture_inherited_or_included_app_methods() {
        let owner = ClassId(Symbol::from("Owner"));
        let child = ClassId(Symbol::from("Child"));
        for (included, ret) in [(false, Ty::Str), (true, Ty::Str), (false, Ty::Untyped)] {
            let mut classes = empty_classes();
            let mut info = ClassInfo::default();
            info.instance_methods.insert(Symbol::from("Array"), ret.clone());
            classes.insert(owner.clone(), info);
            let mut info = ClassInfo::default();
            if included {
                info.includes.push(owner.clone());
            } else {
                info.parent = Some(owner.clone());
            }
            classes.insert(child.clone(), info);
            let mut ctx = Ctx::default();
            ctx.self_ty = Some(Ty::Class { id: child.clone(), args: vec![] });
            let mut expr = send(None, "Array", vec![nil_lit()]);
            assert_eq!(BodyTyper::new(&classes).analyze_expr(&mut expr, &ctx), ret);
        }
    }

    #[test]
    fn kernel_proc_and_lambda_answer_their_block_unless_the_app_defines_them() {
        // `Fmt = proc { |x| x }` / `lambda { … }`: the Proc the block
        // becomes, typed as `->(x) { … }` is (app_profiler's
        // `DefaultProfileFormatter = proc do |upload| … end`).
        let block_of = |body: Expr| {
            synth(ExprNode::Send {
                recv: None,
                method: Symbol::from("proc"),
                args: vec![],
                block: Some(lambda(vec!["x"], body)),
                parenthesized: false,
            })
        };
        let str_lit = || synth(ExprNode::Lit { value: Literal::Str { value: "p".into() } });
        for method in ["proc", "lambda"] {
            let mut expr = block_of(str_lit());
            if let ExprNode::Send { method: m, .. } = &mut *expr.node {
                *m = Symbol::from(method);
            }
            let ty = BodyTyper::new(&empty_classes()).analyze_expr(&mut expr, &Ctx::default());
            assert!(matches!(&ty, Ty::Fn { ret, .. } if **ret == Ty::Str), "{method}: {ty:?}");
        }
        // An app method of the name (inherited here) answers instead.
        let owner = ClassId(Symbol::from("Owner"));
        let child = ClassId(Symbol::from("Child"));
        let mut classes = empty_classes();
        let mut info = ClassInfo::default();
        info.instance_methods.insert(Symbol::from("proc"), Ty::Int);
        classes.insert(owner.clone(), info);
        let mut info = ClassInfo::default();
        info.parent = Some(owner);
        classes.insert(child.clone(), info);
        let mut ctx = Ctx::default();
        ctx.self_ty = Some(Ty::Class { id: child, args: vec![] });
        let mut expr = block_of(str_lit());
        assert_eq!(BodyTyper::new(&classes).analyze_expr(&mut expr, &ctx), Ty::Int);
        // `proc(&blk)` and `proc(arg)` are not the literal-block form.
        let mut expr = send(None, "proc", vec![nil_lit()]);
        assert!(!matches!(BodyTyper::new(&empty_classes()).analyze_expr(&mut expr, &Ctx::default()), Ty::Fn { .. }));
    }

    #[test]
    fn kernel_array_keeps_the_argument_element_type() {
        let array_of = |elem: Ty| Ty::Array { elem: Box::new(elem) };
        for (arg, elem) in [
            (array_of(Ty::Str), Ty::Str),
            (Ty::Tuple { elems: vec![Ty::Str, Ty::Int] }, union_of(Ty::Str, Ty::Int)),
            (Ty::Relation { of: ClassId(Symbol::from("Story")) }, Ty::Class { id: ClassId(Symbol::from("Story")), args: vec![] }),
            (Ty::Class { id: ClassId(Symbol::from("Range")), args: vec![Ty::Int] }, Ty::Int),
            // Unknown elements: the class may unpack through `to_a`.
            (Ty::Class { id: ClassId(Symbol::from("Story")), args: vec![] }, Ty::Var { var: TyVar(0) }),
            (Ty::Sym, Ty::Sym),
            (Ty::Union { variants: vec![array_of(Ty::Sym), Ty::Sym, Ty::Nil] }, Ty::Sym),
        ] {
            let ctx = ctx_with_local("x", arg);
            let mut expr = send(None, "Array", vec![var("x")]);
            assert_eq!(BodyTyper::new(&empty_classes()).analyze_expr(&mut expr, &ctx), array_of(elem));
        }
    }

    #[test]
    fn array_plus_onto_an_unknown_element_takes_the_argument_element() {
        let array_of = |elem: Ty| Ty::Array { elem: Box::new(elem) };
        let mut ctx = ctx_with_local("empty", array_of(Ty::Var { var: TyVar(0) }));
        ctx.local_bindings.insert(Symbol::from("strs"), array_of(Ty::Str));
        ctx.local_bindings.insert(Symbol::from("syms"), array_of(Ty::Sym));
        let mut expr = send(Some(var("empty")), "+", vec![var("strs")]);
        assert_eq!(BodyTyper::new(&empty_classes()).analyze_expr(&mut expr, &ctx), array_of(Ty::Str));
        // A known receiver element still answers for the result.
        let mut expr = send(Some(var("strs")), "+", vec![var("syms")]);
        assert_eq!(BodyTyper::new(&empty_classes()).analyze_expr(&mut expr, &ctx), array_of(Ty::Str));
    }

    #[test]
    fn if_nil_narrows_variable_to_nil_in_then_branch() {
        let cond = send(Some(var("x")), "nil?", vec![]);
        let pred = extract_narrowing(&cond).expect("narrowing detected");
        let ctx = ctx_with_local("x", optional_str());
        let then_ctx = apply_narrowing(&ctx, &pred, true);
        assert_eq!(then_ctx.local_bindings[&Symbol::from("x")], Ty::Nil);
    }

    #[test]
    fn if_nil_narrows_variable_removing_nil_in_else_branch() {
        let cond = send(Some(var("x")), "nil?", vec![]);
        let pred = extract_narrowing(&cond).unwrap();
        let ctx = ctx_with_local("x", optional_str());
        let else_ctx = apply_narrowing(&ctx, &pred, false);
        assert_eq!(else_ctx.local_bindings[&Symbol::from("x")], Ty::Str);
    }

    #[test]
    fn not_nil_is_inverse() {
        let inner = send(Some(var("x")), "nil?", vec![]);
        let cond = send(Some(inner), "!", vec![]);
        let pred = extract_narrowing(&cond).expect("negation recognized");
        let ctx = ctx_with_local("x", optional_str());
        let then_ctx = apply_narrowing(&ctx, &pred, true);
        let else_ctx = apply_narrowing(&ctx, &pred, false);
        assert_eq!(then_ctx.local_bindings[&Symbol::from("x")], Ty::Str);
        assert_eq!(else_ctx.local_bindings[&Symbol::from("x")], Ty::Nil);
    }

    #[test]
    fn explicit_equality_to_nil_narrows() {
        let cond = send(Some(var("x")), "==", vec![nil_lit()]);
        let pred = extract_narrowing(&cond).expect("== nil recognized");
        let ctx = ctx_with_local("x", optional_str());
        let then_ctx = apply_narrowing(&ctx, &pred, true);
        assert_eq!(then_ctx.local_bindings[&Symbol::from("x")], Ty::Nil);
    }

    #[test]
    fn explicit_inequality_to_nil_narrows_inversely() {
        let cond = send(Some(var("x")), "!=", vec![nil_lit()]);
        let pred = extract_narrowing(&cond).expect("!= nil recognized");
        let ctx = ctx_with_local("x", optional_str());
        let then_ctx = apply_narrowing(&ctx, &pred, true);
        let else_ctx = apply_narrowing(&ctx, &pred, false);
        assert_eq!(then_ctx.local_bindings[&Symbol::from("x")], Ty::Str);
        assert_eq!(else_ctx.local_bindings[&Symbol::from("x")], Ty::Nil);
    }

    #[test]
    fn ivar_narrowing() {
        let ivar = synth(ExprNode::Ivar {
            name: Symbol::from("post"),
        });
        let cond = send(Some(ivar), "nil?", vec![]);
        let pred = extract_narrowing(&cond).unwrap();
        let mut ctx = Ctx::default();
        ctx.ivar_bindings
            .insert(Symbol::from("post"), optional_str());
        let then_ctx = apply_narrowing(&ctx, &pred, true);
        let else_ctx = apply_narrowing(&ctx, &pred, false);
        assert_eq!(then_ctx.ivar_bindings[&Symbol::from("post")], Ty::Nil);
        assert_eq!(else_ctx.ivar_bindings[&Symbol::from("post")], Ty::Str);
    }

    #[test]
    fn missing_binding_is_a_noop() {
        let cond = send(Some(var("x")), "nil?", vec![]);
        let pred = extract_narrowing(&cond).unwrap();
        let ctx = Ctx::default();
        let then_ctx = apply_narrowing(&ctx, &pred, true);
        assert!(then_ctx.local_bindings.is_empty());
    }

    #[test]
    fn non_narrowing_condition_returns_none() {
        let cond = send(Some(var("x")), "length", vec![]);
        assert!(extract_narrowing(&cond).is_none());
    }

    #[test]
    fn is_a_string_narrows_to_str() {
        let class_ref = synth(ExprNode::Const {
            path: vec![Symbol::from("String")],
        });
        let cond = send(Some(var("x")), "is_a?", vec![class_ref]);
        let pred = extract_narrowing(&cond).expect("is_a? recognized");
        let mixed = Ty::Union {
            variants: vec![Ty::Str, Ty::Int, Ty::Nil],
        };
        let ctx = ctx_with_local("x", mixed);
        let then_ctx = apply_narrowing(&ctx, &pred, true);
        let else_ctx = apply_narrowing(&ctx, &pred, false);
        assert_eq!(then_ctx.local_bindings[&Symbol::from("x")], Ty::Str);
        assert_eq!(
            else_ctx.local_bindings[&Symbol::from("x")],
            Ty::Union {
                variants: vec![Ty::Int, Ty::Nil],
            }
        );
    }

    #[test]
    fn is_a_user_class_narrows_to_class_ty() {
        let class_ref = synth(ExprNode::Const {
            path: vec![Symbol::from("Post")],
        });
        let cond = send(Some(var("x")), "is_a?", vec![class_ref]);
        let pred = extract_narrowing(&cond).unwrap();
        let ctx = ctx_with_local(
            "x",
            Ty::Class {
                id: ClassId(Symbol::from("Post")),
                args: vec![],
            },
        );
        let then_ctx = apply_narrowing(&ctx, &pred, true);
        assert_eq!(
            then_ctx.local_bindings[&Symbol::from("x")],
            Ty::Class {
                id: ClassId(Symbol::from("Post")),
                args: vec![]
            }
        );
    }

    /// `parent = superclass; parent <= ActiveRecord::Base ? parent.m : d` —
    /// a class object of unknown class narrows to the compared class on
    /// the true side and keeps its type on the false side.
    #[test]
    fn subclass_comparison_narrows_a_class_object_to_the_compared_class() {
        let class_ty = |n: &str| Ty::Class { id: ClassId(Symbol::from(n)), args: vec![] };
        let base = || synth(ExprNode::Const { path: vec![Symbol::from("ActiveRecord"), Symbol::from("Base")] });
        for op in ["<", "<="] {
            let cond = send(Some(var("parent")), op, vec![base()]);
            let pred = extract_narrowing(&cond).expect("class comparison recognized");
            let ctx = ctx_with_local("parent", class_ty("Class"));
            let then_ctx = apply_narrowing(&ctx, &pred, true);
            let else_ctx = apply_narrowing(&ctx, &pred, false);
            assert_eq!(then_ctx.local_bindings[&Symbol::from("parent")], class_ty("ActiveRecord::Base"), "{op}");
            assert_eq!(else_ctx.local_bindings[&Symbol::from("parent")], class_ty("Class"), "{op}");
            // `return nil unless parent < Base` reaches the rest through the negation.
            let negated = send(Some(cond), "!", vec![]);
            let pred = extract_narrowing(&negated).expect("negated comparison recognized");
            let after = apply_narrowing(&ctx, &pred, false);
            assert_eq!(after.local_bindings[&Symbol::from("parent")], class_ty("ActiveRecord::Base"), "!{op}");
        }
    }

    /// The same spelling compares values: `count < LIMIT` leaves an
    /// Integer (or a user instance with Comparable) as it was.
    #[test]
    fn subclass_comparison_leaves_non_class_objects_alone() {
        let limit = synth(ExprNode::Const { path: vec![Symbol::from("Limit")] });
        let cond = send(Some(var("x")), "<", vec![limit]);
        let pred = extract_narrowing(&cond).expect("recognized syntactically");
        let version = Ty::Class { id: ClassId(Symbol::from("Version")), args: vec![] };
        for ty in [Ty::Int, version, Ty::Union { variants: vec![Ty::Int, Ty::Nil] }] {
            let ctx = ctx_with_local("x", ty.clone());
            let then_ctx = apply_narrowing(&ctx, &pred, true);
            assert_eq!(then_ctx.local_bindings[&Symbol::from("x")], ty);
        }
        // A literal right-hand side is never a class: no predicate at all.
        let lit = synth(ExprNode::Lit { value: Literal::Int { value: 3 } });
        assert!(extract_narrowing(&send(Some(var("x")), "<", vec![lit])).is_none());
    }

    /// `subclasses` answers the receiver's own class; `<` on a `Class`
    /// object answers true/false/nil.
    #[test]
    fn class_introspection_types_subclasses_and_comparison() {
        let class_ty = |n: &str| Ty::Class { id: ClassId(Symbol::from(n)), args: vec![] };
        let mut ctx = ctx_with_local("k", class_ty("Post"));
        ctx.local_bindings.insert(Symbol::from("parent"), class_ty("Class"));
        let mut expr = send(Some(var("k")), "subclasses", vec![]);
        assert_eq!(
            BodyTyper::new(&empty_classes()).analyze_expr(&mut expr, &ctx),
            Ty::Array { elem: Box::new(class_ty("Post")) }
        );
        let mut expr = send(Some(var("parent")), "<=", vec![var("k")]);
        assert_eq!(
            BodyTyper::new(&empty_classes()).analyze_expr(&mut expr, &ctx),
            Ty::Union { variants: vec![Ty::Bool, Ty::Nil] }
        );
    }

    #[test]
    fn or_with_an_empty_literal_keeps_the_left_container_type() {
        // `(@registry || {})` with `@registry: Hash[String, [Symbol, Int]]?`:
        // the empty literal is a value of the left's Hash type, so the
        // join is that type, not `Hash[String | _, V | _]`; the literal
        // itself is stamped with it.
        let tuple = Ty::Tuple { elems: vec![Ty::Sym, Ty::Int] };
        let hash = Ty::Hash { key: Box::new(Ty::Str), value: Box::new(tuple) };
        let ctx = ctx_with_local("h", Ty::Union { variants: vec![hash.clone(), Ty::Nil] });
        let mut expr = synth(ExprNode::BoolOp {
            op: crate::expr::BoolOpKind::Or,
            surface: Default::default(),
            left: var("h"),
            right: synth(ExprNode::Hash { entries: vec![], kwargs: false }),
        });
        assert_eq!(BodyTyper::new(&empty_classes()).analyze_expr(&mut expr, &ctx), hash);
        let ExprNode::BoolOp { right, .. } = &*expr.node else { unreachable!() };
        assert_eq!(right.ty, Some(hash.clone()));

        let list = Ty::Array { elem: Box::new(Ty::Str) };
        let ctx = ctx_with_local("a", Ty::Union { variants: vec![list.clone(), Ty::Nil] });
        let mut expr = synth(ExprNode::BoolOp {
            op: crate::expr::BoolOpKind::Or,
            surface: Default::default(),
            left: var("a"),
            right: synth(ExprNode::Array { elements: vec![], style: Default::default() }),
        });
        assert_eq!(BodyTyper::new(&empty_classes()).analyze_expr(&mut expr, &ctx), list);
    }

    #[test]
    fn or_with_an_unknown_container_still_joins() {
        // Only a literal written empty adopts the left's type: an unknown
        // Hash of the same spine may hold anything.
        let hash = Ty::Hash { key: Box::new(Ty::Str), value: Box::new(Ty::Int) };
        let unknown = Ty::Hash {
            key: Box::new(Ty::Var { var: TyVar(0) }),
            value: Box::new(Ty::Var { var: TyVar(0) }),
        };
        let mut ctx = ctx_with_local("h", Ty::Union { variants: vec![hash.clone(), Ty::Nil] });
        ctx.local_bindings.insert(Symbol::from("other"), unknown.clone());
        let mut expr = synth(ExprNode::BoolOp {
            op: crate::expr::BoolOpKind::Or,
            surface: Default::default(),
            left: var("h"),
            right: var("other"),
        });
        assert_eq!(
            BodyTyper::new(&empty_classes()).analyze_expr(&mut expr, &ctx),
            union_of(hash, unknown)
        );
    }

    #[test]
    fn a_local_a_default_assigns_is_nilable_in_the_body() {
        // `def m(default: (no_default = true))`: `no_default` is `true`
        // when the default ran and nil when the caller passed the keyword.
        let mut params = vec![crate::dialect::Param {
            default: Some(synth(ExprNode::Assign {
                target: LValue::Var { id: VarId(0), name: Symbol::from("no_default") },
                value: synth(ExprNode::Lit { value: Literal::Bool { value: true } }),
            })),
            keyword: true,
            ..crate::dialect::Param::positional(Symbol::from("default"))
        }];
        let mut ctx = Ctx::default();
        BodyTyper::new(&empty_classes()).seed_param_default_locals(&mut params, &mut ctx);
        assert_eq!(
            ctx.local_bindings.get(&Symbol::from("no_default")),
            Some(&union_of(Ty::Bool, Ty::Nil))
        );
        // The parameter itself is the caller's business, not the default's.
        assert!(!ctx.local_bindings.contains_key(&Symbol::from("default")));
    }

    #[test]
    fn end_to_end_if_nil_narrows_through_analyzer() {
        // Build: `if x.nil?; x; else; x.length; end` — with x: String | Nil,
        // the If's type should be Nil | Int (then is Nil, else is Int
        // because x narrows to String and String#length → Int).
        let then_branch = var("x");
        let else_branch = send(Some(var("x")), "length", vec![]);
        let if_expr = synth(ExprNode::If {
            cond: send(Some(var("x")), "nil?", vec![]),
            then_branch,
            else_branch,
        });
        let mut expr = if_expr;

        let classes = empty_classes();
        let typer = BodyTyper::new(&classes);
        let ctx = ctx_with_local("x", optional_str());
        let t = typer.analyze_expr(&mut expr, &ctx);

        // Result should be the union of (Nil) | (Int) — i.e., { Nil, Int }.
        let variants = match t {
            Ty::Union { variants } => variants,
            other => panic!("expected Union, got {other:?}"),
        };
        assert!(variants.contains(&Ty::Nil), "variants: {variants:?}");
        assert!(variants.contains(&Ty::Int), "variants: {variants:?}");
    }

    #[test]
    fn assignment_in_condition_binds_then_narrows() {
        // `if x = y then x else x end` with `y: String?` and `x`
        // unbound — the authentication generator's
        // `if user = User.authenticate_by(…)`. The then-branch reads
        // `x` as String (assigned, then truthy), the else-branch as
        // String? (assigned, not narrowed: the falsy side is left
        // alone, see `apply_narrowing`). The same for `(x = y) && x`.
        let assign = || {
            synth(ExprNode::Assign {
                target: LValue::Var { id: VarId(0), name: Symbol::from("x") },
                value: var("y"),
            })
        };
        let mut if_expr = synth(ExprNode::If {
            cond: assign(),
            then_branch: var("x"),
            else_branch: var("x"),
        });
        let classes = empty_classes();
        let typer = BodyTyper::new(&classes);
        let ctx = ctx_with_local("y", optional_str());
        typer.analyze_expr(&mut if_expr, &ctx);
        let ExprNode::If { then_branch, else_branch, .. } = &*if_expr.node else {
            panic!("expected If");
        };
        assert_eq!(then_branch.ty, Some(Ty::Str), "then-branch: assigned, then narrowed");
        assert_eq!(else_branch.ty, Some(optional_str()), "else-branch: assigned, not narrowed");

        let mut and_expr = synth(ExprNode::BoolOp {
            op: crate::expr::BoolOpKind::And,
            surface: Default::default(),
            left: assign(),
            right: var("x"),
        });
        typer.analyze_expr(&mut and_expr, &ctx);
        let ExprNode::BoolOp { right, .. } = &*and_expr.node else {
            panic!("expected BoolOp");
        };
        assert_eq!(right.ty, Some(Ty::Str), "right of `&&`: assigned, then narrowed");
    }

    #[test]
    fn narrowing_writes_back_to_var_ty_inside_assign_in_else_branch() {
        // `if x.nil? then nil else @y = x end` — same shape as the
        // postfix `@y = x unless x.nil?` form. The Var{x} sits inside
        // an Assign, which is inside the else-branch. The Var's .ty
        // should still narrow to String — i.e. write-back has to
        // propagate through every nested node the body-typer walks.
        let then_branch = nil_lit();
        let assign = synth(ExprNode::Assign {
            target: LValue::Ivar { name: Symbol::from("y") },
            value: var("x"),
        });
        let if_expr = synth(ExprNode::If {
            cond: send(Some(var("x")), "nil?", vec![]),
            then_branch,
            else_branch: assign,
        });
        let mut expr = if_expr;

        let classes = empty_classes();
        let typer = BodyTyper::new(&classes);
        let ctx = ctx_with_local("x", optional_str());
        typer.analyze_expr(&mut expr, &ctx);

        let ExprNode::If { else_branch, .. } = &*expr.node else {
            panic!("expected If");
        };
        let ExprNode::Assign { value, .. } = &*else_branch.node else {
            panic!("expected Assign in else");
        };
        assert_eq!(
            value.ty,
            Some(Ty::Str),
            "Var{{x}} inside Assign{{value:}} in else should narrow to Str; \
             got {:?}",
            value.ty,
        );
    }

    #[test]
    fn narrowing_writes_back_to_var_ty_in_else_branch() {
        // `if x.nil? then nil else x end` — x narrows to String in the
        // else branch; the Var{x} read inside else should carry ty=Str
        // on its Expr, not the un-narrowed Option<String>. This is the
        // "narrowing write-back" gate — emit-side coercion paths read
        // value.ty directly, and a stale Option<String> there triggers
        // spurious .to_string().unwrap() chains (E0599 on rust's
        // action_controller_base render).
        let then_branch = nil_lit();
        let else_branch = var("x");
        let if_expr = synth(ExprNode::If {
            cond: send(Some(var("x")), "nil?", vec![]),
            then_branch,
            else_branch,
        });
        let mut expr = if_expr;

        let classes = empty_classes();
        let typer = BodyTyper::new(&classes);
        let ctx = ctx_with_local("x", optional_str());
        typer.analyze_expr(&mut expr, &ctx);

        let ExprNode::If { else_branch, .. } = &*expr.node else {
            panic!("expected If");
        };
        assert_eq!(
            else_branch.ty,
            Some(Ty::Str),
            "else-branch Var{{x}} should reflect narrowed (non-nil) type"
        );
    }

    // ── block return propagation (7b) ──────────────────────────────

    use crate::expr::BlockStyle;

    fn lambda(params: Vec<&str>, body: Expr) -> Expr {
        synth(ExprNode::Lambda { signature: Default::default(), rest_param: None,
            params: params.into_iter().map(Symbol::from).collect(),
            block_param: None,
            body,
            block_style: BlockStyle::Do,
        })
    }

    #[test]
    fn array_map_returns_block_body_type() {
        // arr.map { |x| x.to_s } on arr: Array[Int] should produce Array[Str]
        let arr = {
            let mut e = var("arr");
            e.ty = Some(Ty::Array { elem: Box::new(Ty::Int) });
            e
        };
        let block_body = send(Some(var("x")), "to_s", vec![]);
        let lam = lambda(vec!["x"], block_body);
        let call = synth(ExprNode::Send {
            recv: Some(arr),
            method: Symbol::from("map"),
            args: vec![],
            block: Some(lam),
            parenthesized: false,
        });

        let mut expr = call;
        let classes = empty_classes();
        let typer = BodyTyper::new(&classes);
        let mut ctx = Ctx::default();
        ctx.local_bindings.insert(
            Symbol::from("arr"),
            Ty::Array { elem: Box::new(Ty::Int) },
        );
        let ty = typer.analyze_expr(&mut expr, &ctx);

        assert_eq!(ty, Ty::Array { elem: Box::new(Ty::Str) });
    }

    #[test]
    fn an_array_literal_holds_a_splatted_arrays_elements() {
        // [*arr, "x"] on arr: Array[Array[Str]] is Array[Array[Str] | Str],
        // and a splatted nil adds nothing.
        let splat = |name: &str| synth(ExprNode::Splat { value: var(name) });
        let mut expr = synth(ExprNode::Array {
            elements: vec![splat("arr"), splat("none"), synth(ExprNode::Lit { value: Literal::Str { value: "x".into() } })],
            style: Default::default(),
        });
        let classes = empty_classes();
        let typer = BodyTyper::new(&classes);
        let mut ctx = Ctx::default();
        let inner = Ty::Array { elem: Box::new(Ty::Str) };
        ctx.local_bindings.insert(Symbol::from("arr"), Ty::Array { elem: Box::new(inner.clone()) });
        ctx.local_bindings.insert(Symbol::from("none"), Ty::Nil);
        let ty = typer.analyze_expr(&mut expr, &ctx);

        assert_eq!(ty, Ty::Array { elem: Box::new(union_of(inner, Ty::Str)) });
    }

    #[test]
    fn array_select_preserves_element_type() {
        // arr.select { |x| x > 0 } on arr: Array[Int] should still be Array[Int]
        let arr = {
            let mut e = var("arr");
            e.ty = Some(Ty::Array { elem: Box::new(Ty::Int) });
            e
        };
        let block_body = send(Some(var("x")), ">", vec![synth(ExprNode::Lit {
            value: Literal::Int { value: 0 },
        })]);
        let lam = lambda(vec!["x"], block_body);
        let call = synth(ExprNode::Send {
            recv: Some(arr),
            method: Symbol::from("select"),
            args: vec![],
            block: Some(lam),
            parenthesized: false,
        });

        let mut expr = call;
        let classes = empty_classes();
        let typer = BodyTyper::new(&classes);
        let mut ctx = Ctx::default();
        ctx.local_bindings.insert(
            Symbol::from("arr"),
            Ty::Array { elem: Box::new(Ty::Int) },
        );
        let ty = typer.analyze_expr(&mut expr, &ctx);

        assert_eq!(ty, Ty::Array { elem: Box::new(Ty::Int) });
    }

    #[test]
    fn array_flat_map_flattens_one_level() {
        // arr.flat_map { |x| [x.to_s] } on arr: Array[Int] should be Array[Str]
        let arr = {
            let mut e = var("arr");
            e.ty = Some(Ty::Array { elem: Box::new(Ty::Int) });
            e
        };
        let inner_arr = synth(ExprNode::Array {
            elements: vec![send(Some(var("x")), "to_s", vec![])],
            style: Default::default(),
        });
        let lam = lambda(vec!["x"], inner_arr);
        let call = synth(ExprNode::Send {
            recv: Some(arr),
            method: Symbol::from("flat_map"),
            args: vec![],
            block: Some(lam),
            parenthesized: false,
        });

        let mut expr = call;
        let classes = empty_classes();
        let typer = BodyTyper::new(&classes);
        let mut ctx = Ctx::default();
        ctx.local_bindings.insert(
            Symbol::from("arr"),
            Ty::Array { elem: Box::new(Ty::Int) },
        );
        let ty = typer.analyze_expr(&mut expr, &ctx);

        assert_eq!(ty, Ty::Array { elem: Box::new(Ty::Str) });
    }

    #[test]
    fn array_accumulator_element_refined_by_shovel() {
        // `ordered = []; ordered << 5; ordered` should read `Array[Int]`,
        // not `Array[untyped]` — the empty-literal accumulator's element
        // type is revealed at the `<<` push. Mirrors the Hash `h[k]=v`
        // accumulator refinement. This is the arrange_for_user shape that
        // was leaving the comment-threading collection untyped.
        let assign = synth(ExprNode::Assign {
            target: LValue::Var { id: VarId(0), name: Symbol::from("ordered") },
            value: synth(ExprNode::Array { elements: vec![], style: Default::default() }),
        });
        let five = synth(ExprNode::Lit { value: Literal::Int { value: 5 } });
        let push = send(Some(var("ordered")), "<<", vec![five]);
        let read = var("ordered");
        let mut seq =
            synth(ExprNode::Seq { exprs: vec![assign, push, read] });

        let classes = empty_classes();
        let typer = BodyTyper::new(&classes);
        let ctx = Ctx::default();
        let ty = typer.analyze_expr(&mut seq, &ctx);
        assert_eq!(ty, Ty::Array { elem: Box::new(Ty::Int) });
    }

    #[test]
    fn array_accumulator_seed_literal_retro_stamped() {
        // The refinement must reach the SEED literal's stamped ty, not
        // just the forward binding: decl-site emitters (Go `parts :=
        // []string{}`, Crystal `[] of String`) type the declaration
        // from the literal, while later uses (`.join`) type off the
        // refined binding — a binding-only refinement made Go declare
        // `[]interface{}{}` and hand it to `strings.Join([]string)`.
        let assign = synth(ExprNode::Assign {
            target: LValue::Var { id: VarId(0), name: Symbol::from("ordered") },
            value: synth(ExprNode::Array { elements: vec![], style: Default::default() }),
        });
        let five = synth(ExprNode::Lit { value: Literal::Int { value: 5 } });
        let push = send(Some(var("ordered")), "<<", vec![five]);
        let read = var("ordered");
        let mut seq =
            synth(ExprNode::Seq { exprs: vec![assign, push, read] });

        let classes = empty_classes();
        let typer = BodyTyper::new(&classes);
        let ctx = Ctx::default();
        typer.analyze_expr(&mut seq, &ctx);

        let expected = Ty::Array { elem: Box::new(Ty::Int) };
        let ExprNode::Seq { exprs } = &*seq.node else { panic!("seq") };
        assert_eq!(exprs[0].ty.as_ref(), Some(&expected));
        let ExprNode::Assign { value, .. } = &*exprs[0].node else { panic!("assign") };
        assert_eq!(value.ty.as_ref(), Some(&expected));
    }

    #[test]
    fn nil_seeded_local_refined_by_nested_write() {
        // `size = nil; [..].each { size = <Str> }` must leave `size`
        // reading `Str | Nil`, NOT the seed's bare `Nil`. A stale
        // `Ty::Nil` is not merely imprecise: Go's `nil?` emit folds a
        // `Ty::Nil` receiver to the literal `true`, so a following
        // `if !size.nil?` became `if !(true)` and its body was dropped
        // as dead code — a silent miscompile, not a type error.
        let seed = synth(ExprNode::Assign {
            target: LValue::Var { id: VarId(0), name: Symbol::from("size") },
            value: nil_lit(),
        });
        let str_lit = synth(ExprNode::Lit {
            value: Literal::Str { value: "16x16".into() },
        });
        let inner = synth(ExprNode::Assign {
            target: LValue::Var { id: VarId(0), name: Symbol::from("size") },
            value: str_lit,
        });
        // The write is nested inside a block, which is exactly why a
        // statement-level-only walk missed it.
        let each = send(Some(var("items")), "each", vec![lambda(vec!["x"], inner)]);
        let read = var("size");
        let mut seq = synth(ExprNode::Seq { exprs: vec![seed, each, read] });

        let classes = empty_classes();
        let typer = BodyTyper::new(&classes);
        let ctx = Ctx::default();
        let ty = typer.analyze_expr(&mut seq, &ctx);

        let expected = union_of(Ty::Nil, Ty::Str);
        assert_eq!(ty, expected, "read of the refined local");
        // …and the SEED must agree, so decl-site emitters don't declare
        // from a type the rest of the body contradicts.
        let ExprNode::Seq { exprs } = &*seq.node else { panic!("seq") };
        assert_eq!(exprs[0].ty.as_ref(), Some(&expected), "seed stmt");
        let ExprNode::Assign { value, .. } = &*exprs[0].node else { panic!("assign") };
        assert_eq!(value.ty.as_ref(), Some(&expected), "seed literal");
    }

    #[test]
    fn hash_accumulator_refined_by_nested_index_write() {
        // `attrs = {}; [..].each { attrs[k] = <Untyped> }; attrs[:src] =
        // <Str>` must widen the seed to hold BOTH writes. The nested one
        // arrives as a two-arg `[]=` send (not an `LValue::Index`), and
        // missing it was worse than seeing nothing: the visible String
        // write alone monomorphized the hash to `map[string]string`,
        // which the invisible untyped write then failed to satisfy.
        let seed = synth(ExprNode::Assign {
            target: LValue::Var { id: VarId(0), name: Symbol::from("attrs") },
            value: synth(ExprNode::Hash { entries: vec![], kwargs: false }),
        });
        // The block params take their types from the iterated hash, so
        // `v` reads `Untyped` the way `opts.to_h.each` does in the real
        // helper. `opts` has to be a real ctx binding — a ty stamped on
        // the node is overwritten by the Var lookup during analysis.
        let nested_write = send(Some(var("attrs")), "[]=", vec![var("k"), var("v")]);
        let each = synth(ExprNode::Send {
            recv: Some(var("opts")),
            method: Symbol::from("each"),
            args: vec![],
            block: Some(lambda(vec!["k", "v"], nested_write)),
            parenthesized: true,
        });
        let str_val = synth(ExprNode::Lit {
            value: Literal::Str { value: "/a.png".into() },
        });
        let sym_key = synth(ExprNode::Lit {
            value: Literal::Sym { value: "src".into() },
        });
        let top_write = send(Some(var("attrs")), "[]=", vec![sym_key, str_val]);
        let read = var("attrs");
        let mut seq = synth(ExprNode::Seq {
            exprs: vec![seed, each, top_write, read],
        });

        let classes = empty_classes();
        let typer = BodyTyper::new(&classes);
        let ctx = ctx_with_local(
            "opts",
            Ty::Hash {
                key: Box::new(Ty::Str),
                value: Box::new(Ty::Untyped),
            },
        );
        typer.analyze_expr(&mut seq, &ctx);

        let ExprNode::Seq { exprs } = &*seq.node else { panic!("seq") };
        let Some(Ty::Hash { value, .. }) = exprs[0].ty.as_ref() else {
            panic!("seed stamped as a Hash, got {:?}", exprs[0].ty)
        };
        // Both writes are represented — the value is no longer the
        // placeholder, and it is not the String write alone.
        assert!(
            !matches!(**value, Ty::Var { .. }),
            "seed value still the placeholder: {value:?}"
        );
        assert_ne!(**value, Ty::Str, "monomorphized to the visible write only");
    }

    #[test]
    fn nilable_hash_write_leaves_seed_placeholder() {
        // The counterweight to the test above: `params = {}; params[k] =
        // <Str | Nil>` must NOT stamp the seed. Ruby's `Array#[]` types
        // as `Str | Nil`, but no target emits it that way (Crystal's
        // `Array(String)#[]` raises rather than returning nil), so
        // stamping it contradicts the emit and outranks the author's
        // declared `Hash[String, String]` return — Crystal rejected
        // `Router.match_parts` for returning `Hash(String, String | Nil)`.
        let seed = synth(ExprNode::Assign {
            target: LValue::Var { id: VarId(0), name: Symbol::from("params") },
            value: synth(ExprNode::Hash { entries: vec![], kwargs: false }),
        });
        let nilable = {
            let mut e = var("ap");
            e.ty = Some(optional_str());
            e
        };
        let write = send(Some(var("params")), "[]=", vec![var("name"), nilable]);
        let mut seq = synth(ExprNode::Seq { exprs: vec![seed, write] });

        let classes = empty_classes();
        let typer = BodyTyper::new(&classes);
        let ctx = Ctx::default();
        typer.analyze_expr(&mut seq, &ctx);

        let ExprNode::Seq { exprs } = &*seq.node else { panic!("seq") };
        let Some(Ty::Hash { value, .. }) = exprs[0].ty.as_ref() else {
            panic!("seed is a Hash, got {:?}", exprs[0].ty)
        };
        assert!(
            matches!(**value, Ty::Var { .. }),
            "nilable write must leave the placeholder, got {value:?}"
        );
    }

    #[test]
    fn hash_map_returns_array_of_block_ret() {
        // h.map { |k, v| v.to_s } on h: Hash[Sym, Int] should be Array[Str]
        let h = {
            let mut e = var("h");
            e.ty = Some(Ty::Hash {
                key: Box::new(Ty::Sym),
                value: Box::new(Ty::Int),
            });
            e
        };
        let block_body = send(Some(var("v")), "to_s", vec![]);
        let lam = lambda(vec!["k", "v"], block_body);
        let call = synth(ExprNode::Send {
            recv: Some(h),
            method: Symbol::from("map"),
            args: vec![],
            block: Some(lam),
            parenthesized: false,
        });

        let mut expr = call;
        let classes = empty_classes();
        let typer = BodyTyper::new(&classes);
        let mut ctx = Ctx::default();
        ctx.local_bindings.insert(
            Symbol::from("h"),
            Ty::Hash {
                key: Box::new(Ty::Sym),
                value: Box::new(Ty::Int),
            },
        );
        let ty = typer.analyze_expr(&mut expr, &ctx);

        assert_eq!(ty, Ty::Array { elem: Box::new(Ty::Str) });
    }

    #[test]
    fn hash_transform_values_changes_value_type() {
        // h.transform_values { |v| v.to_s } on Hash[Sym, Int] → Hash[Sym, Str]
        let h = {
            let mut e = var("h");
            e.ty = Some(Ty::Hash {
                key: Box::new(Ty::Sym),
                value: Box::new(Ty::Int),
            });
            e
        };
        let block_body = send(Some(var("v")), "to_s", vec![]);
        let lam = lambda(vec!["v"], block_body);
        let call = synth(ExprNode::Send {
            recv: Some(h),
            method: Symbol::from("transform_values"),
            args: vec![],
            block: Some(lam),
            parenthesized: false,
        });

        let mut expr = call;
        let classes = empty_classes();
        let typer = BodyTyper::new(&classes);
        let mut ctx = Ctx::default();
        ctx.local_bindings.insert(
            Symbol::from("h"),
            Ty::Hash {
                key: Box::new(Ty::Sym),
                value: Box::new(Ty::Int),
            },
        );
        let ty = typer.analyze_expr(&mut expr, &ctx);

        assert_eq!(
            ty,
            Ty::Hash {
                key: Box::new(Ty::Sym),
                value: Box::new(Ty::Str),
            }
        );
    }

    #[test]
    fn map_without_block_falls_back_to_input_elem() {
        // arr.map (no block — Symbol-to-Proc pattern handled elsewhere)
        // should still produce a sensible Array[elem] type.
        let arr = {
            let mut e = var("arr");
            e.ty = Some(Ty::Array { elem: Box::new(Ty::Int) });
            e
        };
        let call = synth(ExprNode::Send {
            recv: Some(arr),
            method: Symbol::from("map"),
            args: vec![],
            block: None,
            parenthesized: false,
        });

        let mut expr = call;
        let classes = empty_classes();
        let typer = BodyTyper::new(&classes);
        let mut ctx = Ctx::default();
        ctx.local_bindings.insert(
            Symbol::from("arr"),
            Ty::Array { elem: Box::new(Ty::Int) },
        );
        let ty = typer.analyze_expr(&mut expr, &ctx);

        assert_eq!(ty, Ty::Array { elem: Box::new(Ty::Int) });
    }

    // ── diagnostic annotation ─────────────────────────────────────

    #[test]
    fn incompatible_add_annotates_diagnostic_on_send() {
        // Build: `1 + "hello"` — concrete Int + Str. Body-typer should
        // annotate the enclosing Send with an IncompatibleBinop
        // diagnostic.
        let lhs = synth(ExprNode::Lit { value: Literal::Int { value: 1 } });
        let rhs = synth(ExprNode::Lit {
            value: Literal::Str { value: "hello".to_string() },
        });
        let add = synth(ExprNode::Send {
            recv: Some(lhs),
            method: Symbol::from("+"),
            args: vec![rhs],
            block: None,
            parenthesized: false,
        });

        let mut expr = add;
        let classes = empty_classes();
        let typer = BodyTyper::new(&classes);
        typer.analyze_expr(&mut expr, &Ctx::default());

        let diag = expr.diagnostic.as_ref().expect("diagnostic set");
        match diag {
            crate::diagnostic::DiagnosticKind::IncompatibleBinop {
                op,
                lhs_ty,
                rhs_ty,
            } => {
                assert_eq!(op.as_str(), "+");
                assert_eq!(lhs_ty, &Ty::Int);
                assert_eq!(rhs_ty, &Ty::Str);
            }
            other => panic!("expected IncompatibleBinop, got {other:?}"),
        }
    }

    #[test]
    fn incompatible_compare_annotates_diagnostic_on_send() {
        // `1 < "hello"` — Ruby's Comparable raises on mixed types.
        // Body-typer should annotate the Send.
        let lhs = synth(ExprNode::Lit { value: Literal::Int { value: 1 } });
        let rhs = synth(ExprNode::Lit {
            value: Literal::Str { value: "hello".to_string() },
        });
        let cmp = synth(ExprNode::Send {
            recv: Some(lhs),
            method: Symbol::from("<"),
            args: vec![rhs],
            block: None,
            parenthesized: false,
        });

        let mut expr = cmp;
        let classes = empty_classes();
        let typer = BodyTyper::new(&classes);
        typer.analyze_expr(&mut expr, &Ctx::default());

        let diag = expr.diagnostic.as_ref().expect("diagnostic set");
        match diag {
            crate::diagnostic::DiagnosticKind::IncompatibleBinop {
                op,
                lhs_ty,
                rhs_ty,
            } => {
                assert_eq!(op.as_str(), "<");
                assert_eq!(lhs_ty, &Ty::Int);
                assert_eq!(rhs_ty, &Ty::Str);
            }
            other => panic!("expected IncompatibleBinop, got {other:?}"),
        }
    }

    #[test]
    fn compatible_compare_leaves_diagnostic_empty() {
        // Int < Int is valid.
        let lhs = synth(ExprNode::Lit { value: Literal::Int { value: 1 } });
        let rhs = synth(ExprNode::Lit { value: Literal::Int { value: 2 } });
        let cmp = synth(ExprNode::Send {
            recv: Some(lhs),
            method: Symbol::from("<"),
            args: vec![rhs],
            block: None,
            parenthesized: false,
        });

        let mut expr = cmp;
        let classes = empty_classes();
        let typer = BodyTyper::new(&classes);
        typer.analyze_expr(&mut expr, &Ctx::default());

        assert!(expr.diagnostic.is_none());
    }

    #[test]
    fn incompatible_sub_annotates_diagnostic_on_send() {
        // `"a" - "b"` — Ruby's String doesn't define `-`, NoMethodError.
        let lhs = synth(ExprNode::Lit { value: Literal::Str { value: "a".to_string() } });
        let rhs = synth(ExprNode::Lit { value: Literal::Str { value: "b".to_string() } });
        let sub = synth(ExprNode::Send {
            recv: Some(lhs),
            method: Symbol::from("-"),
            args: vec![rhs],
            block: None,
            parenthesized: false,
        });

        let mut expr = sub;
        let classes = empty_classes();
        let typer = BodyTyper::new(&classes);
        typer.analyze_expr(&mut expr, &Ctx::default());

        let diag = expr.diagnostic.as_ref().expect("diagnostic set");
        match diag {
            crate::diagnostic::DiagnosticKind::IncompatibleBinop {
                op, lhs_ty, rhs_ty,
            } => {
                assert_eq!(op.as_str(), "-");
                assert_eq!(lhs_ty, &Ty::Str);
                assert_eq!(rhs_ty, &Ty::Str);
            }
            other => panic!("expected IncompatibleBinop, got {other:?}"),
        }
    }

    #[test]
    fn compatible_sub_leaves_diagnostic_empty() {
        // Int - Int is valid.
        let lhs = synth(ExprNode::Lit { value: Literal::Int { value: 5 } });
        let rhs = synth(ExprNode::Lit { value: Literal::Int { value: 2 } });
        let sub = synth(ExprNode::Send {
            recv: Some(lhs),
            method: Symbol::from("-"),
            args: vec![rhs],
            block: None,
            parenthesized: false,
        });

        let mut expr = sub;
        let classes = empty_classes();
        let typer = BodyTyper::new(&classes);
        typer.analyze_expr(&mut expr, &Ctx::default());

        assert!(expr.diagnostic.is_none());
    }

    #[test]
    fn incompatible_mul_annotates_diagnostic_on_send() {
        // `{} * {}` — Hash * Hash is NoMethodError in Ruby.
        let h = || {
            let mut e = synth(ExprNode::Hash {
                entries: vec![],
                kwargs: false,
            });
            e.ty = Some(Ty::Hash {
                key: Box::new(Ty::Sym),
                value: Box::new(Ty::Int),
            });
            e
        };
        let mul = synth(ExprNode::Send {
            recv: Some(h()),
            method: Symbol::from("*"),
            args: vec![h()],
            block: None,
            parenthesized: false,
        });

        let mut expr = mul;
        let classes = empty_classes();
        let typer = BodyTyper::new(&classes);
        typer.analyze_expr(&mut expr, &Ctx::default());

        let diag = expr.diagnostic.as_ref().expect("diagnostic set");
        assert!(matches!(
            diag,
            crate::diagnostic::DiagnosticKind::IncompatibleBinop { op, .. } if op.as_str() == "*"
        ));
    }

    #[test]
    fn incompatible_div_annotates_diagnostic_on_send() {
        // `"a" / 2` — String doesn't define `/`.
        let lhs = synth(ExprNode::Lit { value: Literal::Str { value: "a".to_string() } });
        let rhs = synth(ExprNode::Lit { value: Literal::Int { value: 2 } });
        let div = synth(ExprNode::Send {
            recv: Some(lhs),
            method: Symbol::from("/"),
            args: vec![rhs],
            block: None,
            parenthesized: false,
        });

        let mut expr = div;
        let classes = empty_classes();
        let typer = BodyTyper::new(&classes);
        typer.analyze_expr(&mut expr, &Ctx::default());

        let diag = expr.diagnostic.as_ref().expect("diagnostic set");
        assert!(matches!(
            diag,
            crate::diagnostic::DiagnosticKind::IncompatibleBinop { op, .. } if op.as_str() == "/"
        ));
    }

    #[test]
    fn compatible_add_leaves_diagnostic_empty() {
        // Int + Int must NOT be annotated — it's valid Ruby.
        let lhs = synth(ExprNode::Lit { value: Literal::Int { value: 1 } });
        let rhs = synth(ExprNode::Lit { value: Literal::Int { value: 2 } });
        let add = synth(ExprNode::Send {
            recv: Some(lhs),
            method: Symbol::from("+"),
            args: vec![rhs],
            block: None,
            parenthesized: false,
        });

        let mut expr = add;
        let classes = empty_classes();
        let typer = BodyTyper::new(&classes);
        typer.analyze_expr(&mut expr, &Ctx::default());

        assert!(expr.diagnostic.is_none());
    }

    // ── union_of lattice laws ────────────────────────────────────────
    //
    // `union_of` is THE type join: every fixpoint pass folds observed
    // types through it, and fixpoint convergence checks compare joins
    // with derived `==`. For that comparison to be semantic — and for
    // join results to be independent of the order call sites happen to
    // be visited in — the join must be commutative, associative, and
    // produce one canonical form per set of variants. These tests
    // enumerate a bounded universe of Ty shapes (chosen to cover every
    // special case inside union_of: Bottom filtering, container-spine
    // merging, nested-union flattening, dedup) and check the laws over
    // every pair and triple exhaustively.

    fn law_universe() -> Vec<Ty> {
        let class = |name: &str| Ty::Class {
            id: ClassId(Symbol::from(name)),
            args: vec![],
        };
        let arr = |elem: Ty| Ty::Array { elem: Box::new(elem) };
        let hash = |k: Ty, v: Ty| Ty::Hash {
            key: Box::new(k),
            value: Box::new(v),
        };
        vec![
            Ty::Int,
            Ty::Str,
            Ty::Time,
            Ty::Nil,
            Ty::Untyped,
            Ty::Bottom,
            Ty::Var { var: TyVar(7) },
            class("Story"),
            class("User"),
            Ty::Relation { of: ClassId(Symbol::from("Story")) },
            Ty::Relation { of: ClassId(Symbol::from("User")) },
            arr(Ty::Int),
            arr(Ty::Str),
            arr(Ty::Var { var: TyVar(3) }),
            hash(Ty::Str, Ty::Int),
            hash(Ty::Sym, Ty::Str),
            hash(
                Ty::Var { var: TyVar(1) },
                Ty::Var { var: TyVar(2) },
            ),
            Ty::Union { variants: vec![Ty::Str, Ty::Nil] },
            Ty::Union { variants: vec![Ty::Int, Ty::Str] },
            Ty::Union {
                variants: vec![hash(Ty::Str, Ty::Int), Ty::Nil],
            },
        ]
    }

    #[test]
    fn union_of_bottom_is_identity() {
        for t in law_universe() {
            assert_eq!(union_of(Ty::Bottom, t.clone()), t, "Bottom ⊔ {t:?}");
            assert_eq!(union_of(t.clone(), Ty::Bottom), t, "{t:?} ⊔ Bottom");
        }
    }

    #[test]
    fn union_of_is_idempotent() {
        for t in law_universe() {
            assert_eq!(union_of(t.clone(), t.clone()), t, "{t:?} ⊔ itself");
        }
    }

    #[test]
    fn union_of_is_commutative() {
        for a in law_universe() {
            for b in law_universe() {
                assert_eq!(
                    union_of(a.clone(), b.clone()),
                    union_of(b.clone(), a.clone()),
                    "{a:?} ⊔ {b:?} must equal {b:?} ⊔ {a:?}",
                );
            }
        }
    }

    #[test]
    fn union_of_bool_int_nil_is_structural_not_debug_order() {
        // Debug-string sort put Bool before Int; `ty_tag` puts Int first.
        let got = union_of(union_of(Ty::Bool, Ty::Int), Ty::Nil);
        assert_eq!(
            got,
            Ty::Union {
                variants: vec![Ty::Int, Ty::Bool, Ty::Nil]
            }
        );
    }

    #[test]
    fn union_of_is_associative() {
        let universe = law_universe();
        for a in &universe {
            for b in &universe {
                for c in &universe {
                    assert_eq!(
                        union_of(union_of(a.clone(), b.clone()), c.clone()),
                        union_of(a.clone(), union_of(b.clone(), c.clone())),
                        "({a:?} ⊔ {b:?}) ⊔ {c:?} must equal {a:?} ⊔ ({b:?} ⊔ {c:?})",
                    );
                }
            }
        }
    }

    #[test]
    fn union_of_absorbs_existing_variants() {
        // a ⊔ b then re-joining either operand must be a no-op —
        // this is what lets the fixpoint detect convergence.
        for a in law_universe() {
            for b in law_universe() {
                let joined = union_of(a.clone(), b.clone());
                assert_eq!(
                    union_of(joined.clone(), a.clone()),
                    joined,
                    "({a:?} ⊔ {b:?}) ⊔ {a:?} must absorb",
                );
                assert_eq!(
                    union_of(joined.clone(), b.clone()),
                    joined,
                    "({a:?} ⊔ {b:?}) ⊔ {b:?} must absorb",
                );
            }
        }
    }

    #[test]
    fn union_of_relations_has_no_cross_model_subtyping() {
        // `Relation{of: A} ⊔ Relation{of: A}` is itself (structural
        // equality); differing models join to a flat Union — the
        // lattice must NOT invent subtyping across models (no merged
        // spine like the Hash/Array pointwise join: two relations
        // over different models are genuinely distinct results).
        let rel = |name: &str| Ty::Relation { of: ClassId(Symbol::from(name)) };
        assert_eq!(union_of(rel("Story"), rel("Story")), rel("Story"));
        match union_of(rel("Story"), rel("Comment")) {
            Ty::Union { variants } => {
                assert_eq!(variants.len(), 2, "distinct models stay distinct variants");
                assert!(variants.iter().all(|v| matches!(v, Ty::Relation { .. })));
            }
            other => panic!("expected a union, got {other:?}"),
        }
    }

    #[test]
    fn union_of_merges_container_spines_through_unions() {
        // Regression pin for the associativity break: the Hash/Hash
        // structural join must fire even when one side arrives wrapped
        // in a Union (the `(H₁ ⊔ Nil) ⊔ H₂` association order). A flat
        // union carrying two Hash spines forces every consumer to fan
        // out per variant — union_of's own doc names that shape as the
        // harmful one.
        let h1 = Ty::Hash {
            key: Box::new(Ty::Str),
            value: Box::new(Ty::Int),
        };
        let h2 = Ty::Hash {
            key: Box::new(Ty::Sym),
            value: Box::new(Ty::Str),
        };
        let via_nil_first = union_of(union_of(h1.clone(), Ty::Nil), h2.clone());
        let via_hashes_first = union_of(union_of(h1.clone(), h2.clone()), Ty::Nil);
        assert_eq!(via_nil_first, via_hashes_first);

        let spines = match &via_nil_first {
            Ty::Union { variants } => variants
                .iter()
                .filter(|v| matches!(v, Ty::Hash { .. }))
                .count(),
            other => panic!("expected a union, got {other:?}"),
        };
        assert_eq!(spines, 1, "hash spines must merge, got {via_nil_first:?}");
    }

    #[test]
    fn concrete_value_shapes_are_truthy_for_boolean_operators() {
        let values = [
            Ty::Date,
            Ty::Tuple { elems: vec![Ty::Int] },
            Ty::Record { row: Row::default() },
            Ty::Fn {
                params: Vec::new(), block: None, ret: Box::new(Ty::Str),
                effects: crate::effect::EffectSet::pure(),
            },
        ];
        for left in values {
            assert!(never_falsy(&left), "{left:?} must short-circuit `||`");
            assert_eq!(falsy_part(&left), None, "{left:?} must yield the right arm of `&&`");
        }
    }

    #[test]
    fn nil_arms_of_concrete_value_unions_remain_falsy() {
        for truthy in [
            Ty::Date,
            Ty::Tuple { elems: vec![Ty::Int] },
            Ty::Record { row: Row::default() },
            Ty::Fn {
                params: Vec::new(), block: None, ret: Box::new(Ty::Str),
                effects: crate::effect::EffectSet::pure(),
            },
        ] {
            let left = Ty::Union { variants: vec![truthy, Ty::Nil] };
            assert!(!never_falsy(&left), "{left:?} must not always short-circuit `||`");
            assert_eq!(falsy_part(&left), Some(Ty::Nil), "{left:?} must retain nil for `&&`");
        }
    }

    #[test]
    fn case_match_hash_pattern_binds_value_omission_key_from_hash_value_ty() {
        use crate::expr::{MatchArm, MatchPattern};

        // case h
        // in {status: "ok", data:}
        //   data
        // end
        //
        // `h` is statically `Hash[Sym, Str]`; the value-omission bind
        // `data:` should type as the hash's uniform value type (`Str`),
        // and so should the arm body that reads it back.
        let h_ty = Ty::Hash { key: Box::new(Ty::Sym), value: Box::new(Ty::Str) };
        let pattern = MatchPattern::Hash {
            constant: None,
            pairs: vec![
                (
                    Symbol::from("status"),
                    Some(MatchPattern::Value {
                        expr: synth(ExprNode::Lit {
                            value: Literal::Str { value: "ok".to_string() },
                        }),
                    }),
                ),
                (Symbol::from("data"), None),
            ],
            rest: None,
        };
        let arm = MatchArm { pattern, guard: None, body: var("data") };
        let mut expr = synth(ExprNode::CaseMatch {
            scrutinee: var("h"),
            arms: vec![arm],
            else_body: None,
        });

        let classes = empty_classes();
        let typer = BodyTyper::new(&classes);
        let ctx = ctx_with_local("h", h_ty);
        let ty = typer.analyze_expr(&mut expr, &ctx);
        assert_eq!(ty, Ty::Str, "case/in result should be the arm body's type");

        let ExprNode::CaseMatch { arms, .. } = &*expr.node else {
            panic!("expected CaseMatch, got {:?}", expr.node);
        };
        assert_eq!(
            arms[0].body.ty,
            Some(Ty::Str),
            "`data` should read as the hash's value type, not Untyped"
        );
    }

    #[test]
    fn case_match_array_pattern_with_constant_binds_untyped() {
        use crate::expr::{MatchArm, MatchPattern};

        // case r
        // in Success(page)
        //   page
        // else
        //   nil
        // end
        //
        // `Success` is an unregistered (gem) class — a constant-
        // narrowed pattern's bindings type `Untyped`, a deliberate
        // gradual opt-out rather than an error (mirrors how a bare
        // `Const` read of an unknown class always resolves rather than
        // failing to type at all).
        let pattern = MatchPattern::Array {
            constant: Some(synth(ExprNode::Const { path: vec![Symbol::from("Success")] })),
            pre: vec![MatchPattern::Bind { name: Symbol::from("page") }],
            rest: None,
            post: vec![],
        };
        let arm = MatchArm { pattern, guard: None, body: var("page") };
        let mut expr = synth(ExprNode::CaseMatch {
            scrutinee: var("r"),
            arms: vec![arm],
            else_body: Some(nil_lit()),
        });

        let classes = empty_classes();
        let typer = BodyTyper::new(&classes);
        let ctx = ctx_with_local("r", Ty::Untyped);
        typer.analyze_expr(&mut expr, &ctx);

        let ExprNode::CaseMatch { arms, .. } = &*expr.node else {
            panic!("expected CaseMatch, got {:?}", expr.node);
        };
        assert_eq!(arms[0].body.ty, Some(Ty::Untyped));
    }
}

/// The arms of `ty` a falsy value can come from -- `nil`, `false`, and
/// anything not known to be truthy. `None` when there are none.
fn falsy_part(ty: &Ty) -> Option<Ty> {
    match ty {
        Ty::Union { variants } => {
            let kept: Vec<Ty> = variants.iter().filter_map(falsy_part).collect();
            match kept.len() {
                0 => None,
                1 => kept.into_iter().next(),
                _ => Some(Ty::Union { variants: kept }),
            }
        }
        t if never_falsy(t) => None,
        t => Some(t.clone()),
    }
}

/// Can this type NEVER be Ruby-falsy?
///
/// Ruby's falsy set is exactly `nil` and `false`; every other object,
/// `0` and `""` included, is truthy. So a type that admits neither
/// makes `x || y` short-circuit on every run, and the result is `x`'s
/// type rather than a union with `y`'s.
///
/// A whitelist, not a blacklist: `Untyped`, `Var` and anything else the
/// enum grows must fall through to the union, because being wrong here
/// drops a real arm from a real type.
fn never_falsy(ty: &Ty) -> bool {
    match ty {
        Ty::Int
        | Ty::Float
        | Ty::Str
        | Ty::Sym
        | Ty::Date
        | Ty::Time
        | Ty::Array { .. }
        | Ty::Hash { .. }
        | Ty::Tuple { .. }
        | Ty::Record { .. }
        | Ty::Relation { .. }
        | Ty::Class { .. }
        | Ty::Fn { .. } => true,
        // `Bool` is the whole point of the exclusion — it is the one
        // scalar that carries `false`.
        Ty::Union { variants } => variants.iter().all(never_falsy),
        _ => false,
    }
}

// Not the `(str, now)` form: only one argument is grounded to `ActiveSupport.parse_time` / `zone_parse`.
fn time_parse_ty(recv: &Expr, method: &Symbol, args: &[Expr]) -> Option<Ty> {
    if method.as_str() != "parse" || args.len() != 1 {
        return None;
    }
    if is_time_const(recv) {
        return Some(Ty::Time);
    }
    match &*recv.node {
        ExprNode::Send { recv: Some(r), method, args, block: None, .. }
            if method.as_str() == "zone" && args.is_empty() && is_time_const(r) =>
        {
            Some(Ty::Union { variants: vec![Ty::Time, Ty::Nil] })
        }
        _ => None,
    }
}

impl BodyTyper<'_> {
    // A declared constructor can return another class. Only the default
    // Class#new operation promises an instance of the receiving class.
    /// Whether the class-side `method` the receiver's class (or the
    /// nearest ancestor defining it) answers is registered as building an
    /// instance of the receiving class (`Ty::SelfInstance`).
    fn class_method_builds_receiver(&self, ty: Option<&Ty>, method: &Symbol) -> bool {
        let Some(Ty::Class { id, .. }) = ty else { return false };
        let mut next = Some(id);
        let mut seen = crate::det::HashSet::default();
        while let Some(id) = next {
            if !seen.insert(id) { break; }
            let Some(class) = self.classes().get(id) else { break };
            if let Some(found) = class.class_method(method) {
                return matches!(found, Ty::SelfInstance);
            }
            next = class.parent_id();
        }
        false
    }

    fn has_declared_constructor(&self, ty: Option<&Ty>) -> bool {
        let Some(Ty::Class { id, .. }) = ty else { return false };
        let mut next = Some(id);
        let mut seen = crate::det::HashSet::default();
        while let Some(id) = next {
            if !seen.insert(id) { break; }
            let Some(class) = self.classes().get(id) else { break };
            if class.declares_constructor
                || matches!(class.class_method(&Symbol::from("new")), Some(Ty::Fn { .. })) {
                return true;
            }
            next = class.parent_id();
        }
        false
    }
}

fn is_time_const(e: &Expr) -> bool {
    matches!(&*e.node, ExprNode::Const { path } if path.len() == 1 && path[0].as_str() == "Time")
}

// Not the value type for `expect(article: [:title, …])`: it answers the permitted Parameters; the array forms (`ids: []`, `[[…]]`) stay unmodeled until their lowering runs.
fn expect_hash_arg_ty(recv_ty: Option<&Ty>, method: &str, args: &[crate::expr::Expr]) -> Option<Ty> {
    use crate::expr::ExprNode;
    if method != "expect" {
        return None;
    }
    let (key, value) = match recv_ty {
        Some(Ty::Hash { key, value }) => (key.clone(), value.clone()),
        Some(Ty::Class { id, .. }) if id.0.as_str() == "ActionController::Parameters" => {
            (Box::new(Ty::Str), Box::new(Ty::Untyped))
        }
        _ => return None,
    };
    let [arg] = args else { return None };
    let ExprNode::Hash { entries, .. } = &*arg.node else { return None };
    let [(_, permitted)] = entries.as_slice() else { return None };
    let ExprNode::Array { elements, .. } = &*permitted.node else { return None };
    if elements.is_empty() || !elements.iter().all(|e| matches!(&*e.node, ExprNode::Lit { value: crate::expr::Literal::Sym { .. } })) {
        return None;
    }
    Some(Ty::Hash { key: key.clone(), value: value.clone() })
}

// Not every `params[...]` read: a scalar read stays a String, and only a call no String answers (or a Symbol-keyed index) reads the request's nested shape.
fn promotes_to_param_value(
    recv: &crate::expr::Expr,
    recv_ty: Option<&Ty>,
    method: &crate::ident::Symbol,
    args: &[crate::expr::Expr],
    locals: &HashMap<Symbol, Ty>,
) -> bool {
    use crate::expr::{ExprNode, Literal};
    // Keep the canonical strong-params chain intact for typed factory
    // discovery. Nested indexed values still use the request-value runtime.
    if method.as_str() == "permit"
        && matches!(&*recv.node, ExprNode::Send { recv: Some(root), method, .. }
            if method.as_str() == "require" && matches!(&*root.node,
                ExprNode::Send { recv: None, method, args, .. }
                if method.as_str() == "params" && args.is_empty()))
    {
        return false;
    }
    let stringish = match recv_ty {
        Some(Ty::Str) => true,
        Some(Ty::Union { variants }) => {
            variants.iter().any(|v| matches!(v, Ty::Str)) && variants.iter().all(|v| matches!(v, Ty::Str | Ty::Nil))
        }
        _ => false,
    };
    // Not only the source's `params`: after the controller lowering it reads `@params`, a `Hash[String, untyped]`.
    let untyped = match recv_ty {
        Some(Ty::Untyped) => true,
        Some(Ty::Union { variants }) => variants.iter().all(|v| matches!(v, Ty::Untyped | Ty::Nil)),
        _ => false,
    };
    // The fork models the full parameter-value union before this
    // contextual runtime lowering, including nested Parameters/uploads.
    let parameter_value = recv_ty.is_some_and(|ty| match ty {
        Ty::Union { variants } => variants.iter().any(|v|
            matches!(v, Ty::Class { id, .. } if id.0.as_str() == "ActionController::Parameters")),
        _ => false,
    });
    let lowered = untyped && is_ivar_params_rooted(recv, locals);
    if !((stringish || parameter_value) && is_params_rooted(recv, locals)) && !lowered {
        return false;
    }
    let keyed = matches!(
        method.as_str(),
        "[]" | "[]=" | "fetch" | "dig" | "key?" | "has_key?" | "delete" | "slice" | "except"
    ) && matches!(args.first().map(|a| &*a.node), Some(ExprNode::Lit { value: Literal::Sym { .. } }));
    let structural = STRUCTURAL.contains(&method.as_str());
    keyed || (structural && matches!(send::str_method(method), Ty::Var { .. }))
}

// Not a field on `Ctx`: a name no Ruby local can take rides in `local_bindings`, which every scope already clones.
fn param_local_mark(name: &Symbol) -> Symbol {
    Symbol::from(format!("#params-local:{}", name.as_str()).as_str())
}

fn is_param_local(e: &crate::expr::Expr, locals: &HashMap<Symbol, Ty>) -> bool {
    matches!(&*e.node, crate::expr::ExprNode::Var { name, .. } if locals.contains_key(&param_local_mark(name)))
}

fn reads_structurally(e: &crate::expr::Expr, name: &Symbol) -> bool {
    use crate::expr::{ExprNode, Literal};
    if let ExprNode::Send { recv: Some(r), method, args, .. } = &*e.node {
        if matches!(&*r.node, ExprNode::Var { name: n, .. } if n == name) {
            let keyed = matches!(method.as_str(), "[]" | "[]=" | "fetch" | "dig" | "key?" | "has_key?")
                && matches!(args.first().map(|a| &*a.node), Some(ExprNode::Lit { value: Literal::Sym { .. } }));
            if keyed || STRUCTURAL.contains(&method.as_str()) {
                return true;
            }
        }
    }
    let mut found = false;
    e.node.for_each_child(&mut |c| found = found || reads_structurally(c, name));
    found
}

const STRUCTURAL: &[&str] = &[
    "each", "each_pair", "each_value", "each_key", "each_with_index", "reverse_each", "map", "collect",
    "flat_map", "filter_map", "select", "filter", "reject", "any?", "all?", "none?", "keys", "values", "to_a",
    "first", "last", "compact", "uniq", "sort", "sort_by", "merge", "key?", "has_key?", "dig", "permit",
    "permit!", "to_unsafe_h", "require",
];

fn is_params_rooted(e: &crate::expr::Expr, locals: &HashMap<Symbol, Ty>) -> bool {
    use crate::expr::ExprNode;
    match &*e.node {
        ExprNode::Send { recv: None, method, args, .. } => method.as_str() == "params" && args.is_empty(),
        ExprNode::Send { recv: Some(r), method, .. } => {
            matches!(method.as_str(), "[]" | "fetch" | "dig" | "require") && is_params_rooted(r, locals)
        }
        _ => is_param_local(e, locals),
    }
}

fn is_ivar_params_rooted(e: &crate::expr::Expr, locals: &HashMap<Symbol, Ty>) -> bool {
    use crate::expr::ExprNode;
    match &*e.node {
        ExprNode::Send { recv: Some(r), method, .. }
            if matches!(method.as_str(), "[]" | "fetch" | "dig" | "slice" | "except" | "to_h" | "merge") =>
        {
            matches!(&*r.node, ExprNode::Ivar { name } if name.as_str() == "params")
                || is_ivar_params_rooted(r, locals)
                || is_param_local(r, locals)
        }
        _ => is_param_local(e, locals),
    }
}

/// `[a, b].min` / `.max` (no count, no block) on an array LITERAL: the
/// literal has at least one element, so the extremum is an element, never
/// the `nil` an empty collection would give. `[x, LIMIT].min` is the
/// clamp idiom; typing it `T?` (as `Enumerable#min` does for an arbitrary
/// array) makes every `MAX - [x, LIMIT].min` look like arithmetic on nil.
///
/// Nil is stripped from the element type only when some element excludes
/// Nil: comparing nil with a non-nil value raises, so a nil result cannot
/// occur. A single nilable element (`[maybe].min`) or an all-nilable
/// literal can still return nil without a comparison error.
/// What a receiverless Kernel conversion function returns: `Integer(x)`,
/// `Float(x)`, `String(x)`, `BigDecimal(x)`, `Rational(a, b)` and
/// `Complex(a, b)`. With an `exception:` keyword the call can answer nil
/// instead of raising (`Integer("x", exception: false)`); `String` takes
/// none. Arity outside Ruby's signatures is no answer.
fn kernel_conversion_ty(method: &Symbol, args: &[Expr]) -> Option<Ty> {
    let (positional, keywords): (Vec<&Expr>, Vec<&Expr>) = args.iter().partition(|arg| {
        !matches!(&*arg.node, ExprNode::Hash { kwargs: true, .. })
    });
    let exception = match keywords.as_slice() {
        [] => false,
        [hash] => match &*hash.node {
            ExprNode::Hash { entries, .. } => {
                let only_exception = entries.iter().all(|(key, _)| {
                    matches!(&*key.node, ExprNode::Lit { value: crate::expr::Literal::Sym { value } } if value.as_str() == "exception")
                });
                if !only_exception || entries.is_empty() { return None; }
                true
            }
            _ => return None,
        },
        _ => return None,
    };
    let class = |name: &str| Ty::Class { id: ClassId(Symbol::from(name)), args: vec![] };
    let (ty, arity) = match method.as_str() {
        // `Integer(string, base)`.
        "Integer" => (Ty::Int, 1..=2),
        "Float" => (Ty::Float, 1..=1),
        "String" if !exception => (Ty::Str, 1..=1),
        // `BigDecimal(value, digits)`.
        "BigDecimal" => (class("BigDecimal"), 1..=2),
        "Rational" => (class("Rational"), 1..=2),
        "Complex" => (class("Complex"), 1..=2),
        _ => return None,
    };
    if !arity.contains(&positional.len()) { return None; }
    Some(if exception { Ty::Union { variants: vec![ty, Ty::Nil] } } else { ty })
}

fn literal_extremum_ty(recv: Option<&Expr>, recv_ty: &Ty, method: &Symbol, args: &[Expr]) -> Option<Ty> {
    if !matches!(method.as_str(), "min" | "max") || !args.is_empty() {
        return None;
    }
    let recv = recv?;
    let ExprNode::Array { elements, .. } = &*recv.node else { return None };
    if elements.is_empty() {
        return None;
    }
    let Ty::Array { elem } = recv_ty else { return None };
    // A nil result needs every element to be nilable. One non-nilable
    // element makes a nil result raise instead of returning nil.
    let some_non_nil = elements.iter().any(|e| match e.ty.as_ref() {
        Some(Ty::Nil) | None => false,
        Some(Ty::Union { variants }) => !variants.iter().any(|v| matches!(v, Ty::Nil)),
        Some(_) => true,
    });
    if !some_non_nil {
        return None;
    }
    Some(match &**elem {
        Ty::Union { variants } => {
            let kept: Vec<Ty> = variants.iter().filter(|v| !matches!(v, Ty::Nil)).cloned().collect();
            match kept.len() {
                0 => return None,
                1 => kept.into_iter().next().unwrap(),
                _ => Ty::Union { variants: kept },
            }
        }
        other => other.clone(),
    })
}

// Exact top-level stdlib names used by receiver dispatch.
pub(super) const RUBY_TOP_LEVEL: &[&str] = &[
    "Base64", "Benchmark", "BigDecimal", "CGI", "CSV", "Comparable", "Complex", "Coverage",
    "Date", "DateTime", "Digest", "Dir", "ERB", "Encoding", "Enumerable", "Errno", "Etc",
    "Fiber", "File", "FileUtils", "Find", "Forwardable", "GC", "IO", "IPAddr", "JSON",
    "Kernel", "Logger", "Marshal", "Math", "Monitor", "Mutex", "Net", "ObjectSpace", "Open3",
    "OpenSSL", "OpenStruct", "PP", "Pathname", "Prism", "Process", "Psych", "Random",
    "Rational", "Ripper", "SecureRandom", "Set", "Shellwords", "Signal", "Singleton",
    "Socket", "StringIO", "Struct", "Tempfile", "Thread", "Time", "Timeout", "URI", "YAML",
    "Zlib",
];

/// The type a block parameter's default gives it: a non-nil literal's
/// own type, else Untyped (a call may pass anything).
fn block_default_ty(default: &Expr, ty: Ty) -> Ty {
    match &*default.node {
        ExprNode::Lit { value } if !matches!(value, crate::expr::Literal::Nil) => ty,
        _ => Ty::Untyped,
    }
}

/// The ivars `expr` reads or writes, short of a nested expression-position
/// `def`, whose own self they would be.
fn ivars_of_untyped_self(expr: &Expr, out: &mut Vec<crate::ident::Symbol>) {
    match &*expr.node {
        ExprNode::MethodDefExpr { .. } => return,
        ExprNode::Ivar { name } | ExprNode::Assign { target: LValue::Ivar { name }, .. } => {
            if !out.contains(name) {
                out.push(name.clone());
            }
        }
        _ => {}
    }
    expr.node.for_each_child(&mut |child| ivars_of_untyped_self(child, out));
}

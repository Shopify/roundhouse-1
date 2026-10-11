//! The whole-program IR root: a Rails application as one data
//! structure. Ingest produces an `App`, analyze annotates it in place,
//! the post-analyze lowerings reshape it, and every emitter consumes
//! it — this struct is the deliverable each stage hands the next. It
//! is serde-serializable end to end because tests, the wasm build, and
//! the IR dump round-trip it as JSON (`schema_version` names the
//! shape), so a field that can't serialize can't join the IR. Beyond
//! the core sections (schema, models, controllers, routes, views),
//! the trailing maps persist facts analyze already computed — partial
//! local types, view ivar contexts, render edges — so lowerers and
//! IDE consumers read them instead of re-deriving.

use std::collections::{BTreeMap, BTreeSet};
use crate::det::HashMap;

use serde::{Deserialize, Serialize};

use crate::dialect::{
    Controller, Filter, Fixture, LibraryClass, MethodDef, Model, ModelBodyItem, RouteTable,
    TestModule, View,
};
use crate::expr::Expr;
use crate::ident::{ClassId, Symbol};
use crate::schema::Schema;
use crate::ty::Ty;

/// The top-level IR: a Rails application as data. This is the serializable
/// deliverable — the thing ingesters produce and emitters consume.
/// One `establish_connection` call in the test helper.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HelperConnection {
    pub call: Expr,
    /// A statement of the file itself, which the helper runs once and
    /// unconditionally as it loads: not under a condition, in a class
    /// body or `begin`, or in a method.
    pub top_level: bool,
}

/// Which `def` a call-site parameter observation belongs to: the
/// class, the method name, and the side it is defined on. A class
/// method and an instance method of the same name (`def self.call` /
/// `def call`, every service object) are different methods with
/// different parameters; keyed by (class, name) alone, each one's
/// call sites typed the other's params.
pub type ParamKey = (ClassId, Symbol, crate::dialect::MethodReceiver);

/// One include's carried `class_attribute`s (`App::included_class_attributes`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct IncludedClassAttributes {
    /// The includer's own include the Concern hangs off.
    pub after: ClassId,
    /// The generated defs, by receiver and name.
    pub methods: Vec<(crate::dialect::MethodReceiver, Symbol)>,
    /// The seeds (`@__class_attr_x = default`, `self.x = []`), by span.
    pub seeds: Vec<crate::span::Span>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct App {
    pub schema_version: u32,
    /// The primary database's schema. For an app with a
    /// `database_configurations` manifest it is the schema the manifest
    /// selects for the primary configuration; otherwise `db/schema.rb`,
    /// else `db/structure.sql`, else the folded migrations.
    pub schema: Schema,
    /// The manifest's database configurations for its environment, in
    /// manifest order; empty for an app without a manifest
    /// ([`crate::database`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub databases: Vec<crate::database::Database>,
    /// Each model's database, read from source (`connects_to` through
    /// abstract parents, app-defined connection overrides). Empty when
    /// the app has no manifest and no model declares any routing.
    #[serde(default, skip_serializing_if = "indexmap::IndexMap::is_empty")]
    pub model_databases: indexmap::IndexMap<ClassId, crate::database::ModelDatabase>,
    /// Database-identity facts the emitted runtime does not carry (a
    /// stale manifest, a second database, unresolvable routing), each
    /// reported as an error by `analyze::database_identity`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub database_refusals: Vec<crate::database::DatabaseRefusal>,
    /// `config/database.yml`, compiled for boot-time resolution; `None`
    /// when the app states no compiled-for adapter ([`crate::database::DatabaseYml`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database_yml: Option<crate::database::DatabaseYml>,
    pub models: Vec<Model>,
    /// Non-model classes living under `app/models/` (e.g. specialized
    /// has_many proxies). Classified at ingest time by superclass:
    /// extends ApplicationRecord/ActiveRecord::Base → `models`;
    /// otherwise → here.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub library_classes: Vec<LibraryClass>,
    pub controllers: Vec<Controller>,
    pub routes: RouteTable,
    pub views: Vec<View>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub test_modules: Vec<TestModule>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fixtures: Vec<Fixture>,
    /// Body of `db/seeds.rb` as a typed expression (usually a
    /// `Seq` of AR-create calls with an early-return guard). The
    /// TS emitter wraps it in `async function run()` and the
    /// generated `main.ts` invokes it at startup when the DB is
    /// fresh. None when the app has no seeds file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seeds: Option<Expr>,
    /// Pins from `config/importmap.rb`, expanded (each
    /// `pin_all_from` has been resolved into explicit per-file
    /// pins via `app/javascript/**` walking). Consumed by the
    /// `<%= javascript_importmap_tags %>` view-helper lowering.
    /// None when the app has no importmap.rb.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub importmap: Option<Importmap>,
    /// Logical stylesheet names discovered in `app/assets/stylesheets/`
    /// + `app/assets/builds/` (file stems without `.css`). When the
    /// ERB uses `stylesheet_link_tag :app, ...`, Rails with Propshaft
    /// + tailwindcss-rails expands to one `<link>` per stylesheet in
    /// these dirs; our emitter mirrors the expansion so the rendered
    /// head matches structurally.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stylesheets: Vec<String>,
    /// User-authored RBS sidecars discovered under `sig/**/*.rbs` in
    /// the Rails app root. Keyed by fully-qualified class/module name
    /// (nested namespaces joined with `::`), inner map is method name
    /// → signature (`Ty::Fn`). The analyzer consults these when
    /// building `ClassInfo` so user methods the Rails conventions
    /// can't fully type (helpers, concerns, POROs) still flow types.
    /// Empty when the app ships no `sig/` directory.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub rbs_signatures: HashMap<ClassId, HashMap<Symbol, Ty>>,
    /// The `rbs_signatures` entries a source `sig`/`#:` declared on the
    /// SINGLETON side (`def self.x`, a `def` in `class << self`). The
    /// analyzer registers those in `class_methods`; every other entry is
    /// an instance method (or a sidecar's, whose side is not recorded).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub rbs_singleton_methods: HashMap<ClassId, crate::det::HashSet<Symbol>>,
    /// What a `sig/**/*.rbs` sidecar says a class INCLUDES, keyed by
    /// the declaring class and holding the module names as written.
    ///
    /// Ancestry is the one fact about a gem's base class that the tree
    /// cannot see for itself, and guessing it from a method name is
    /// what `docs/guide/transpile.md` says not to do. A sidecar is
    /// where an app states it.
    pub rbs_includes: HashMap<ClassId, Vec<ClassId>>,
    /// The app's `Gemfile.lock`, parsed, when the tree carries one.
    /// Read at ingest for the gem census ([`crate::gems`]) and the
    /// unknown-gem attribution of diagnostics, and by the view lowering
    /// for the one gem that changes a framework helper's markup: `lexxy`
    /// replaces Action Text's `rich_text_area`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gem_lock: Option<crate::gems::Lockfile>,
    /// The classes and method headers the locked gems' Tapioca RBIs
    /// declare ([`crate::gem_boundary`]). Declarations only, read at
    /// ingest when the tree (or the real location of its lockfile) has
    /// `sorbet/rbi/gems/`; empty otherwise.
    #[serde(default, skip_serializing_if = "crate::gem_boundary::GemBoundary::is_empty")]
    pub gem_boundary: crate::gem_boundary::GemBoundary,
    /// Attributes the app (and its gems) add to Action Text's sanitizer
    /// allow-list at boot — `ActionText::ContentHelper.allowed_attributes`
    /// as the initializers leave it, minus the framework defaults the
    /// runtime already has. Resolved to literals at ingest
    /// (`ingest::app::content_helper_attribute_additions`); rendered into
    /// the runtime's generated `ContentHelper.app_allowed_attributes`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub content_helper_allowed_attributes: Vec<String>,
    /// Each `establish_connection` call in `test/test_helper.rb`, as
    /// written. `analyze::test_helper_connection` decides whether the
    /// emitted harness can open the database it names
    /// (`honored_database`) and refuses every other.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub test_helper_connections: Vec<HelperConnection>,
    /// What else in the app could set or change the database the tests
    /// run on, which the harness does not carry: in the helper, a
    /// statement that runs against the database, a local file it loads,
    /// or an exit before its connection; elsewhere, another
    /// `establish_connection`. Each names its file. A helper connection
    /// the harness could otherwise open is refused while any is present.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub test_helper_connection_blockers: Vec<String>,
    /// Call-site-unified parameter types, written by the analyzer's
    /// fixpoint (`unify_params_from_call_sites`) as its last act and
    /// read by lowerings that build method signatures — the controller
    /// lowering's private-helper params were pinned `untyped` before
    /// this channel existed, which dissolved everything reached
    /// through such a param (campfire's `broadcast_create_room(room)`
    /// took the typed `Room` its one caller passes, and `room` still
    /// widened every chain under it to poly). Keyed like the
    /// analyzer's own table: (declaring class, method) → positional
    /// param types, `Ty::Var` where no call site contributed.
    #[serde(skip)]
    pub inferred_method_params: HashMap<ParamKey, Vec<Ty>>,
    /// Source files the text pipeline cannot represent — every emitted
    /// file's content is a `String`, so an app's images, fonts and
    /// binary test fixtures were silently dropped on the floor. Carried
    /// as `(relative path, bytes)` and copied VERBATIM into the emitted
    /// tree; there is nothing to transpile in a JPEG. The app's
    /// `config/**/*.json` data rides along the same way
    /// (`ingest::app::collect_config_data`).
    ///
    /// `serde(skip)`: these are a passthrough concern, not IR. The app
    /// JSON round-trip feeds the analyzer and the browser IDE, and
    /// neither has any use for blob bytes — encoding them as JSON number
    /// arrays would bloat every dump for nothing.
    #[serde(skip)]
    pub binary_assets: Vec<(String, Vec<u8>)>,
    /// The app's own I18n locale files (`config/locales/**/*.{yml,rb}`
    /// at the root and under each component), app-relative and sorted.
    /// Rails resolves an error TYPE (`errors.add(:a, :blank)`) and the
    /// attribute's human name through this data, so a lowering that
    /// would bake Rails' default text declines when it is non-empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub locale_files: Vec<String>,
    /// App-helper method registry: maps each method name defined in an
    /// `app/helpers/*.rb` module to the helper module (`ClassId`) that
    /// defines it. Rails mixes all helper modules into every view, so a
    /// bare `avatar_img(...)` in a template should resolve to the helper
    /// that declares it. The ruby emit-path helper-lowering pass uses this
    /// to (a) rewrite such bare calls to `<Module>.method(...)` and (b)
    /// emit the helper modules as module-functions. Last-writer-wins on a
    /// name collision (mirrors Rails include order). Empty when the app
    /// ships no helpers or only empty helper modules (the blog).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub helper_method_index: HashMap<Symbol, ClassId>,
    /// Per helper module, the methods it defines on its SINGLETON
    /// (`def self.x`, `class << self`, a Concern's class side) rather
    /// than as instance methods or `module_function` / `extend self`
    /// copies. Both land as `MethodReceiver::Class`; only the latter are
    /// in [`Self::helper_method_index`], because Rails exposes a helper's
    /// instance methods to views and includers and nothing else. Read by
    /// the Ruby helper lowering, which flips the instance surface onto
    /// the module's singleton and so has to keep the two apart.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub helper_singleton_methods: BTreeMap<ClassId, BTreeSet<Symbol>>,
    /// Controller methods the app declared view-visible with Rails'
    /// `helper_method :name`. A view lowers to a module function with no
    /// controller instance, so a bare call to one of these routes
    /// through the per-dispatch `ActionController::Current.controller`
    /// — the same seam `flash` and `cookies` already use.
    ///
    /// Distinct from [`Self::helper_method_index`], which maps a name to
    /// the `app/helpers/` MODULE that defines it. This one names methods
    /// that live on the CONTROLLER.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub view_visible_controller_methods: BTreeSet<Symbol>,
    /// Methods of a helper object the app builds at load time from a
    /// YAML table (`WebUrlHelpers = WebUrlHelpersFactory.create(
    /// YAML.load_file(…)["paths"])`), keyed by the constant. The names
    /// come from the table's keys and the factory's `define_method`
    /// suffixes — see [`crate::ingest::generated_helpers`]. A registry
    /// fact for the analyzer only: the constant's initializer still
    /// builds the object at runtime, so nothing is emitted for it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub generated_helper_methods: BTreeMap<ClassId, BTreeSet<Symbol>>,
    /// Model class names a `GlobalID::Locator.locate(gid, only: K)`
    /// call site names, collected by [`crate::lower::global_id_locate`]
    /// as it rewrites each site to a per-model `locate_<model>`.
    ///
    /// The generic `locate` takes its finder as a CLASS OBJECT, and a
    /// class object has no singleton on a strict target — `only.find`
    /// emits a call to a class method `ActiveRecord::Base` never
    /// defines (matz/spinel#4217). The set is what
    /// `project::apply_global_id_locate` needs to write one
    /// specialization per model into `runtime/global_id_locator.rb`,
    /// where the finder is spelled as a literal constant.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub global_id_locate_models: BTreeSet<Symbol>,
    /// Model class names a `GlobalID::Locator.locate_signed(sgid,
    /// only: K, for:)` call site names. Same monomorphize reason as
    /// [`Self::global_id_locate_models`]; the generated entry point is
    /// `locate_signed_<model>(sgid, purpose)`.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub global_id_locate_signed_models: BTreeSet<Symbol>,
    /// Model names whose attachment sgid the app resolves even when
    /// its SIGNATURE fails — campfire's `%w[ User ]`, read by
    /// [`crate::ingest::on_load_reopen`] from the `from_node` reopen
    /// in its `lib/rails_ext/`, so that rotating `SECRET_KEY_BASE`
    /// does not orphan every @mention. In source order, as the app
    /// wrote them. `project::apply_attachable_locate` writes the list
    /// into the emitted `global_id_locator.rb` as
    /// `ActionText::Attachment.permitted_without_signature`; empty for
    /// every app without the reopen, where a tampered sgid is missing
    /// as it is in stock Rails.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachable_unsigned_models: Vec<Symbol>,
    /// Pending `on_load(:active_storage_attachment)` includes-only
    /// reopens: `(model_name, module_paths)`. File order can visit the
    /// load-hook file before the model; ingest applies these once
    /// `app.models` is complete, just before the concern splice.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_attachment_on_load: Vec<(Symbol, Vec<Symbol>)>,
    /// Modules `include`d inside `ActiveSupport.on_load(:active_record)`
    /// that provide class-method macros. Mixin instance methods are not
    /// installed. Expansion treats these as an explicit provider origin
    /// (not a seeded `include` set on every model).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub load_hook_class_macros: Vec<ClassId>,
    /// Modules boot includes into ActiveRecord::Base, in the order found
    /// (`ingest::base_includes`): a carried railtie's, a support file's
    /// or a boot initializer's `ActiveSupport.on_load(:active_record) {
    /// include M }` (or `ActiveRecord::Base.include(M)`, `.send(:include,
    /// M)`), as shopify-money's MoneyColumn::Railtie includes
    /// MoneyColumn::ActiveRecordHooks. Every model inherits them, so the
    /// class methods their `included` hook extends onto the base are
    /// class macros of every model (`macro_replay::ClassSide`). Gem
    /// modules count: the tree carries their source.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub framework_base_includes: Vec<ClassId>,
    /// Modules a lazy-load hook other than `:active_record` includes at
    /// run time into the hooked base (`ingest::base_includes`): a carried
    /// railtie's or boot program's `ActiveSupport.on_load(:active_support_test_case)
    /// { include M }`. The base is no class the app writes, so there is
    /// no includer to splice a Concern's `ClassMethods` into; the module
    /// keeps its `extend ActiveSupport::Concern` instead.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub load_hook_includes: Vec<ClassId>,
    /// Partial → local name → type, harvested by the analyzer from the
    /// RENDER SITES that pass each local (`render partial: "form",
    /// locals: { new_message: @new_message }` with `@new_message` typed
    /// `Message` by the controller's `Message.new`).
    ///
    /// The analyzer already computed this to seed each partial's body
    /// typing; recording it on the App is what lets the LOWERER stamp
    /// the same fact into the emitted signature. Without it the view
    /// lowerer can only guess a param's type from its NAME (`user` →
    /// `User`), so a local whose name isn't a model — lobsters'
    /// `new_message` — emits `untyped` and every read off it refuses on
    /// the strict targets, even though the analyzer knew the type all
    /// along. Empty for apps whose partials take no locals.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub partial_local_types: HashMap<Symbol, HashMap<Symbol, Ty>>,
    /// View → ivar name → type: the CONTROLLER-side ivar context each
    /// view renders against (`settings/index` sees `@edit_user: User`
    /// because the action assigns `@user.dup`), propagated transitively
    /// onto partials.
    ///
    /// The naming convention gets an ivar's type right whenever the name
    /// IS the model (`@user` → User) and cannot possibly get it right
    /// otherwise. `form_with model: @edit_user` is the case that bites:
    /// Rails names its fields from the RECORD's `param_key`
    /// (`user[username]`), and with no type for `@edit_user` the lowerer
    /// falls back to the view directory (`setting[username]`) — every
    /// field name and id on /settings wrong. The analyzer already
    /// computed this context to type each view body; recording it is
    /// what lets the lowerer name the form after the record.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub view_ivar_types: HashMap<Symbol, HashMap<Symbol, Ty>>,
    /// Methods whose result Rails would treat as an html-safe buffer,
    /// because their body ends in `<e>.html_safe` (lobsters'
    /// `Hat#to_html_label`). The mark is a VALUE-level fact in Rails,
    /// carried by a String subclass the shared runtime cannot have, so
    /// `lower::html_safe` records it here and erases the call. The view
    /// lowerer consults it before wrapping an interpolation in
    /// `html_escape` — escaping a marked result ships literal
    /// `&lt;span&gt;` markup to the page.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub html_safe_methods: BTreeSet<Symbol>,
    /// App-registered `to_fs` formats from `config/initializers/`, in
    /// both spellings Rails accepts: the current
    /// `ActiveSupport::TimeFormats.register(:name, fmt)` and the
    /// deprecated `Time::DATE_FORMATS[:name] = fmt` (campfire's
    /// `time_formats.rb` defines `:epoch` as
    /// `->(time) { (time.to_f * 1000).to_i }`).
    ///
    /// Recorded because `to_fs(:name)` is otherwise unknowable and
    /// Rails' fallback for an unknown format is `to_s` — a completely
    /// different value, silently. `lower::time_current` expands each
    /// call site from this. Empty for apps that register no formats.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub time_formats: BTreeMap<Symbol, TimeFormat>,
    /// `X.prepend Y` / `X.include Y` registered by a
    /// `config/initializers/` file — the one initializer shape that
    /// CHANGES METHOD LOOKUP, and therefore the one a tree cannot
    /// silently drop.
    ///
    /// campfire's `turbo_streams_authorization.rb` is the case that
    /// forced this: `Turbo::StreamsChannel.prepend
    /// RoomStreamsAreAuthorized` is what makes `RoomMessagesChannel`
    /// the only door onto a room's message stream. The concern itself
    /// ingests (it lives in `app/channels/concerns/`), so dropping the
    /// prepend left the guard defined and unreachable — a security
    /// control present in the tree and not in the lookup chain.
    ///
    /// Recorded here rather than executed at ingest because the mixin
    /// only means something once BOTH constants exist in the emitted
    /// tree; `lower::module_mixins` drops the ones that cannot resolve
    /// and reports them, so a tree never carries a line that would
    /// NameError at boot.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub module_mixins: Vec<ModuleMixin>,
    /// `X.before_action :m[, only: …]` registered by a
    /// `config/initializers/` file — the mixin's companion: an app
    /// that mixes a guard INTO a framework controller also has to
    /// tell that controller to RUN it, and Rails apps write the two
    /// lines together. campfire's `active_storage_authentication.rb`
    /// includes `ActiveStorageAuthentication` into Active Storage's
    /// direct-upload controllers and adds the `before_action` that
    /// makes an anonymous upload a 401. Same standing as
    /// `module_mixins`: recorded at ingest, resolved by
    /// `lower::module_mixins`, which keeps only a filter onto a
    /// runtime controller that carries the seam and whose method a
    /// kept mixin supplies.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub initializer_filters: Vec<InitializerFilter>,
    /// Every `config/initializers/**/*.rb` Rails runs at boot, in its
    /// load order: each in-repo engine's files (engines in the order
    /// their classes are defined), then the application's, each
    /// railtie's files sorted by full path
    /// (`ingest::initializers::initializer_files`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub initializer_files: Vec<String>,
    /// Top-level initializer statements no ingest reader consumed, in
    /// [`Self::initializer_files`] order. Each is code Rails runs at
    /// boot and the emitted app does not, reported as an
    /// `initializer_unsupported` error by `analyze::initializer_coverage`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub initializer_gaps: Vec<InitializerGap>,
    /// The top-level constants boot initializers assign, one entry per
    /// initializer, in [`Self::initializer_files`] order. Rails defines
    /// them as each initializer runs; the CRuby boot requires each
    /// holder's file in this order (`ingest::initializer_constants`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub initializer_constants: Vec<InitializerConstants>,
    /// The statements of each boot initializer the emitted boot runs,
    /// one holder per initializer, in [`Self::initializer_files`] order
    /// (`ingest::initializer_programs`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub initializer_programs: Vec<InitializerProgram>,
    /// Each `config/environments/<env>.rb`, as the program the emitted
    /// boot runs when `Rails.env` selects it, in file-name order
    /// (`ingest::environments`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub environment_programs: Vec<EnvironmentProgram>,
    /// Framework load hooks the app's initializers register for, with
    /// the base Rails fires each with (`ingest::initializer_programs`).
    /// The emitted boot fires them where the runtime has loaded the base.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub framework_load_hooks: Vec<FrameworkLoadHook>,
    /// `ActiveSupport.run_load_hooks(:hook, base)` written in a class or
    /// module body, or at the top level of an app file, after the class
    /// it names: the emitted class file ends with it
    /// (`ingest::initializer_programs::class_load_hooks`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub class_load_hooks: Vec<ClassLoadHook>,
    /// Each load hook the app's code fires, with the base it fires it
    /// with (`Object` when it passes none): the `self` of an initializer's
    /// `ActiveSupport.on_load(:hook)` block.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub load_hook_bases: std::collections::BTreeMap<String, String>,
    /// The file-level statements of support files that their file
    /// programs (`App::file_programs`) do not carry
    /// (`ProgramItem::NotCarried`): a statement the expression ingest
    /// refuses, a method defined in a block, a declaration inside a
    /// `case` or `begin`. Ruby runs each when it loads the file; the
    /// emitted app names it at boot. Each is a
    /// `file_statement_unsupported` error at the statement
    /// (`analyze::ingest_coverage`).
    pub file_statement_gaps: Vec<InitializerGap>,
    /// How many source declarations ingest merged into each library
    /// class reopened across declarations (`class X … end` again, in
    /// this file or another), for classes with more than one that
    /// declares something on the class (a namespace-only reopen around a
    /// nested class is not counted). The IR
    /// holds one class per name; this keeps the fact that its body came
    /// in pieces whose relative load order Ruby decides. The forwarding
    /// contracts count them as fragments, and a method that two files
    /// both define is reported (`analyze::ingest_coverage`).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub library_class_reopens: HashMap<crate::ident::ClassId, usize>,
    /// `ActiveSupport.on_load(:hook)` blocks of boot initializers, lifted
    /// onto the Application reopen in boot order
    /// (`ingest::on_load_hooks`); the emitted boot registers each one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub load_hooks: Vec<LoadHook>,
    /// `Rails.autoloaders.main.on_load("Model") { Model.include(M) }`
    /// callbacks of boot initializers whose includes ingest appended to
    /// the model's body (`ingest::load_hook_includes::
    /// lift_autoload_includes`); the boot registers each one empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lifted_autoload_callbacks: Vec<LiftedAutoloadCallback>,
    /// Source files whose library-class ingest failed and that ingest
    /// went on without (the isolate-tolerant support-dir walk; in survey
    /// mode, every per-file class ingest): none of their classes,
    /// modules or constants are in the emitted app. Each is an
    /// `ingest_unsupported` error at the construct that failed it
    /// (`analyze::ingest_coverage`); survey mode also lists the gap in
    /// its punch list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ingest_drops: Vec<IngestDrop>,
    /// The `Rails::Railtie` and `Rails::Engine` subclasses the support
    /// walk carried (a gem source's railtie, an in-repo engine), in the
    /// order Rails defines them: railties, then engines in the order
    /// their initializers run (`ingest::initializers::
    /// engines_in_initializer_order`). The PostgreSQL CRuby tree loads
    /// them at boot in this order, so `Railtie.subclasses` sorts as
    /// Rails' does, and runs their initializers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub railties: Vec<CarriedRailtie>,
    /// Each support file whose top level runs more than declarations
    /// when Ruby loads it (`ingest::library_class::file_program`): its
    /// statements, conditional definitions and the definitions behind a
    /// file-level `return`, in source order. The Ruby family emits each
    /// as one file the boot loads (`app/models/roundhouse_files/…`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub file_programs: Vec<FileProgram>,
    /// The app files native boot requires from the boot files the tree
    /// does not carry (`config/boot.rb`, `config/application.rb` and its
    /// `Bundler.require`, `config/environment.rb`, the selected
    /// `config/environments/<env>.rb`), in load order
    /// (`ingest::boot_requires`). The emitted boot loads a file program
    /// at boot only when it is one of these.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub boot_loads: Vec<BootLoad>,
    /// The boot files' requires under a condition (`if`/`unless`), which
    /// the emitted boot does not evaluate: named, not loaded.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub boot_conditional_loads: Vec<BootLoad>,
    /// config/boot.rb's own top-level, unconditional `require "lit"` of a
    /// feature no file of the tree answers (a gem from the gem home:
    /// Core's `require "shopify/types/setup"`, which configures
    /// sorbet-runtime before any typed code loads), as `(feature, site)`
    /// in source order (`ingest::boot_requires::boot_native_requires`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub boot_native_requires: Vec<(String, String)>,
    /// The Gemfile gems the boot's `Bundler.require` requires, by
    /// environment (`ingest::boot_requires::bundler_required_gems`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bundler_gems: Vec<BundlerGem>,
    /// The directories of the gems `roundhouse.yml` names under
    /// `gem_sources:`, relative to the app root and each ending in `/`: installed gem trees compiled
    /// as app code, whose class bodies run as the gem's own Ruby does
    /// (`emit::ruby::library::declared_in_gem_source`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gem_source_dirs: Vec<String>,
    /// The test environment's `$LOAD_PATH` below the app root, each
    /// ending in `/`: Rails' `test`, and each `paths.add(…, load_path:
    /// true)` of the application and its engines, unconditional or in the
    /// test environment (Core's engines' `paths.add("test", load_path:
    /// true)`; `ingest::app::rails_test_load_path`). A test hook's or a
    /// test-support file's `require "support/helpers/…"` resolves there
    /// (`project::test_load_path_features`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub test_load_roots: Vec<String>,
    /// The run-time `$LOAD_PATH` below the app root in every environment,
    /// each ending in `/`: the application's and each engine's `lib`
    /// (Rails::Engine's `paths.add("lib", load_path: true)`, railties
    /// engine/configuration.rb; Rails::Application inherits it). A carried
    /// file below one answers a `require` of its feature
    /// (`project::lib_load_path_features`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lib_load_roots: Vec<String>,
    /// The carried file programs `test/test_helper.rb` requires, by
    /// source path, in the order it requires them: Core's
    /// `require "platform/testing/test_helper"` inside its load block,
    /// which loads the test runtime, its helpers and every test
    /// initializer. The emitted helper is the harness's own, so each
    /// test requires these after it, where the app's helper had run
    /// them (`ingest::app::test_helper_programs`). Each is `(source,
    /// site)`, the site the helper's `test/test_helper.rb:<line>`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub test_helper_programs: Vec<(String, String)>,
    /// Each support file the ingest carried => the library classes and
    /// modules it declares, in the order it declares them; what a
    /// `require` of that file loads in the emitted tree.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub library_class_files: Vec<(String, Vec<ClassId>)>,
    /// `config.eager_load = …` from each `config/environments/<env>.rb`,
    /// in file-name order. The emitted CRuby app evaluates the one for
    /// `Rails.env` at boot to choose eager loading or autoload on
    /// reference, as Rails' `eager_load!` finisher does.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub eager_load: Vec<EagerLoadSetting>,
    /// Zeitwerk loaders support files create (`Zeitwerk::Loader.for_gem`
    /// in an in-repo gem's entry file), compiled into the generated
    /// autoload instead of run (`ingest::zeitwerk`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub zeitwerk_loaders: Vec<ZeitwerkLoader>,
    /// SQL functions an initializer registers on every SQLite
    /// connection (`raw_connection.create_function("regexp", 2) do …`,
    /// `create_aggregate("stddev", 1) do step … finalize … end`).
    /// Lobsters' raw-SQL flag statistics call `stddev(…)`, so without
    /// them the query fails with "no such function". Each body is an
    /// ordinary method; installing them is per-lane runtime glue — see
    /// [`SqlFunction`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sql_functions: Vec<SqlFunction>,
    /// The app's `Rails::Application` subclass from
    /// `config/application.rb` (e.g. `Lobsters::Application`),
    /// reparented at ingest onto `Rails::Application` itself. Its
    /// instance methods are app config (`read_only?`, `name`, `domain`,
    /// `ssl?`) reached at runtime via `Rails.application.<m>` — the
    /// runtime shim memoizes `Rails::Application.new`, so emitting the
    /// class as a reopen makes them reachable regardless of require
    /// order (the app namespace is never referenced at runtime and
    /// drops out). None when the app has no config/application.rb or
    /// its class defines no methods.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rails_application: Option<LibraryClass>,
    /// Filters declared inside a concern module's `included do` block
    /// (`AccountOwnedConcern` → its `before_action :set_account, …`
    /// lines), keyed by the module. Rails runs these as if written in
    /// each including class; analyze extends every includer's filter
    /// chain from this map so concern-seeded ivars (`@account`) resolve
    /// in actions and views. Populated by the concern-module arms of
    /// the app walk; empty for apps without concerns.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub concern_filters: HashMap<ClassId, Vec<Filter>>,
    /// graphql-ruby object types (`ingest::graphql_ruby`), for the
    /// analyzer only; their synthesized methods leave at lowering.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub graphql_types: Vec<crate::dialect::GraphqlObjectType>,
    /// Signatures `ingest::graphql_ruby` declared in `rbs_signatures`
    /// (field arguments, input object readers), removed with the
    /// synthesized methods at lowering.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub graphql_signatures: Vec<(ClassId, Symbol)>,
    /// Provenance for concern METHODS spliced into a controller:
    /// controller → (method name → the module it was cut from).
    /// `splice_concerns_into_controllers` copies a concern's methods
    /// into every includer; the copy is then typed against THAT
    /// controller's ivar environment, which is wrong when the concern
    /// is included high in the chain. campfire puts
    /// `include TrackedRoomVisit` on ApplicationController, whose
    /// `remember_last_room_visited` reads `@room` — a ivar
    /// ApplicationController never sets, because the concern's method
    /// runs as a before_action on the Rooms controllers below it. The
    /// honest seed is the union across includers, which
    /// `concern_ivar_env` already computes; this map is what lets the
    /// seeding site find it. Filters carry the same fact in
    /// `Filter::from_concern`; methods had nowhere to put it.
    ///
    /// Analysis-only, so it lives beside the app rather than on
    /// `Action`: no emitter needs it (the splice is already a verbatim
    /// copy by the time one runs), and a field on `Action` would touch
    /// every construction site including a dozen in tests.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub concern_spliced_actions: HashMap<ClassId, HashMap<Symbol, ClassId>>,
    /// The class-side twin of `concern_spliced_actions`: concern methods
    /// `splice_concern_class_methods_into_includers` copied onto a model
    /// or library class, keyed includer → method → the source module.
    ///
    /// The module keeps its own copy of that `def` (dead there, since
    /// `include` never carries a singleton method), so ONE method now
    /// has two `MethodDef`s in two classes. Call sites are recorded
    /// against the receiver — `User.create_bot!` — and without this map
    /// nothing connects them to the module's copy, which is the one the
    /// sidecar beside `app/models/user/bot.rb` is written from. The two
    /// descriptions then disagree about the parameter types.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub concern_spliced_class_methods: HashMap<ClassId, HashMap<Symbol, ClassId>>,
    /// The subset of `concern_spliced_class_methods` on a LIBRARY class
    /// that came from a concern's nested `module ClassMethods`, keyed
    /// includer → one entry per carrier module.
    ///
    /// In Ruby those methods reach the includer through `extend`, never a
    /// copy: the authored `def self.included(base) base.extend(ClassMethods)`
    /// hook, or ActiveSupport::Concern's `append_features`. The copies stay
    /// in the IR for analysis and for the targets without mixins; the Ruby
    /// emit writes the includer without them, and with the `extend` that
    /// only Concern would have run (its `append_features` is not emitted).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub class_methods_carrier_splices: HashMap<ClassId, Vec<ClassMethodsCarrierSplice>>,
    /// An ActiveSupport::Concern's `class_methods do` defs, by name span,
    /// keyed by the Concern. The IR keeps them as the Concern's class
    /// methods (the splice copies them onto includers); in Ruby they are
    /// the instance methods of the `Owner::ClassMethods` the block
    /// defines, which the Ruby emit writes nested in the Concern, so an
    /// includer's `extend` (`class_methods_carrier_splices`) and an
    /// authored `extend Owner::ClassMethods` both find it.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub class_methods_blocks: HashMap<ClassId, Vec<crate::span::Span>>,
    /// The Ruby target's forwarders for a library class's instance-side
    /// `delegate :m, to: :t`: `def m(...) = t.m(...)`, as Rails generates.
    /// `ingest::delegate` writes the IR forwarder zero-arg (argument
    /// forwarding is the shape the strict targets do not lower), and
    /// that stays the method analysis and the other targets see. The
    /// Ruby emit writes this one in its place, so a caller in another
    /// file passing arguments (asset_cloud's `add_error(...)` through
    /// `delegate :add_error, to: :asset`) reaches the target.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub ruby_delegate_forwarders: HashMap<ClassId, Vec<crate::dialect::MethodDef>>,
    /// The `(...)` forwarders of the instance-side `delegate`
    /// declarations `ingest::delegate` declines for the strict targets:
    /// the class calls the name with arguments, or a zero-arg forwarder
    /// would mis-arity an admitted `def m(a, *)`. The declaration stays
    /// in `unknown_calls` and the IR has no method; the Ruby emit adds
    /// these, as Rails defines them. A `delegate_missing_to`'s
    /// `respond_to_missing?` and `method_missing` are here too, each
    /// placed at the call.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub ruby_declined_delegate_forwarders: HashMap<ClassId, Vec<crate::dialect::MethodDef>>,
    /// The instance methods `ingest::class_eval_macro` added to a library
    /// class for a class-body call to a macro that writes them with a
    /// `class_eval` heredoc (Core's ComplexStruct `attribute :value,
    /// String`), by the call. The Ruby emit replays the call, which runs
    /// the macro, and writes the class without these copies.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub class_eval_macro_expansions: HashMap<ClassId, Vec<ClassEvalExpansion>>,
    /// Model DSL declared inside a concern module's `included do`
    /// (`Account::Associations` → its `has_many :statuses` etc.),
    /// keyed by the module and classified as the same
    /// [`ModelBodyItem`]s a model body carries. Rails evaluates the
    /// block in each including model; analyze registers these items
    /// on every includer (associations as typed readers/writers,
    /// scopes as relation-returning class methods) so dispatch and
    /// completion see the mixed-in surface. Registry-level only for
    /// now — the items are deliberately NOT spliced into `Model.body`,
    /// keeping source round-trip exact; a transpile-grade splice (with
    /// item provenance) can follow when emission needs it.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub concern_model_items: HashMap<ClassId, Vec<ModelBodyItem>>,
    /// A Concern's `prepended do` DSL, keyed like `concern_model_items`:
    /// spliced after a model's `prepend <Concern>` line, as Rails
    /// evaluates the block in a prepender's body.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub concern_prepended_items: HashMap<ClassId, Vec<ModelBodyItem>>,
    /// Classes that were `ActiveSupport::CurrentAttributes` subclasses
    /// before `ingest::current_attributes` flattened them. Recorded
    /// because that pass CLEARS the parent — nothing downstream could
    /// recognize them afterwards — and the dispatch scaffold needs the
    /// names to emit a per-request `reset`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub current_attribute_classes: Vec<ClassId>,
    /// Concerns whose `included do` block every static includer gets
    /// from ingest: a model (`splice_concerns_into_models`) or a plain
    /// class or module (`concern_accessors::splice_into_library_includers`),
    /// each of which reports what it could not carry. The module's own
    /// emitted body drops the call without a warning
    /// (`concern_accessors::carried_included_blocks`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub carried_included_blocks: Vec<ClassId>,
    /// The library classes and modules the source declared inside a
    /// `class << self` (Core's `ElasticSearch::TaxCodeIndexing`'s
    /// `TaxCodeSourceBuilder`, which its class methods read bare). Ruby
    /// puts them on the singleton class: `X.singleton_class::Helper`, not
    /// `X::Helper`. Each is named `X::Helper` here, as analysis and the
    /// other targets hoist it; the Ruby family writes it back inside
    /// `class << self` and reads it there (`emit::ruby::singleton_scoped`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub singleton_scoped_classes: Vec<ClassId>,
    /// Where the source declares each library class that several
    /// declarations open, at least one of them without a member of its
    /// own: the span of each `class X` / `module X` statement. Merged
    /// into one class (`ingest::app::merge_library_class_reopens`), such
    /// a class's members all sit at a later declaration, while Ruby
    /// defines the constant at the first: Core's component/schema.rb
    /// forward-declares `class Validator; end` above the
    /// `class_attribute … default: Validator` that reads it, and
    /// writes its body at the bottom. The Ruby family writes a nested
    /// class at its earliest declaration (`emit::ruby::library`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub library_class_declarations: Vec<(ClassId, crate::span::Span)>,
    /// The `included do` statements a plain includer runs where its
    /// `include` runs, keyed by the includer, each with the direct include
    /// it runs after (the root its Concern hangs off), in the order Rails
    /// runs them: a job's ActiveJob::Callbacks registrations
    /// (`before_perform`, `around_enqueue`, `rescue_from`, ...;
    /// `concern_accessors::splice_into_library_includers`). The Ruby
    /// emitter writes each after its `include` line, where it registers in
    /// the same order against the include hooks and the class's own body.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub included_block_statements: HashMap<ClassId, Vec<(ClassId, crate::expr::Expr)>>,
    /// The `class_attribute`s a Concern's `included do` gives a plain
    /// includer, keyed by the includer, each group with the direct include
    /// it runs after, as `included_block_statements` has it
    /// (`concern_accessors::splice_into_library_includers`). The generated
    /// defs stay in `LibraryClass::methods` and the seeds in
    /// `class_ivar_initializers`, where every pass reads them; this names
    /// them, so the Ruby emitter writes them right after that `include`,
    /// where Ruby runs the block. A class-body macro below the include
    /// then writes over the seeded default, as Core's `retries 3` writes
    /// BackgroundQueue::Retry's `retry_limit`.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub included_class_attributes: HashMap<ClassId, Vec<IncludedClassAttributes>>,
    /// Renderer view → the partial views it renders (`articles/show` →
    /// [`articles/_form`]), harvested from actual render sites as views
    /// are analyzed. The other half of the render graph that
    /// `view_feeders` closes over — persisted for related-file
    /// navigation (view ↔ its partials, partial ↔ its renderers).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub render_edges: HashMap<Symbol, Vec<Symbol>>,
    /// View name (`articles/show`, `articles/_form`, `layouts/application`)
    /// → controllers whose actions feed that view, recorded by analyze
    /// while it harvests the action→view ivar channel (explicit `render`
    /// targets and the implicit action-name convention), the effective-
    /// layout resolution, and the renderer→partial edges (a partial's
    /// feeders are the transitive union of its renderers'). This is the
    /// same linkage the ivar seeding used — persisted so consumers can
    /// trace a view-side symptom back to the controller responsible:
    /// diagnostic gap-attribution (an unresolved `@ivar` in a view whose
    /// feeder had an ingest gap is a coverage note, not a user error)
    /// and, later, controller↔view navigation. Sorted for determinism.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub view_feeders: HashMap<Symbol, Vec<ClassId>>,
    /// Source files read during ingest, indexed by `Span.file`
    /// (`FileId(n)` → `sources[n - 1]`; `FileId(0)` is the synthetic
    /// sentinel). Carries the parsed text so diagnostics can resolve
    /// byte-offset spans to file:line:col without re-reading disk —
    /// which the wasm ingest path couldn't do anyway. Empty for Apps
    /// built by hand in tests.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<crate::span::SourceFile>,
    /// Rails' ParamsWrapper default for every controller: wrap a JSON
    /// body under the controller's model name. On when
    /// `config.load_defaults` is 7.0 or later (Rails sets
    /// `action_controller.wrap_parameters_by_default`), unless that key
    /// is set explicitly, or when an initializer calls
    /// `wrap_parameters format: [:json]` (the pre-7.0 generator).
    /// Read by `lower::controller_to_library::params_wrapper`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub wrap_parameters_by_default: bool,
    /// Rubydex answers for `sources`, resolved while ingest finished.
    /// The analyzer resolves the sources itself when this is absent.
    #[serde(skip)]
    pub const_resolver: crate::analyze::PreparedConstResolver,
    /// Source files whose class ingest failed under survey mode, with
    /// the gap that failed each, in record order: the classes, modules
    /// and constants that ingest would have registered from the file
    /// are not in the IR. Gap attribution names the gap behind a
    /// reference to one of them. Empty outside survey mode.
    #[serde(skip)]
    pub dropped_files: Vec<crate::ingest::survey::DroppedFile>,
    /// Every readable `X = T.type_alias { T }` in a class body, by
    /// owner, whether or not the Ruby trees carry it
    /// (`ingest::sorbet_check::settle_type_aliases`). The analysis reads
    /// a declared type through these.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sorbet_type_aliases: Vec<(ClassId, Symbol, crate::expr::Expr)>,
    /// Ingested source apps cannot enter the standalone IR-only analyzer mode
    /// if a caller drops their source table. Persisted across serialization.
    #[serde(default)]
    pub source_index_required: bool,
    /// Per-controller resolved request machinery, computed once by
    /// analyze's parent-chain walk and persisted (the self-describing-IR
    /// move: `run_typing_passes` already built these to seed ivars, and
    /// used to discard them). Keyed by controller class. Consumers —
    /// `ide::traceroute`, the MCP tool, gap attribution — compose over
    /// this instead of re-deriving inheritance + concern splicing.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub controller_resolutions: HashMap<ClassId, ControllerResolution>,
    /// App directory as passed to ingest (`fixtures/real-blog`), `""`
    /// for in-memory trees (map VFS, wasm). `sources` paths keep this
    /// prefix so diagnostics print compiler-cwd-relative (clickable)
    /// locations; consumers that need app-relative paths (source-map
    /// `sources` entries must not differ by ingest mode) strip it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub root: String,
    /// App-layer roots ingest walked, relative to `root`: `["app"]` for
    /// an ordinary Rails app, `["app", "packs/blog/app", …]` for a
    /// Packwerk app whose packages carry their own `app/` tree,
    /// `["app", "lib/billing/app"]` for one with an in-repo engine
    /// (`ingest::app::app_roots`). `app` is always first; the rest are
    /// sorted. Exists so a consumer (today, `check`'s summary line) can
    /// report what got walked without recomputing it from the VFS.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub app_roots: Vec<String>,
}

/// One controller's resolved request machinery: the full filter chain
/// as Rails would execute it (inheritance + concern splicing applied)
/// and the effective layout. Per-controller, not per-action — the
/// chain keeps each filter's `only:`/`except:` gating and any `skip_*`
/// entries, so the per-action view is a cheap filter over this record
/// (apply the gates, drop targets named by an applicable Skip) rather
/// than a duplicated copy per action.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct ControllerResolution {
    /// Filters in Rails execution order: ancestors' first (oldest
    /// ancestor's declarations first), then this controller's own body
    /// order with concern-contributed filters spliced at their
    /// `include` site. Includes `After` and `Skip` kinds — consumers
    /// pick the subset they care about.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub filter_chain: Vec<ResolvedFilter>,
    /// Effective layout view name (`layouts/application`), resolved by
    /// walking the inheritance chain; `None` records an explicit
    /// `layout false`. Convention default applies, so this may name a
    /// layout view the app doesn't ship — Rails would render bare.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<Symbol>,
}

/// One hop of a resolved filter chain: the declaration plus the
/// provenance and typed consequences analyze already knew when it
/// seeded ivars through this filter.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResolvedFilter {
    /// The declaration as written (kind, target method, `only:`/
    /// `except:` gating, symbol-form `if:`/`unless:` guards).
    pub filter: crate::dialect::Filter,
    /// Class or concern module whose body declared this filter — the
    /// trace hop's "defined in AccountOwnedConcern", distinct from the
    /// controller whose chain it landed in.
    pub defined_in: ClassId,
    /// Chain segment that carried the filter in: the controller (or
    /// ancestor) whose class body — an `include` line for concern
    /// filters, the declaration itself otherwise — put this entry in
    /// the chain. Equals `defined_in` for directly-declared filters;
    /// for concern filters it's the includer (`set_locale`: defined_in
    /// `Localized`, included_via `ApplicationController`). Lets
    /// consumers group contiguous runs under two-level headers without
    /// reconstructing segment boundaries from order.
    pub included_via: ClassId,
    /// Ivars the target method's body assigns, with inferred types
    /// (`@account` → `Account`). Empty for `Skip` entries and for
    /// targets analyze couldn't see (e.g. framework-defined).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub assigns: HashMap<Symbol, Ty>,
    /// The target method's effect set (`DbRead`…), so a trace doubles
    /// as a static query profile without re-finding the method body.
    #[serde(default, skip_serializing_if = "crate::effect::EffectSet::is_pure")]
    pub effects: crate::effect::EffectSet,
}

/// What an app registered a `to_fs` format AS. Rails accepts both,
/// and `Time#to_fs` picks between them by asking the value whether it
/// responds to `call`:
///
/// ```ruby
/// formatter.respond_to?(:call) ? formatter.call(self).to_s : strftime(formatter)
/// ```
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum TimeFormat {
    /// A strftime string — `register(:month_and_year, "%B %Y")`.
    Strftime { format: String },
    /// A `->(t) { … }`, as a one-parameter method whose body is the
    /// lambda's. Inlined with the receiver substituted for the
    /// parameter, then `.to_s`, which is what Rails applies to the
    /// call's result above.
    Lambda { method: MethodDef },
}

/// One class-body macro call `ingest::class_eval_macro` expanded.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClassEvalExpansion {
    /// The call's span, as it stands in `LibraryClass::unknown_calls`.
    pub call: crate::span::Span,
    /// The instance methods its `class_eval` heredoc defines.
    pub methods: Vec<Symbol>,
    /// The call is `base.<macro>(…)` in the `self.included(base)` hook of
    /// a module the class includes, so on Ruby the include itself runs it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hook: bool,
}

/// One concern's nested `module ClassMethods`, as an includer received it
/// (see `App::class_methods_carrier_splices`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClassMethodsCarrierSplice {
    /// The carrier module (`Owner::ClassMethods`).
    pub carrier: ClassId,
    /// Class-side copies of its defs on the includer: typing only in Ruby.
    pub methods: Vec<Symbol>,
    /// Whether the includer itself must `extend` the carrier in Ruby:
    /// true when only ActiveSupport::Concern's `append_features` (reached
    /// through a chain of Concerns) extends it; false when the authored
    /// `included` hook does, or nothing does.
    pub extend: bool,
}

/// `Owner` for a module named `Owner::ClassMethods` — the nested carrier
/// ActiveSupport::Concern (and the vanilla `base.extend(ClassMethods)`
/// hook) extends an includer with.
pub fn class_methods_carrier_owner(name: &crate::ident::ClassId) -> Option<crate::ident::ClassId> {
    name.0.as_str()
        .strip_suffix("::ClassMethods")
        .filter(|owner| !owner.is_empty())
        .map(|owner| crate::ident::ClassId(Symbol::from(owner)))
}

/// One `X.prepend Y` / `X.include Y` an initializer registers.
///
/// The receiver is stored as WRITTEN (`Turbo::StreamsChannel`), not
/// resolved: a mixin onto a constant this tree does not define is a gap
/// to report, and reporting it needs the app's own spelling.
/// One SQL function an initializer registers with SQLite.
///
/// The block bodies become methods of the `SqlFunctions` module, each
/// taking the SQLite context as its first parameter under the block's
/// own name (`fn`), so a body reads exactly as the app wrote it:
/// `fn.result = …` sets the value, and an aggregate keeps its running
/// state in `fn[:key]`. A `next` that left the block early is a
/// `return` in the method. The context object is the lane's: the
/// sqlite3 gem's function proxy on CRuby.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SqlFunction {
    /// The SQL-visible name (`"stddev"`).
    pub name: String,
    /// Argument count SQLite dispatches on.
    pub arity: usize,
    pub kind: SqlFunctionKind,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum SqlFunctionKind {
    /// `create_function` — one method, called per row.
    Scalar { method: crate::dialect::MethodDef },
    /// `create_aggregate` — `step` per row, `finalize` once.
    Aggregate { step: crate::dialect::MethodDef, finalize: crate::dialect::MethodDef },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModuleMixin {
    /// The constant receiving the module, as the initializer spells it.
    pub target: Symbol,
    /// The module being mixed in.
    pub module: Symbol,
    pub kind: MixinKind,
}

/// `X.before_action :m, only: [:a, …]` from a `config/initializers/`
/// file. `only` empty means every action, as Rails reads it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InitializerFilter {
    /// The controller receiving the filter, as the initializer spells it.
    pub target: Symbol,
    /// The filter method, which a mixin onto the same target supplies.
    pub method: Symbol,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub only: Vec<Symbol>,
}

/// One initializer statement (a whole top-level statement, or one
/// statement of a container a reader partly consumed) that nothing in
/// ingest applies to the emitted app.
/// One environment's `config.eager_load` (`config/environments/<env>.rb`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EagerLoadSetting {
    /// The environment, from the file name (`test`).
    pub env: String,
    /// The assigned expression, or why it cannot be read as one
    /// (assigned under a condition, reads a local of the file).
    pub value: Result<crate::expr::Expr, String>,
}

/// A Zeitwerk loader a support file creates, read statically
/// (`ingest::zeitwerk`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ZeitwerkLoader {
    /// The constant holding it (`Functions::ZeitwerkLoader`).
    pub constant: String,
    /// The directory it manages.
    pub root: String,
    /// Classes defined by the files it autoloads and eager-loads.
    pub managed: Vec<crate::ident::ClassId>,
    /// Classes defined by files under its root that it leaves alone (an
    /// ignored path, or every file when it is never set up): carried,
    /// but neither autoloaded nor eager-loaded.
    pub ignored: Vec<crate::ident::ClassId>,
    /// The directories its `eager_load_dir` names, in call order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub eager_dirs: Vec<ZeitwerkEagerDir>,
    /// The directories it collapses (`collapse`, glob patterns expanded),
    /// absolute like `root`: each adds no namespace, so a file under one
    /// is named by its path without that directory.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub collapsed: Vec<String>,
}

/// A directory a Zeitwerk loader's `eager_load_dir` loads
/// (`ingest::zeitwerk`): what the handle requires when the directory's
/// namespace is defined.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ZeitwerkEagerDir {
    /// Relative to the loader's root (`shopify/patches`), the key the
    /// call site names.
    pub dir: String,
    /// The managed files under it, as ingested, sorted.
    pub sources: Vec<String>,
    /// The classes those files define.
    pub classes: Vec<crate::ident::ClassId>,
}

/// A railtie or engine class the app carries ([`App::railties`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CarriedRailtie {
    pub name: crate::ident::ClassId,
    /// An engine's root relative to the app root: the directory of the
    /// in-repo gem whose `lib/` defines it, where Rails' `find_root`
    /// lands. `None` for a railtie, which has no root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
}

/// A file native boot requires ([`App::boot_loads`]).
/// A Gemfile gem the boot's `Bundler.require` requires
/// (`ingest::boot_requires::bundler_required_gems`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BundlerGem {
    /// The lockfile/Gemfile gem name.
    pub name: String,
    /// An explicit `require:` path (None: Bundler's default).
    pub require: Option<String>,
    /// The environments its groups limit it to (empty: every one).
    pub envs: Vec<String>,
    /// The Gemfile line (`Gemfile:665`).
    pub site: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BootLoad {
    /// The required file, relative to the app root.
    pub source: String,
    /// The environments it loads in (empty: every one): an environment
    /// file's require, or a Gemfile group's gem.
    pub envs: Vec<String>,
    /// Where the boot requires it (`config/boot.rb:31`).
    pub site: String,
}

/// A support file's file-level program ([`App::file_programs`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FileProgram {
    /// The source file, as ingested.
    pub source: String,
    pub items: Vec<ProgramItem>,
}

/// One top-level statement of a [`FileProgram`], in source order.
/// A part of a `class_eval` body ([`ProgramItem::ClassEval`]), in
/// source order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ClassEvalPart {
    /// Body statements, as the methods and constants of a module named
    /// `ingest::library_class::CLASS_EVAL_BODY`.
    Body(crate::dialect::LibraryClass),
    /// An `if`/`unless` in the body that defines methods.
    Branch { cond: Expr, negate: bool, then_parts: Vec<ClassEvalPart>, else_parts: Vec<ClassEvalPart> },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ProgramItem {
    /// A `class`/`module` the walk carried as library classes (these
    /// names), loaded here so the statements around it run in order.
    Declared(Vec<ClassId>),
    /// A statement Ruby runs as the file loads.
    Statement(Expr),
    /// A file-level `return` (or `return unless …`): the rest of the
    /// file runs only when it does not return.
    Guard(Expr),
    /// `require "feature"` / `require_relative "path"`, resolved against
    /// the tree when emitted.
    Require { span: crate::span::Span, feature: String, relative: bool },
    /// Declarations carried only here, as Ruby evaluates them: inside a
    /// file-level conditional, or behind a file-level `return`.
    Inline(Vec<crate::dialect::LibraryClass>),
    /// `receiver.method do … end` (`class_eval` and kin), its body as
    /// the methods and constants of a module named
    /// `ingest::library_class::CLASS_EVAL_BODY` (`None`: an empty body).
    ClassEval { receiver: Expr, method: String, parts: Vec<ClassEvalPart> },
    /// A file-level `if`/`unless` holding declarations.
    Branch {
        cond: Expr,
        negate: bool,
        then_items: Vec<ProgramItem>,
        else_items: Vec<ProgramItem>,
    },
    /// A statement the ingest cannot carry, named when the file loads
    /// and reported as `file_statement_unsupported`. `construct` is the
    /// groupable name (`Money::Converters.register`, `if`, `begin`).
    NotCarried { span: crate::span::Span, construct: String, message: String },
}

/// A source file a failed library-class ingest left out of the IR.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct IngestDrop {
    /// The construct that failed the file, or the file's first byte
    /// when no frame noted one.
    pub span: crate::span::Span,
    /// What the file's ingest refused.
    pub message: String,
    /// Set when the file was ingested and only the statement at `span`
    /// is not carried as Ruby runs it: the `unsupported` construct it
    /// is reported as, rather than a whole
    /// file left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub construct: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InitializerGap {
    /// The statement's source range.
    pub span: crate::span::Span,
    /// Short, stable, groupable name: `ActiveSupport.on_load`,
    /// `require`, `Rails.application.config.filter_parameters+=`, `if`.
    pub construct: String,
    /// The statement's first line, trimmed and capped.
    pub detail: String,
}

/// The top-level constant writes of one boot initializer, carried as the
/// `LibraryClassOrigin::FileConstants` holder `holder`
/// (`ingest::initializer_constants`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InitializerConstants {
    /// The initializer that assigns them, relative to the app root.
    pub file: String,
    /// The holder in `App::library_classes`, named for the first one.
    pub holder: crate::ident::ClassId,
    /// The constants, in source order.
    pub names: Vec<crate::ident::Symbol>,
}

/// The statements of one boot initializer the emitted boot runs, as the
/// body of `holder.run_initializer` (`ingest::initializer_programs`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InitializerProgram {
    /// The initializer, relative to the app root.
    pub file: String,
    /// The holder module in `App::library_classes`.
    pub holder: crate::ident::ClassId,
    /// Statements carried (top level, and inside a deferred block).
    pub statements: usize,
}

/// One `config/environments/<env>.rb`, as the emitted boot runs it when
/// `Rails.env` is `env` (`ingest::environments`): the carried statements
/// are the body of `holder.run_environment`; each statement not carried
/// is named, and boot names it again when it selects this environment.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EnvironmentProgram {
    /// The environment, from the file name (`test`).
    pub env: String,
    /// The file, relative to the app root.
    pub file: String,
    /// The file as ingest registered it (`App::sources`).
    pub source: String,
    /// The holder module in `App::library_classes`; `None` when nothing
    /// of the file is carried.
    pub holder: Option<crate::ident::ClassId>,
    /// Statements carried (top level, the `configure` block's, and inside
    /// a deferred block).
    pub carried: usize,
    /// `config.*` writes applied by being inert, or read elsewhere
    /// (`config.eager_load`).
    pub consumed: usize,
    /// `<file>:<line> <statement>` and why it is not carried.
    pub refused: Vec<(String, String)>,
}

/// A framework load hook an initializer registers for, and the runtime
/// class Rails fires it with.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FrameworkLoadHook {
    pub hook: String,
    /// The base (`ActiveRecord::Base`).
    pub base: String,
    /// Where Rails fires it (`activerecord/lib/active_record/base.rb:336`).
    pub site: String,
}

/// An app's own `ActiveSupport.run_load_hooks` at class-body or file
/// level.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClassLoadHook {
    /// The hook name, as written (a Symbol, or a String).
    pub hook: String,
    /// The hook literal as Ruby (`:shop`, `"x"`).
    pub hook_literal: String,
    /// The base expression as emitted (`Shop`, `Object`).
    pub base: String,
    /// The class or module whose emitted file the call ends.
    pub class: String,
    /// `file:line` of the call, relative to the app root.
    pub site: String,
    /// The call.
    pub span: crate::span::Span,
}

/// A main-autoloader `on_load(cpath)` callback whose includes were
/// appended to the model `cpath` names.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LiftedAutoloadCallback {
    /// The constant path the callback names (`ApplicationRecord`).
    pub cpath: String,
    /// The initializer it came from, as the ingest read it.
    pub file: String,
    /// The statement's start offset in `file`.
    pub start: u32,
}

/// One `ActiveSupport.on_load(:hook)` block of a boot initializer, lifted
/// to `Rails.application.<method>(base)`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LoadHook {
    /// The hook name (`active_record_postgresqladapter`).
    pub hook: String,
    /// The method on the Application reopen holding the block's body.
    pub method: String,
    /// The initializer it came from.
    pub file: String,
}

/// `prepend` inserts AHEAD of the target in the lookup chain, `include`
/// behind it. The distinction is the whole point of campfire's
/// `RoomStreamsAreAuthorized`, whose `subscribed` calls `super` — as an
/// `include` it would never run, because the class defines its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MixinKind {
    Prepend,
    Include,
}

impl MixinKind {
    pub fn as_str(self) -> &'static str {
        match self {
            MixinKind::Prepend => "prepend",
            MixinKind::Include => "include",
        }
    }
}

/// A Rails-style importmap: one `<name>` → `<path>` entry per
/// pin, in declaration order (Rails preserves order for
/// modulepreload link emission).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct Importmap {
    pub pins: Vec<ImportmapPin>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ImportmapPin {
    /// Module specifier the page imports (`"application"`,
    /// `"@hotwired/turbo-rails"`, `"controllers/hello_controller"`).
    pub name: String,
    /// Served asset path (`/assets/application.js`,
    /// `/assets/turbo.min.js`, …). Canonical (no fingerprint);
    /// real deployments sprinkle digests in here.
    pub path: String,
}

impl App {
    /// Whether the CRuby tree carries the PostgreSQL runtime: boot
    /// resolves config/database.yml and the primary is postgresql
    /// (project.rs `apply_postgres_bootstrap`, which also carries the
    /// test-tree ports in `runtime_files::POSTGRESQL_TEST_PORTS`).
    pub fn carries_postgresql_runtime(&self) -> bool {
        self.database_yml.is_some() && self.primary_database_adapter() == Some(&crate::database::DatabaseAdapter::Postgresql)
    }

    /// The adapter the primary database is compiled for: the manifest's
    /// primary, else `config/database.yml`'s declared compile target;
    /// `None` keeps the single-database (sqlite3) selection.
    pub fn primary_database_adapter(&self) -> Option<&crate::database::DatabaseAdapter> {
        self.databases
            .iter()
            .find(|db| db.primary && !db.replica)
            .or_else(|| self.databases.iter().find(|db| db.primary))
            .map(|db| &db.adapter)
            .or_else(|| self.database_yml.as_ref().map(|yml| &yml.adapter))
    }

    /// Module → the one non-module class that includes it, directly or
    /// through another module; absent when none or several do. A
    /// concern's methods run on its includer, and with exactly one
    /// includer that class is what `self` means in them — the analyzer
    /// types the bodies against it and `lower::class_body_new` binds a
    /// bare `new` to it. Read from the same `include` lines both the
    /// registry and the emit are built from: a model's `include X`
    /// body items, a library class's `includes`.
    pub fn sole_includer_of_modules(
        &self,
    ) -> crate::det::HashMap<crate::ident::ClassId, crate::ident::ClassId> {
        self.includers_of_modules()
            .into_iter()
            .filter_map(|(m, set)| (set.len() == 1).then(|| (m, set.into_iter().next().unwrap())))
            .collect()
    }

    /// Module → every non-module class that includes it, directly or
    /// through another module (`sole_includer_of_modules` keeps the
    /// modules with exactly one).
    pub fn includers_of_modules(
        &self,
    ) -> crate::det::HashMap<crate::ident::ClassId, std::collections::BTreeSet<crate::ident::ClassId>> {
        use std::collections::{BTreeMap, BTreeSet};
        use crate::det::HashMap;
        let modules: BTreeSet<&crate::ident::ClassId> = self
            .library_classes
            .iter()
            .filter(|lc| lc.is_module)
            .map(|lc| &lc.name)
            .collect();
        if modules.is_empty() {
            return HashMap::default();
        }
        // Every class's DIRECT includes, modules included (for the
        // transitive walk).
        let mut direct: BTreeMap<crate::ident::ClassId, Vec<crate::ident::ClassId>> = BTreeMap::new();
        for lc in &self.library_classes {
            direct.insert(lc.name.clone(), lc.includes.clone());
        }
        for model in &self.models {
            let mut ids = Vec::new();
            for item in &model.body {
                if let crate::dialect::ModelBodyItem::Unknown { expr, .. } = item {
                    if let crate::expr::ExprNode::Send { recv: None, method, args, block: None, .. } = &*expr.node {
                        if method.as_str() == "include" {
                            for arg in args {
                                if let crate::expr::ExprNode::Const { path } = &*arg.node {
                                    ids.push(crate::analyze::mixin_path_id(path));
                                }
                            }
                        }
                    }
                }
            }
            direct.entry(model.name.clone()).or_default().extend(ids);
        }
        // Controllers include concerns too, and a module shared between
        // a controller and a channel (campfire's
        // `Authentication::SessionLookup`) has TWO includers, not the
        // one the library classes alone would show.
        for controller in &self.controllers {
            let ids = crate::analyze::controller_includes(controller);
            direct.entry(controller.name.clone()).or_default().extend(ids);
        }
        let mut includers: HashMap<crate::ident::ClassId, BTreeSet<crate::ident::ClassId>> = HashMap::default();
        for (id, includes) in &direct {
            if modules.contains(id) || includes.is_empty() {
                continue;
            }
            let mut queue = includes.clone();
            let mut seen: BTreeSet<crate::ident::ClassId> = queue.iter().cloned().collect();
            let mut qi = 0;
            while qi < queue.len() {
                let m = queue[qi].clone();
                qi += 1;
                if modules.contains(&m) {
                    includers.entry(m.clone()).or_default().insert(id.clone());
                }
                if let Some(nested) = direct.get(&m) {
                    for n in nested {
                        if seen.insert(n.clone()) {
                            queue.push(n.clone());
                        }
                    }
                }
            }
        }
        includers
    }
    pub const SCHEMA_VERSION: u32 = 1;

    pub fn new() -> Self {
        Self {
            schema_version: Self::SCHEMA_VERSION,
            schema: Schema::default(),
            databases: Vec::new(),
            model_databases: indexmap::IndexMap::new(),
            database_refusals: Vec::new(),
            database_yml: None,
            binary_assets: Vec::new(),
            locale_files: Vec::new(),
            models: Vec::new(),
            library_classes: Vec::new(),
            controllers: Vec::new(),
            routes: RouteTable::default(),
            views: Vec::new(),
            test_modules: Vec::new(),
            fixtures: Vec::new(),
            seeds: None,
            importmap: None,
            stylesheets: Vec::new(),
            rbs_signatures: HashMap::default(),
            rbs_includes: HashMap::default(),
            rbs_singleton_methods: HashMap::default(),
            gem_lock: None,
            gem_boundary: Default::default(),
            content_helper_allowed_attributes: Vec::new(),
            test_helper_connections: Vec::new(),
            test_helper_connection_blockers: Vec::new(),
            inferred_method_params: HashMap::default(),
            helper_method_index: HashMap::default(),
            helper_singleton_methods: BTreeMap::new(),
            view_visible_controller_methods: BTreeSet::new(),
            generated_helper_methods: BTreeMap::new(),
            global_id_locate_models: BTreeSet::new(),
            global_id_locate_signed_models: BTreeSet::new(),
            attachable_unsigned_models: Vec::new(),
            pending_attachment_on_load: Vec::new(),
            load_hook_class_macros: Vec::new(),
            framework_base_includes: Vec::new(),
            load_hook_includes: Vec::new(),
            partial_local_types: HashMap::default(),
            view_ivar_types: HashMap::default(),
            html_safe_methods: BTreeSet::new(),
            time_formats: BTreeMap::new(),
            module_mixins: Vec::new(),
            initializer_filters: Vec::new(),
            initializer_files: Vec::new(),
            initializer_gaps: Vec::new(),
            initializer_constants: Vec::new(),
            initializer_programs: Vec::new(),
            environment_programs: Vec::new(),
            framework_load_hooks: Vec::new(),
            class_load_hooks: Vec::new(),
            load_hook_bases: std::collections::BTreeMap::new(),
            file_statement_gaps: Vec::new(),
            library_class_reopens: HashMap::default(),
            load_hooks: Vec::new(),
            lifted_autoload_callbacks: Vec::new(),
            ingest_drops: Vec::new(),
            railties: Vec::new(),
            file_programs: Vec::new(),
            boot_loads: Vec::new(),
            boot_conditional_loads: Vec::new(),
            boot_native_requires: Vec::new(),
            bundler_gems: Vec::new(),
            gem_source_dirs: Vec::new(),
            test_load_roots: Vec::new(),
            lib_load_roots: Vec::new(),
            test_helper_programs: Vec::new(),
            library_class_files: Vec::new(),
            eager_load: Vec::new(),
            zeitwerk_loaders: Vec::new(),
            sql_functions: Vec::new(),
            rails_application: None,
            concern_filters: HashMap::default(),
            graphql_types: Vec::new(),
            graphql_signatures: Vec::new(),
            concern_spliced_actions: HashMap::default(),
            concern_spliced_class_methods: HashMap::default(),
            class_methods_carrier_splices: HashMap::default(),
            class_methods_blocks: HashMap::default(),
            ruby_delegate_forwarders: HashMap::default(),
            ruby_declined_delegate_forwarders: HashMap::default(),
            class_eval_macro_expansions: HashMap::default(),
            concern_model_items: HashMap::default(),
            concern_prepended_items: HashMap::default(),
            current_attribute_classes: Vec::new(),
            carried_included_blocks: Vec::new(),
            singleton_scoped_classes: Vec::new(),
            library_class_declarations: Vec::new(),
            included_block_statements: HashMap::default(),
            included_class_attributes: HashMap::default(),
            render_edges: HashMap::default(),
            view_feeders: HashMap::default(),
            controller_resolutions: HashMap::default(),
            sources: Vec::new(),
            wrap_parameters_by_default: false,
            const_resolver: Default::default(),
            dropped_files: Vec::new(),
            sorbet_type_aliases: Vec::new(),
            source_index_required: false,
            root: String::new(),
            app_roots: Vec::new(),
        }
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

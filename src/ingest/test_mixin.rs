//! Shared test modules: a module in a test file whose body declares tests,
//! and the test classes that include it.
//!
//! Core writes a suite once and runs it in several classes:
//!
//! ```ruby
//! module CredentialSharedTests
//!   include SharedTests            # ActiveSupport::Concern + Declarative + TestTags
//!   test "is valid" do … end
//! end
//!
//! class PaypalBillingAgreementTest < ActiveSupport::TestCase
//!   include CredentialSharedTests  # runs PaypalBillingAgreementTest#test_is_valid
//! end
//! ```
//!
//! A module-only file has no test class, so `ingest_test_files` returns
//! nothing for it, and the class only recorded the `include`: the tests
//! were in no index and the include named a module nothing emitted.
//!
//! Here the modules are read as they load ([`TestMixins::scan`]), and each
//! including class is simulated the way Ruby builds its method table
//! ([`TestMixins::splice`]): `include` order and ancestry, the method names
//! `ActiveSupport::Testing::Declarative#test` and Core's `TestTags` give,
//! the wrapper `SharedTests::ClassMethods#included` defines on the includer
//! for every `test_*` method (`<prefix>_setup`, `super`, `<prefix>_teardown`),
//! the raise `Declarative#test` makes on a name already defined, an
//! `ActiveSupport::Concern`'s `included do` block run on the class, and
//! the TestTags persistent hook that renames the class's own test methods.
//! The tests the class ends up running are spliced in as its own, under
//! their native method names; the module's helpers and constants come with
//! them, the class's own definitions winning, as they do in Ruby.
//!
//! A module declared once in a classless support file that also has its
//! own nested classes and modules, `class << self`, `def self.…` or
//! module-level state stays a real library module (`Mixin::real`): those
//! members, its constants, its helper methods that do not call `super`
//! and the `include`s that put a module in its own ancestry are carried in
//! the library file (`TestMixins::library_source`), and the includer
//! includes it. Its tests and callbacks are spliced, and so are copies of
//! its helpers, lowered as the includer's test code. Spliced bodies read a
//! bare constant in the module's lexical scopes, then in its own ancestry,
//! as Ruby does, among the constants loaded where the includer runs
//! (`qualify_lexical`). An `include` in it of a module not known to be
//! plain or a concern refuses its includers by name.
//!
//! What this does not model is said by name: a module whose tests cannot be
//! built, or whose body has a statement the splice does not model (a nested
//! class or `class << self` outside such a support file, a hook such as
//! `self.included`, `ClassMethods`, `private_constant`, an unknown `extend`
//! or DSL call), gets its includers refused; a statement whose tests are not
//! modeled (`generate_tests`, a computed name) records a gap on that module.

use ruby_prism::Node;

use crate::dialect::{MethodDef, MethodReceiver, MethodVisibility, Test, TestModule};
use crate::expr::{Expr, ExprNode};
use crate::span::Span;
use crate::{ClassId, Symbol};

use super::expr::ingest_expr;
use super::library_class::ingest_library_method;
use super::test::{
    class_body_items, hidden_test_declaration, ingest_setup_declaration, ingest_teardown_block,
    ingest_test_declaration, is_test_declaration_shape, merge_tags, name_with_tags, statement_tests, tag_declaration,
    BodyItem, TagConstants, TestTags,
};
use super::util::{class_name_path, constant_id_str, constant_path_is_rooted, constant_path_of, flatten_statements, module_name_path};
use super::{IngestError, IngestResult};

type Map<K, V> = crate::det::HashMap<K, V>;
type Set<T> = crate::det::HashSet<T>;
/// Each module's table, by mixin index: `None` until built.
type Tables = [Option<Result<Table, String>>];

fn table_of(tables: &Tables, m: usize) -> Result<&Table, &str> {
    match &tables[m] {
        Some(Ok(t)) => Ok(t),
        Some(Err(why)) => Err(why),
        None => Err("its table was not built"),
    }
}

/// A constant an `include` names, as written.
#[derive(Clone, Debug)]
struct IncludeRef {
    path: Vec<String>,
    rooted: bool,
}

impl IncludeRef {
    fn of(node: &Node<'_>) -> Option<Self> {
        let path = constant_path_of(node)?;
        let rooted = node.as_constant_path_node().is_some_and(|p| {
            // `::Foo` and `::Foo::Bar` both start at the root.
            let mut p = p;
            loop {
                match p.parent().and_then(|n| n.as_constant_path_node()) {
                    Some(parent) => p = parent,
                    None => return constant_path_is_rooted(&p),
                }
            }
        });
        Some(Self { path, rooted })
    }

    fn written(&self) -> String {
        self.path.join("::")
    }
}

/// One `module` declaration in a test file, as the scan saw it.
struct Decl {
    name: String,
    /// `Module.nesting` inside the body: the module, then each module
    /// around it, innermost first.
    nesting: Vec<String>,
    file: String,
    offset: usize,
    includes: Vec<IncludeRef>,
    /// The body has a statement other than a nested module or class.
    content: bool,
    /// The body declares a test, or an `included do` block declares a
    /// test, a tag or a setup for its includers.
    declares: bool,
    /// Core's `SharedTests` concern itself.
    provider: bool,
    /// The methods its `module ClassMethods` / `class_methods do` gives
    /// an includer's class body that declare tests when called
    /// (`test_macros`).
    macros: Vec<String>,
    /// Its `def teardown`, as `BodyTeardown::def`: its body's, or one its
    /// `included do` block defines on the includer.
    teardown: Option<bool>,
    /// The modules its `included do` block includes in the includer.
    block_includes: Vec<IncludeRef>,
    /// The body has what belongs to the module itself and not to an
    /// includer: a nested class or module (other than `ClassMethods`),
    /// `class << self`, a `def self.…`, or a module-level instance
    /// variable write.
    members: bool,
    /// The body extends `ActiveSupport::Concern`.
    concern: bool,
}

/// A class with a superclass: a test class may get all of its tests from
/// the shared test modules its body includes, and a subclass inherits
/// what its includes and teardowns registered (`TestMixins::ancestry`).
struct ClassRef {
    name: String,
    nesting: Vec<String>,
    includes: Vec<IncludeRef>,
    /// The constants its body `extend`s, in source order, with each
    /// call's offset.
    extends: Vec<(IncludeRef, usize)>,
    file: String,
    /// The superclass as written (`None` for an expression).
    superclass: Option<IncludeRef>,
    /// What the body itself declares for `teardown`.
    teardown: BodyTeardown,
}

/// A class body's own `teardown` declarations, as the scan read them.
#[derive(Clone, Copy, Debug, Default)]
struct BodyTeardown {
    /// A `teardown` call: a callback ActiveSupport runs after the
    /// `def teardown` chain.
    callbacks: bool,
    /// Its `def teardown`: `Some(true)` when the def's last statement
    /// is `super`, so whatever its ancestors run after it still runs
    /// last.
    def: Option<bool>,
}

/// A class-body statement of a shared test module, in source order.
enum Item {
    Include(Vec<IncludeRef>),
    ExtendConcern,
    ExtendDeclarative,
    Test { name: String, method_name: String, body: Expr, dsl: bool, tags_arg: bool, at: String },
    Setup(Expr, String),
    Teardown(Expr, String),
    Tag(String),
    IncludedBlock(String),
}

/// A shared test module, ingested.
struct Mixin {
    name: String,
    file: String,
    nesting: Vec<String>,
    provider: bool,
    items: Vec<Item>,
    helpers: Vec<MethodDef>,
    constants: Vec<(Symbol, Expr)>,
    /// The `included do … end` call of an `ActiveSupport::Concern`.
    included_block: Option<usize>,
    /// Why no includer can be built (the module raises while it loads, or
    /// declares something not modeled).
    fatal: Option<String>,
    /// Statements whose tests are not modeled; the module's other tests are.
    gaps: Vec<String>,
    /// The module is also a library module of its own (a support file's,
    /// declared once): what belongs to the module itself (`Decl::members`),
    /// its constants, helpers and own-ancestry `include`s stay in it,
    /// carried with the file (`TestMixins::library_source`), and the
    /// includer includes it where the shared module stands. Only the tests
    /// and callbacks it gives an includer are spliced.
    real: bool,
    /// Its body's `include`s, as (modules, the call's offset, whether
    /// `extend ActiveSupport::Concern` came before it, where).
    body_includes: Vec<(Vec<IncludeRef>, usize, bool, String)>,
    /// For a library module: the body `include`s it keeps
    /// (`TestMixins::library_includes`), by offset, with which of each
    /// call's modules it keeps (natively in its own ancestry, and defined
    /// when the library file loads).
    kept_includes: Vec<(usize, Vec<bool>)>,
    /// For a library module: its library body keeps its instance helpers
    /// too (`library_member`), because something reaches the module's own
    /// methods rather than an includer's copies: reflection on its
    /// methods, or a use the splice does not see (`TestMixins::reached`).
    keeps_helpers: bool,
    /// For a library module: a library class includes it
    /// (`TestMixins::library_included`), so its library body stays the
    /// Concern it is, `extend ActiveSupport::Concern` and `included do`
    /// with it, and that class's include runs the block natively.
    library_concern: bool,
    /// The modules in its own ancestry the test files declare, nearest
    /// first (`TestMixins::module_ancestry`): after its lexical scopes,
    /// Ruby looks a constant its bodies read up in these.
    ancestry: Vec<String>,
    /// Not a library module, but a test file reads its constant
    /// (`TestMixins::referenced`): the library file keeps the module as an
    /// empty marker, carrying no tests or helpers (the splice gives each
    /// includer those), and each includer includes it, so
    /// `Class.include?(Module)` answers as natively.
    marker: bool,
    /// A library module's `private_constant`s: Ruby refuses to read one by
    /// a scoped path (`::Mod::Name`), so a spliced body keeps it a bare name
    /// (as the module's own methods read it), which the includer's ancestry,
    /// the module included, resolves (`Mixin::own_copied`).
    private_constants: Vec<Symbol>,
}

impl Mixin {
    /// The constants a spliced body would no longer find by a bare name
    /// (`qualify_lexical`): everything but the module's own copied
    /// constants, which the includer gets (`ClassWalk::finish`).
    fn own_copied(&self) -> Vec<Symbol> {
        if self.real {
            self.private_constants.clone()
        } else {
            self.constants.iter().map(|(n, _)| n.clone()).collect()
        }
    }

}

/// A method's body and parameter defaults, through `q`.
fn qualify_method(h: &mut MethodDef, q: &dyn Fn(&mut Expr)) {
    q(&mut h.body);
    for p in h.params.iter_mut().chain(h.block_param.iter_mut()) {
        if let Some(d) = &mut p.default {
            q(d);
        }
    }
}

/// `name` and each namespace above it (`A::B::C`: `A::B`, `A`).
fn add_with_namespaces(set: &mut Set<String>, name: &str) {
    let name = name.trim_start_matches("::");
    for (at, _) in name.match_indices("::") {
        set.insert(name[..at].to_string());
    }
    set.insert(name.to_string());
}

/// A body spliced out of a module (into an includer, whose lexical
/// scopes are not the module's) reads each constant as Ruby resolves it
/// where the module's file wrote it: `Module.nesting` first, innermost
/// out. A bare (or relative) constant one of those scopes declares
/// (`lexical`: the test files' declarations and the app's) is rewritten
/// to the rooted path it resolves to, so `FailingRedisDriver` in
/// `Resiliency::TestHelper` stays `::Resiliency::TestHelper::FailingRedisDriver`
/// in any includer. `own` (the module's constants copied into the
/// includer) and what no scope declares stay as written; those resolve
/// through the includer's ancestry as before. After the lexical scopes,
/// Ruby looks in the ancestors of the innermost one: `ancestry`, the
/// module's (`Mixin::ancestry`). `lexical` answers for the constants
/// loaded where the includer runs (`loaded`).
fn qualify_lexical(e: &mut Expr, scopes: &[String], ancestry: &[String], own: &[Symbol], lexical: &dyn Fn(&str) -> bool) {
    if let ExprNode::Const { path } = &mut *e.node {
        if let Some(first) = path.first().filter(|f| !f.as_str().is_empty() && !own.contains(f)) {
            if let Some(scope) = scopes.iter().chain(ancestry).find(|s| lexical(&format!("{s}::{first}"))) {
                let mut rooted = vec![Symbol::from("")];
                rooted.extend(scope.split("::").map(Symbol::from));
                rooted.append(path);
                *path = rooted;
            }
        }
    }
    e.node.for_each_child_mut(&mut |c| qualify_lexical(c, scopes, ancestry, own, lexical));
}

/// Whether constant `name` is loaded where a test class of `file` runs,
/// having included the shared modules `used`: the app's and the support
/// files' (`TestMixins::lexical`), its own file's, and each of those
/// modules' files' (Ruby loaded each before the include).
fn loaded(reg: &TestMixins, file: &str, used: &[usize], name: &str) -> bool {
    reg.lexical.contains(name)
        || std::iter::once(file)
            .chain(used.iter().map(|&m| reg.mixins[m].file.as_str()))
            .any(|f| reg.declared.get(f).is_some_and(|d| d.contains(name)))
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Slot {
    /// A test the owner declares: an `Item::Test` index for a module, an
    /// own-test index for the class being spliced.
    Own(usize),
    /// The wrapper `SharedTests::ClassMethods#included` defined when the
    /// module (by index) was included.
    Wrapper(usize),
}

/// The owner's method table, as far as tests go, and its ancestry.
#[derive(Clone, Default)]
struct Table {
    /// The `test_*` methods the owner itself defines, in definition order.
    slots: Vec<(String, Slot)>,
    /// Ancestors after the owner, shared test modules only, in lookup order.
    mro: Vec<usize>,
    /// `ActiveSupport::Concern` dependencies deferred to the includer.
    deps: Vec<usize>,
    concern: bool,
    /// `SharedTests::ClassMethods#included` is the module's `included`.
    hook: bool,
    /// The last `setup do` / `teardown do` of a `SharedTests` module: its
    /// `<prefix>_setup` / `<prefix>_teardown` method.
    setup_fn: Option<Expr>,
    teardown_fn: Option<Expr>,
    /// The other modules it includes, flattened through shared test
    /// modules, in source order, and where each shared test module
    /// stands among them.
    passthrough: Vec<OutInclude>,
    /// For a class: the shared test modules its superclass chain already
    /// has. `Concern#append_features` returns early on them (`base <
    /// self`), so their `included do` blocks do not run again.
    inherited: Vec<usize>,
}

impl Table {
    fn slot(&self, name: &str) -> Option<Slot> {
        self.slots.iter().find(|(n, _)| n == name).map(|(_, s)| *s)
    }

    fn set(&mut self, name: &str, slot: Slot) {
        match self.slots.iter_mut().find(|(n, _)| n == name) {
            Some(entry) => entry.1 = slot,
            None => self.slots.push((name.to_string(), slot)),
        }
    }
}

/// One `include` of the emitted class, in the order it writes them.
#[derive(Clone, Debug, PartialEq)]
enum OutInclude {
    /// A module other than a shared test one, as the class includes it.
    Module(String),
    /// Where a shared test module (by index) stands: the module of the
    /// definitions of it a `super` reaches (`RoundhouseSuper…`), if it has
    /// any, is included there, so the lookup finds them where Ruby finds
    /// the module.
    Spliced(usize),
}

#[derive(Clone, Copy, PartialEq, PartialOrd)]
enum Dsl {
    /// `test` is `Kernel#test`.
    None,
    /// `extend ActiveSupport::Testing::Declarative`.
    Declarative,
    /// `include SharedTests`: TestTags, Declarative, and its setup/teardown.
    SharedTests,
}

/// Shared test modules read from the test files, and the splice of their
/// tests into the classes that include them.
#[derive(Default)]
pub(super) struct TestMixins {
    decls: Vec<Decl>,
    classes: Vec<ClassRef>,
    /// The reopened base class of the test classes, `class ActiveSupport::TestCase`
    /// with no superclass (Core's test-support initializer): what its body
    /// includes is in every test class's ancestry (`TestMixins::ancestry`).
    bases: Vec<ClassRef>,
    sources: Map<String, Vec<u8>>,
    /// Every module a test file declares.
    known: Set<String>,
    mixins: Vec<Mixin>,
    by_name: Map<String, usize>,
    tables: Vec<Option<Result<Table, String>>>,
    reported: std::cell::RefCell<Set<usize>>,
    /// Each module whose includers get a test-declaring class method
    /// (its own `macros` or an included module's), as (method, module).
    macro_modules: Map<String, Vec<(String, String)>>,
    /// The files that declare a shared test module (`finish`): spliced
    /// into their includers, so not library modules of their own.
    shared_files: Set<String>,
    /// `classes` by name: the first declaration of each.
    class_index: Map<String, usize>,
    /// Some shared test module's `included do` block registers a
    /// teardown callback (`applies` then checks subclasses too).
    block_teardowns: bool,
    /// `block_summary` of each of `mixins`.
    summaries: Vec<BlockSummary>,
    /// `decls` by name: every declaration of each module.
    module_index: Map<String, Vec<usize>>,
    /// Every class and module each test file declares, and each constant
    /// their bodies assign, by full name, by file.
    declared: Map<String, Set<String>>,
    /// The test-path files without a test class, which `ingest::app`
    /// carries as library files (`set_library_context`).
    support_files: Set<String>,
    /// The modules the app and its gems declare outside the test paths, by
    /// full name, with whether each `extend`s `ActiveSupport::Concern`
    /// (`set_app_modules`). A shared test module's `include` of one is
    /// kept or deferred by what the module is, as for a support file's.
    app_modules: Map<String, bool>,
    /// The constants a body's lexical scopes may have wherever it runs
    /// (`qualify_lexical`): the app's, the support files' `declared`, and
    /// every namespace above one. A test class's file adds its own
    /// (`ClassWalk::file_lexical`); another test class's file is not
    /// loaded with it.
    lexical: Set<String>,
    /// The support files of real modules (`Mixin::real`) whose library
    /// carry failed, with why (`note_uncarried`).
    uncarried: std::cell::RefCell<Map<String, String>>,
    /// Each module the test files reach other than by a test class's or
    /// module's own `include`, as written: a receiver of
    /// `METHOD_REFLECTION`, or what an `include`/`extend`/`prepend` with a
    /// receiver, in a block or method, or in a class without a superclass
    /// names (`reached_modules`); `::` leads a rooted one. A module is
    /// reached by its rooted name, or by a relative one its full name ends
    /// with, which errs toward keeping its helpers where that resolves
    /// lexically elsewhere.
    reached: Set<String>,
    /// The modules a receiverless `include` in the body of a class
    /// without a superclass names, as written (`reached_modules`): Core's
    /// `class ActiveSupport::TestCase; include ActiveRecordTestHelper` in
    /// a test-support initializer. Matched as `reached` is.
    library_included: Set<String>,
    /// Each constant the test files read other than as the module of a
    /// test class's or module's own `include`, as written
    /// (`referenced_constants`): `self.class.include?(GraphQLMutation
    /// ReturningJobTests)`. A shared module that is not a library module
    /// is spliced into each includer, so a reference like that needs a
    /// marker (`Mixin::marker`). Matched as `reached` is.
    referenced: Set<String>,
}

/// What a concern's `included do` block does on the class, as far as a
/// subclass's splice needs it (`TestMixins::ancestry`).
#[derive(Default)]
struct BlockSummary {
    includes: Vec<IncludeRef>,
    /// It registers a `teardown` callback.
    callbacks: bool,
    /// It defines `def teardown` on the class.
    def_teardown: bool,
}

/// One test class above the one being spliced, as the scan read it.
#[derive(Clone, Debug)]
struct AncestorTeardown {
    name: String,
    /// It registers teardown callbacks: its body's, or a concern's
    /// `included do` block's.
    callbacks: bool,
    /// Its body includes shared test modules, so the splice rebuilds its
    /// `teardown` (its def part then reaches the def chain through
    /// `super`).
    spliced: bool,
    /// Its `def teardown`, as `BodyTeardown::def`. One an `included do`
    /// block defines counts as not ending in `super`.
    def: Option<bool>,
    /// The modules its body includes, through what they include in turn
    /// (`TestMixins::module_teardowns`).
    modules: Vec<ModuleTeardown>,
}

/// A module in a test class's ancestry, as far as its `def teardown` goes.
#[derive(Clone, Debug)]
struct ModuleTeardown {
    /// As a test file declares it, else as written.
    name: String,
    /// A test file declares it, so the scan read its `def teardown`; or it
    /// is one of `NO_TEARDOWN`.
    known: bool,
    /// Its `def teardown`, as `Decl::teardown`.
    def: Option<bool>,
}

/// The Rails test classes that descend from `ActiveSupport::TestCase`: a test
/// class whose superclass chain ends at one has the modules Core's test-support
/// initializer includes into `ActiveSupport::TestCase` in its ancestry.
const TEST_CASE_ROOTS: &[&str] = &[
    "ActiveSupport::TestCase",
    "ActiveJob::TestCase",
    "ActionView::TestCase",
    "ActionController::TestCase",
    "ActionMailer::TestCase",
    "ActionMailbox::TestCase",
    "ActionCable::TestCase",
    "ActionDispatch::IntegrationTest",
    "ActionDispatch::SystemTestCase",
];

/// Modules of Ruby and of the testing gems Core uses (minitest 5.27,
/// ActiveSupport, mocha) that a test class includes and that define no
/// `teardown` (checked against those gems): what they bring cannot change
/// the `def teardown` chain.
const NO_TEARDOWN: &[&str] = &[
    "Kernel",
    "Minitest::Assertions",
    "ActiveSupport::Testing::Assertions",
    "ActiveSupport::Testing::TimeHelpers",
    "ActiveSupport::Testing::ErrorReporterAssertions",
    "ActiveSupport::Testing::ConstantStubbing",
    "Mocha::API",
];

/// Modules of Ruby itself: a library module's `include` of one puts it in
/// the module's own ancestry, and it is defined before any app file loads.
const CORE_LIBRARY_MODULES: &[&str] = &["Kernel", "Comparable", "Enumerable"];

/// Modules of the testing gems Core uses (minitest 5.27, ActiveSupport,
/// ActiveJob) that are not an `ActiveSupport::Concern` and define no
/// `included` hook (checked against those gems). Natively a library
/// module's `include` of one puts it in the module's own ancestry; but the
/// library file loads with the app tree, before the test runtime defines
/// them (the CRuby tree's `RoundhouseAutoload.eager_load!` runs in boot,
/// ahead of `test/test_helper.rb`), so the include is left to each
/// includer, which the splice gives it where the module stands
/// (`ClassWalk`): the includer's ancestry has it right after the module,
/// as natively. Only reflection on the module itself (`M.include?(X)`,
/// `M.ancestors`) tells.
const TEST_FRAMEWORK_MODULES: &[&str] = &[
    "Minitest::Assertions",
    "ActiveSupport::Testing::Assertions",
    "ActiveSupport::Testing::TimeHelpers",
    "ActiveSupport::Testing::ErrorReporterAssertions",
    "ActiveSupport::Testing::ConstantStubbing",
    "ActiveJob::TestHelper",
];

/// Calls on a module that read or change its own instance methods: a
/// library module so reflected on keeps its helpers in its library body
/// (`Mixin::keeps_helpers`).
const METHOD_REFLECTION: &[&str] = &[
    "instance_methods",
    "public_instance_methods",
    "private_instance_methods",
    "protected_instance_methods",
    "undefined_instance_methods",
    "instance_method",
    "public_instance_method",
    "method_defined?",
    "public_method_defined?",
    "private_method_defined?",
    "protected_method_defined?",
    "remove_method",
    "undef_method",
    "alias_method",
    "send",
    "public_send",
    "__send__",
];

/// What a test class inherits from the test classes the scan read above
/// it.
#[derive(Default)]
struct Ancestry {
    /// Shared test modules already in its superclass chain.
    mixins: Vec<usize>,
    /// Its known ancestors, nearest first, up to the first one the test
    /// files do not declare.
    classes: Vec<AncestorTeardown>,
}

impl TestMixins {
    /// Read the module declarations of one test file.
    pub(super) fn scan(&mut self, source: &[u8], file: &str) {
        // A file with no module, no class and no `include`/`extend`
        // declares nothing the scan records. A file with only classes
        // still matters: a subclass inherits the includes and teardowns
        // of the classes above it.
        let has = |word: &[u8]| source.windows(word.len()).any(|w| w == word);
        if !has(b"module ") && !has(b"class ") && !has(b"include") && !has(b"extend") {
            return;
        }
        let result = super::prism::parse_silent(source);
        let root = result.node();
        let before = self.decls.len();
        let stmts = root.as_program_node().map(|p| p.statements().body().iter().collect::<Vec<_>>()).unwrap_or_default();
        reached_modules(&root, &mut self.reached, &mut self.library_included);
        referenced_constants(&root, &mut self.referenced);
        let declared = self.declared.entry(file.to_string()).or_default();
        for stmt in stmts {
            scan_node(&stmt, &[], file, &mut self.decls, &mut self.classes, &mut self.bases, &mut self.known, declared);
        }
        for name in declared.iter().cloned().collect::<Vec<_>>() {
            add_with_namespaces(declared, &name);
        }
        if self.decls[before..].iter().any(|d| d.declares || d.provider || !d.includes.is_empty()) {
            self.sources.insert(file.to_string(), source.to_vec());
        }
    }

    /// Before `finish`: the test-path files `ingest::app` carries as
    /// library files (the ones without a test class), and the constants
    /// the app declares (its library classes and modules, their
    /// constants, its models), for the lexical lookup of the bodies
    /// spliced out of a module (`qualify_lexical`).
    pub(super) fn set_library_context(&mut self, support_files: impl IntoIterator<Item = String>, app_constants: impl IntoIterator<Item = String>) {
        self.support_files.extend(support_files);
        for name in app_constants {
            add_with_namespaces(&mut self.lexical, &name);
        }
    }

    /// Before `finish`: the modules the app and its gems declare (not in
    /// a test file), each with whether it extends `ActiveSupport::Concern`.
    /// A shared test module that includes one needs the kind to decide
    /// whether the include stays in its library body or is deferred to
    /// each includer (`library_include`).
    pub(super) fn set_app_modules(&mut self, modules: impl IntoIterator<Item = (String, bool)>) {
        self.app_modules.extend(modules);
    }

    /// After every test file is scanned: ingest the modules that matter to
    /// a test class's tests — the ones that declare tests, include such a
    /// module, or are `SharedTests` — and build their method tables.
    pub(super) fn finish(&mut self) {
        for (file, names) in &self.declared {
            if self.support_files.contains(file) {
                self.lexical.extend(names.iter().cloned());
            }
        }
        let mut content: Map<String, Vec<usize>> = Map::default();
        for (i, d) in self.decls.iter().enumerate() {
            if d.content {
                content.entry(d.name.clone()).or_default().push(i);
            }
        }
        let resolve = |r: &IncludeRef, nesting: &[String]| -> Option<String> {
            candidates(r, nesting).into_iter().find(|c| content.contains_key(c))
        };
        let mut relevant: Set<String> = Set::default();
        for d in &self.decls {
            if d.declares || d.provider {
                relevant.insert(d.name.clone());
            }
        }
        loop {
            let mut grew = false;
            for d in &self.decls {
                if relevant.contains(&d.name) {
                    continue;
                }
                if d.includes.iter().any(|r| resolve(r, &d.nesting).is_some_and(|n| relevant.contains(&n))) {
                    relevant.insert(d.name.clone());
                    grew = true;
                }
            }
            if !grew {
                break;
            }
        }
        self.shared_files = self.decls.iter().filter(|d| relevant.contains(&d.name)).map(|d| d.file.clone()).collect();
        let mut names: Vec<&String> = relevant.iter().collect();
        names.sort();
        for name in names {
            let Some(decls) = content.get(name) else { continue };
            let decl = &self.decls[decls[0]];
            // A support file's module declared once is a library module
            // too; one in a test class's file (not carried) or reopened
            // keeps its members refused.
            let real = decl.members && decls.len() == 1 && !decl.provider && self.support_files.contains(&decl.file);
            let mut mixin = match self.sources.get(&decl.file) {
                Some(source) => ingest_mixin(decl, source, real),
                None => continue,
            };
            if decls.len() > 1 {
                let files: Vec<&str> = decls.iter().map(|&i| self.decls[i].file.as_str()).collect();
                mixin.fatal.get_or_insert(format!(
                    "module `{name}` is declared more than once, in {}; a reopened shared test module is not modeled",
                    files.join(", ")
                ));
            }
            self.by_name.insert(name.clone(), self.mixins.len());
            self.mixins.push(mixin);
        }
        for (i, d) in self.decls.iter().enumerate() {
            self.module_index.entry(d.name.clone()).or_default().push(i);
        }
        for k in 0..self.mixins.len() {
            let m = &self.mixins[k];
            let includes: Vec<(IncludeRef, bool)> =
                m.body_includes.iter().flat_map(|(refs, _, after, _)| refs.iter().map(|r| (r.clone(), *after))).collect();
            let mut ancestry = Vec::new();
            self.module_ancestry(&includes, &m.nesting.clone(), &mut ancestry, 0);
            self.mixins[k].ancestry = ancestry;
            let name = self.mixins[k].name.clone();
            let names = |w: &String| match w.strip_prefix("::") {
                Some(full) => name == full,
                None => name == *w || name.ends_with(&format!("::{w}")),
            };
            let marker = !self.mixins[k].real && self.mixins[k].fatal.is_none() && self.referenced.iter().any(names);
            self.mixins[k].marker = marker;
            if self.mixins[k].real {
                let keeps_helpers = self.reached.iter().any(names);
                let library_concern = self.library_included.iter().any(names);
                self.mixins[k].keeps_helpers = keeps_helpers;
                self.mixins[k].library_concern = library_concern;
                if self.mixins[k].fatal.is_none() {
                    match self.library_includes(k) {
                        Ok(kept) => self.mixins[k].kept_includes = kept,
                        Err(why) => self.mixins[k].fatal = Some(why),
                    }
                }
            }
        }
        let mut tables: Vec<Option<Result<Table, String>>> = vec![None; self.mixins.len()];
        for i in 0..self.mixins.len() {
            let mut stack = Vec::new();
            self.build(i, &mut tables, &mut stack);
        }
        self.tables = tables;
        for (i, c) in self.classes.iter().enumerate() {
            self.class_index.entry(c.name.clone()).or_insert(i);
        }
        self.summaries = (0..self.mixins.len()).map(|k| self.block_summary(k)).collect();
        self.block_teardowns = self.summaries.iter().any(|s| s.callbacks);

        // ActiveSupport::Concern extends an includer with the class
        // methods of the module and of the concerns it includes.
        if self.decls.iter().any(|d| !d.macros.is_empty()) {
            let mut by_name: Map<&str, Vec<usize>> = Map::default();
            for (i, d) in self.decls.iter().enumerate() {
                by_name.entry(d.name.as_str()).or_default().push(i);
            }
            let mut macro_modules: Map<String, Vec<(String, String)>> = Map::default();
            for (i, d) in self.decls.iter().enumerate() {
                let (mut seen, mut stack, mut got) = (Set::default(), vec![i], Vec::new());
                while let Some(j) = stack.pop() {
                    if !seen.insert(j) {
                        continue;
                    }
                    let e = &self.decls[j];
                    got.extend(e.macros.iter().map(|m| (m.clone(), e.name.clone())));
                    for r in &e.includes {
                        if let Some(found) = candidates(r, &e.nesting).iter().find_map(|c| by_name.get(c.as_str())) {
                            stack.extend(found.iter().copied());
                        }
                    }
                }
                if !got.is_empty() {
                    let entry = macro_modules.entry(d.name.clone()).or_default();
                    for m in got {
                        if !entry.contains(&m) {
                            entry.push(m);
                        }
                    }
                }
            }
            self.macro_modules = macro_modules;
        }
    }

    /// Which of library module `k`'s body `include`s (`Mixin::real`) its
    /// library body keeps, by offset, with which of each call's modules:
    /// natively each puts its module in `k`'s own ancestry, unless `k`
    /// already extends `ActiveSupport::Concern` and the module is a
    /// concern too (`append_features` then defers it to each includer,
    /// which the splice includes it in). A testing gem's module is left to
    /// the includers too (`TEST_FRAMEWORK_MODULES`). A module whose kind
    /// or library form is not known refuses `k`'s includers by name.
    fn library_includes(&self, k: usize) -> Result<Vec<(usize, Vec<bool>)>, String> {
        let m = &self.mixins[k];
        let mut kept = Vec::new();
        for (refs, offset, after_concern, site) in &m.body_includes {
            let keep = refs.iter().map(|r| self.library_include(m, r, *after_concern, site)).collect::<Result<Vec<bool>, String>>()?;
            if keep.iter().any(|&k| k) {
                kept.push((*offset, keep));
            }
        }
        Ok(kept)
    }

    /// The modules a body's `includes` (each with whether `extend
    /// ActiveSupport::Concern` came before it), written in a body with
    /// `nesting`, put in its module's ancestry, nearest first, through
    /// what they include in turn: each a test file declares, unless both
    /// are concerns (then `append_features` deferred it to the includer).
    fn module_ancestry(&self, includes: &[(IncludeRef, bool)], nesting: &[String], out: &mut Vec<String>, depth: usize) {
        if depth > 8 {
            return;
        }
        // `include A; include B` looks in B first.
        for (r, after_concern) in includes.iter().rev() {
            let Some((name, decls)) = candidates(r, nesting).into_iter().find_map(|c| self.module_index.get(&c).map(|d| (c, d))) else { continue };
            let concern = decls.iter().any(|&i| self.decls[i].concern || self.decls[i].provider);
            if (concern && *after_concern) || out.contains(&name) {
                continue;
            }
            out.push(name);
            for &i in decls {
                let d = &self.decls[i];
                let inner: Vec<(IncludeRef, bool)> = d.includes.iter().map(|r| (r.clone(), d.concern)).collect();
                self.module_ancestry(&inner, &d.nesting, out, depth + 1);
            }
        }
    }

    /// `library_includes` for one module `r` names.
    fn library_include(&self, m: &Mixin, r: &IncludeRef, after_concern: bool, site: &str) -> Result<bool, String> {
        let written = r.written();
        let found = candidates(r, &m.nesting).into_iter().find_map(|c| self.module_index.get(&c).map(|d| (c, d)));
        let Some((name, decls)) = found else {
            let gem = written.trim_start_matches("::");
            if CORE_LIBRARY_MODULES.contains(&gem) {
                return Ok(true);
            }
            if TEST_FRAMEWORK_MODULES.contains(&gem) {
                return Ok(false);
            }
            // A module of the app or a gem (`Markets::TestHelper`,
            // `FixtureFactory::Registry`): loaded with the app tree, so a
            // plain one stays in the module's own ancestry, as natively,
            // and a concern after this module's own `extend
            // ActiveSupport::Concern` is deferred to each includer.
            if let Some((name, concern)) = candidates(r, &m.nesting).into_iter().find_map(|c| self.app_modules.get(&c).map(|k| (c, *k))) {
                if concern && after_concern {
                    return Ok(false);
                }
                if concern {
                    return Err(format!(
                        "its `include {written}` {site} comes before an `extend ActiveSupport::Concern` (or it has none): the concern `{name}` is included into `{}` itself, its `included do` block run on the module, which the library module does not model",
                        m.name
                    ));
                }
                return Ok(true);
            }
            return Err(format!(
                "its `include {written}` {site}: whether `{written}` is an ActiveSupport::Concern (an include deferred to each includer) or a plain module (one in `{}`'s own ancestry) is not known to the shared-test splice",
                m.name
            ));
        };
        let concern = decls.iter().any(|&i| self.decls[i].concern || self.decls[i].provider);
        if concern && after_concern {
            return Ok(false);
        }
        if concern {
            return Err(format!(
                "its `include {written}` {site} comes before an `extend ActiveSupport::Concern` (or it has none): the concern `{name}` is included into `{}` itself, its `included do` block run on the module, which the library module does not model",
                m.name
            ));
        }
        if let Some(&j) = self.by_name.get(&name) {
            if !self.mixins[j].real {
                return Err(format!(
                    "its `include {written}` {site} puts shared tests `{name}` in `{}`'s own ancestry, but `{name}` is spliced into each includer, not a library module of its own",
                    m.name
                ));
            }
            return Ok(true);
        }
        match decls.iter().map(|&i| &self.decls[i].file).find(|f| !self.support_files.contains(*f)) {
            Some(file) => Err(format!(
                "its `include {written}` {site} names `{name}`, which {file} declares: a test class's file, not carried as a library file"
            )),
            None => Ok(true),
        }
    }

    /// Whether `file` declares a shared test module (one that declares
    /// tests, tags or setups for its includers, includes such a module,
    /// or is `SharedTests`). Its modules are spliced into the test
    /// classes that include them; a file of other modules only is a
    /// test-support library file (`ingest::app`).
    pub(super) fn declares_shared_tests(&self, file: &str) -> bool {
        self.shared_files.contains(file)
    }

    /// The library file `file` (a support file declaring shared test
    /// modules) is, natively, minus what the splice gives each includer:
    /// each shared module's body keeps what belongs to the module itself
    /// when it is a library module (`Mixin::real`: its nested classes and
    /// modules, `class << self`, `def self.…`, module-level instance
    /// variables, constants, `extend T::…`, attributes, the `include`s
    /// `library_includes` keeps, and its helpers when something reaches
    /// them there, `Mixin::keeps_helpers`; `library_member`), and nothing
    /// for one that is not; the file's other classes, modules and requires
    /// stay as written. Removed statements and arguments become blanks of
    /// the same lines, so every kept offset and line is the source's.
    /// `None` when nothing would be left but empty shared modules: the
    /// file is spliced only, as before; or when a statement cannot keep
    /// just some of its arguments (`note_uncarried` then says why).
    pub(super) fn library_source(&self, file: &str, source: &[u8]) -> Option<Vec<u8>> {
        let mut spliced: Map<usize, Option<LibraryForm>> = Map::default();
        let mut markers: Set<usize> = Set::default();
        let mut kept: Vec<(usize, Vec<bool>)> = Vec::new();
        for d in self.decls.iter().filter(|d| d.file == file) {
            if let Some(&k) = self.by_name.get(&d.name) {
                let m = &self.mixins[k];
                if m.marker {
                    markers.insert(d.offset);
                }
                spliced.insert(d.offset, m.real.then_some(LibraryForm { helpers: m.keeps_helpers, concern: m.library_concern }));
                kept.extend(m.kept_includes.iter().cloned());
            }
        }
        if spliced.is_empty() {
            return None;
        }
        let result = super::prism::parse_silent(source);
        let root = result.node();
        let stmts = root.as_program_node().map(|p| p.statements().body().iter().collect::<Vec<_>>()).unwrap_or_default();
        let mut walk = LibraryBlanks { spliced: &spliced, markers: &markers, kept: &kept, source, blank: Vec::new(), continued: Vec::new(), keeps: false };
        if let Err(why) = walk.walk(&stmts, false) {
            self.note_uncarried(file, why);
            return None;
        }
        if !walk.keeps {
            return None;
        }
        let mut out = source.to_vec();
        for (start, end) in walk.blank {
            for b in &mut out[start..end] {
                if *b != b'\n' {
                    *b = b' ';
                }
            }
        }
        for at in walk.continued {
            out[at] = b'\\';
        }
        Some(out)
    }

    /// The library carry of `file` (`library_source`) failed: the
    /// includers of its library modules are refused with `why`.
    pub(super) fn note_uncarried(&self, file: &str, why: String) {
        self.uncarried.borrow_mut().entry(file.to_string()).or_insert(why);
    }

    /// Each scanned class with a superclass, with its
    /// `Module.nesting` (the class, then each scope around it as the file
    /// wrote them, innermost first): `module Foo; class BarTest` is
    /// [Foo::BarTest, Foo], `class Foo::BarTest` is [Foo::BarTest].
    pub(super) fn class_nestings(&self) -> crate::det::HashMap<String, Vec<String>> {
        self.classes.iter().map(|c| (c.name.clone(), c.nesting.clone())).collect()
    }

    /// Each scanned class's `extend`s, as (class, file, [(candidates,
    /// offset)]): the names the constant may resolve to, innermost scope
    /// first and the top level last (just the top level for a rooted
    /// `::M`), and the call's offset in the file. The test-class ingest
    /// does not carry a class-body `extend`; `ingest::app` resolves these
    /// against the app's modules.
    pub(super) fn class_extends(&self) -> Vec<(String, String, Vec<(Vec<String>, usize)>)> {
        self.classes
            .iter()
            .filter(|c| !c.extends.is_empty())
            .map(|c| {
                let extends = c.extends.iter().map(|(r, at)| (candidates(r, &c.nesting), *at)).collect();
                (c.name.clone(), c.file.clone(), extends)
            })
            .collect()
    }

    /// The test-declaring class methods the modules `tm` includes give
    /// it, as (method, module).
    fn included_test_macros(&self, tm: &TestModule) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = Vec::new();
        for inc in &tm.includes {
            let written = inc.0.as_str();
            for (name, macros) in &self.macro_modules {
                if name == written || name.ends_with(&format!("::{written}")) {
                    out.extend(macros.iter().filter(|m| !out.contains(m)).cloned().collect::<Vec<_>>());
                }
            }
        }
        out.sort();
        out
    }

    /// Whether a module `tm` includes gives it a class method that
    /// declares tests (`mark_test_macro_calls`).
    pub(super) fn gives_test_macros(&self, tm: &TestModule) -> bool {
        !self.macro_modules.is_empty() && !self.included_test_macros(tm).is_empty()
    }

    /// Each statement of `tm`'s class body that calls a test-declaring
    /// class method an included module gives it (`test_input_field_
    /// authorization(ProductInput, :vendor, …)`, whose `ClassMethods`
    /// body runs `generate_test`) declares tests ingest does not see. The
    /// statement is carried (`TestModule::body_statements`) and the
    /// included module's class method is the app's, so the emitted class
    /// declares them as it loads: the class lists them then
    /// (`TestModule::tests_at_load`).
    pub(super) fn mark_test_macro_calls(&self, tm: &mut TestModule, source: &[u8], file: &str) {
        let macros = self.included_test_macros(tm);
        if macros.is_empty() {
            return;
        }
        let result = super::prism::parse_silent(source);
        let root = result.node();
        let Some((class, _)) = find_class(&root, &[], tm.name.0.as_str()) else { return };
        for stmt in class.body().map(flatten_statements).unwrap_or_default() {
            if stmt.as_def_node().is_some()
                || stmt.as_class_node().is_some()
                || stmt.as_module_node().is_some()
                || stmt.as_singleton_class_node().is_some()
            {
                continue;
            }
            let hit = self_calls(&stmt).into_iter().any(|(called, _)| macros.iter().any(|(m, _)| *m == called));
            if hit && !super::test::macro_call_expanded(file, stmt.location().start_offset()) {
                tm.tests_at_load = true;
            }
        }
    }

    /// Test classes `ingest_test_files` did not reach that include shared
    /// test modules, as (file, class name): a class nested in a class that
    /// is not a test (`class Global; class PaypalBillingAgreementTest <
    /// ActiveSupport::TestCase`) whose tests all come from the modules.
    pub(super) fn unreached_includers(&self, have: &dyn Fn(&str) -> bool) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = Vec::new();
        for c in &self.classes {
            if have(&c.name) || out.iter().any(|(_, n)| n == &c.name) {
                continue;
            }
            if c.includes.iter().any(|r| self.resolve(r, &c.nesting).is_some()) {
                out.push((c.file.clone(), c.name.clone()));
            }
        }
        out
    }

    /// Ingest the test class `name` of `source` (one `unreached_includers`
    /// named), the way `ingest_test_files` ingests a reached one.
    pub(super) fn ingest_includer(&self, source: &[u8], file: &str, name: &str) -> IngestResult<Option<TestModule>> {
        let result = super::prism::parse_silent(source);
        let root = result.node();
        let Some((class, nesting)) = find_class(&root, &[], name) else { return Ok(None) };
        let scope: Vec<String> = nesting.get(1).map(|o| o.split("::").map(str::to_string).collect()).unwrap_or_default();
        let tm = super::test::ingest_test_class(&(scope, class), &[], &[], file).map_err(|err| match err {
            IngestError::Unsupported { file, message } => {
                IngestError::Unsupported { message: format!("test class `{name}` refused: {message}"), file }
            }
            other => other,
        })?;
        Ok(Some(tm))
    }

    /// Whether `tm` may include a shared test module (one of its includes
    /// names one, by its last segments), or inherits from a test class
    /// whose spliced teardown registers callbacks (`splice` checks that
    /// its own teardown keeps them running as Rails runs them).
    pub(super) fn applies(&self, tm: &TestModule) -> bool {
        self.may_include_shared(tm) || self.inherits_spliced_callbacks(tm)
    }

    fn may_include_shared(&self, tm: &TestModule) -> bool {
        tm.includes.iter().any(|inc| {
            let written = inc.0.as_str();
            self.by_name.keys().any(|n| n == written || n.ends_with(&format!("::{written}")))
        })
    }

    fn inherits_spliced_callbacks(&self, tm: &TestModule) -> bool {
        if !self.block_teardowns {
            return false;
        }
        let Some(&i) = self.class_index.get(tm.name.0.as_str()) else { return false };
        let c = &self.classes[i];
        self.ancestry(c.superclass.as_ref(), &c.nesting[1..]).classes.iter().any(|a| a.callbacks && a.spliced)
    }

    /// What a class whose superclass is written `superclass`, in a body
    /// with nesting `outer` (the class's own nesting without itself),
    /// inherits from the test classes above it: their includes simulated
    /// in order on one table, as Ruby builds the chain.
    fn ancestry(&self, superclass: Option<&IncludeRef>, outer: &[String]) -> Ancestry {
        let mut chain: Vec<&ClassRef> = Vec::new();
        let mut next = superclass.map(|r| (r.clone(), outer.to_vec()));
        while let Some((r, nesting)) = next.take() {
            let Some(&i) = candidates(&r, &nesting).iter().find_map(|c| self.class_index.get(c)) else { break };
            let c = &self.classes[i];
            if chain.len() > 32 || chain.iter().any(|d| d.name == c.name) {
                break;
            }
            chain.push(c);
            next = c.superclass.clone().map(|r| (r, c.nesting[1..].to_vec()));
        }
        let mut table = Table::default();
        let mut classes: Vec<AncestorTeardown> = Vec::new();
        // A test class descends from `ActiveSupport::TestCase`, which Core's
        // test-support initializer reopens to include helpers: `include` of one
        // of them again is a no-op (`Concern#append_features` returns early when
        // `base < self`), so its `included do` block does not run a second time.
        // Only the table learns of them: the base's callbacks are not a class of
        // the chain.
        let root = chain.last().map_or(superclass, |c| c.superclass.as_ref());
        if root.is_some_and(|r| TEST_CASE_ROOTS.contains(&r.written().trim_start_matches("::"))) {
            for base in &self.bases {
                let mut scratch = AncestorTeardown { name: base.name.clone(), callbacks: false, spliced: false, def: None, modules: Vec::new() };
                self.simulate_includes(&mut table, &base.includes, &base.nesting, &mut scratch, 0);
            }
        }
        for c in chain.iter().rev() {
            let mut a = AncestorTeardown {
                name: c.name.clone(),
                callbacks: c.teardown.callbacks,
                spliced: false,
                def: c.teardown.def,
                modules: self.module_teardowns(&c.includes, &c.nesting),
            };
            self.simulate_includes(&mut table, &c.includes, &c.nesting, &mut a, 0);
            classes.push(a);
        }
        classes.reverse();
        Ancestry { mixins: table.mro, classes }
    }

    /// An ancestor's `include`s, on the chain's table: the concerns whose
    /// `included do` blocks run on it, and what those blocks register.
    fn simulate_includes(&self, table: &mut Table, refs: &[IncludeRef], nesting: &[String], a: &mut AncestorTeardown, depth: usize) {
        for r in refs.iter().rev() {
            let Some(k) = self.resolve(r, nesting) else { continue };
            a.spliced = true;
            let Ok(out) = self.include_module(table, k, true, &self.tables) else { continue };
            for b in out.blocks {
                let s = &self.summaries[b];
                a.callbacks |= s.callbacks;
                if s.def_teardown {
                    a.def = Some(false);
                }
                if depth < 8 {
                    let nesting = self.mixins[b].nesting.clone();
                    self.simulate_includes(table, &s.includes, &nesting, a, depth + 1);
                }
            }
        }
    }

    /// The modules including `refs` (written in a body with `nesting`)
    /// brings into a class's ancestry: each, and what it includes, its
    /// `included do` block included, in turn. One no test file declares
    /// is not `known`, nor is what it includes.
    fn module_teardowns(&self, refs: &[IncludeRef], nesting: &[String]) -> Vec<ModuleTeardown> {
        let mut out: Vec<ModuleTeardown> = Vec::new();
        let mut stack: Vec<(IncludeRef, Vec<String>)> = refs.iter().map(|r| (r.clone(), nesting.to_vec())).collect();
        stack.reverse();
        while let Some((r, nesting)) = stack.pop() {
            if out.len() > 256 {
                break;
            }
            let found = candidates(&r, &nesting).into_iter().find_map(|c| self.module_index.get(&c).map(|d| (c, d)));
            let Some((name, decls)) = found else {
                let name = r.written();
                if !out.iter().any(|m| m.name == name) {
                    let known = NO_TEARDOWN.contains(&name.as_str());
                    out.push(ModuleTeardown { name, known, def: None });
                }
                continue;
            };
            if out.iter().any(|m| m.name == name) {
                continue;
            }
            let mut def: Option<bool> = None;
            let mut inner: Vec<(IncludeRef, Vec<String>)> = Vec::new();
            for &i in decls {
                let d = &self.decls[i];
                if let Some(ok) = d.teardown {
                    def = Some(def.unwrap_or(true) && ok);
                }
                inner.extend(d.includes.iter().chain(&d.block_includes).map(|r| (r.clone(), d.nesting.clone())));
            }
            out.push(ModuleTeardown { name, known: true, def });
            stack.extend(inner.into_iter().rev());
        }
        out
    }

    /// The statements of `k`'s `included do` block that matter to a
    /// subclass: its includes, teardown callbacks and `def teardown`.
    fn block_summary(&self, k: usize) -> BlockSummary {
        let m = &self.mixins[k];
        let mut s = BlockSummary::default();
        let (Some(offset), Some(source)) = (m.included_block, self.sources.get(&m.file)) else { return s };
        let result = super::prism::parse_silent(source);
        let root = result.node();
        let Some(call) = find_call(&root, offset) else { return s };
        let stmts = call.block().and_then(|b| b.as_block_node()).and_then(|b| b.body()).map(flatten_statements).unwrap_or_default();
        for stmt in &stmts {
            if let Some(c) = stmt.as_call_node().filter(|c| c.receiver().is_none()) {
                match constant_id_str(&c.name()) {
                    "include" => {
                        if let Some(args) = c.arguments() {
                            s.includes.extend(args.arguments().iter().filter_map(|n| IncludeRef::of(&n)));
                        }
                    }
                    "teardown" => s.callbacks = true,
                    _ => {}
                }
            }
            if instance_def(stmt).is_some_and(|d| d.name().as_slice() == b"teardown") {
                s.def_teardown = true;
            }
        }
        s
    }

    fn resolve(&self, r: &IncludeRef, nesting: &[String]) -> Option<usize> {
        candidates(r, nesting).into_iter().find_map(|c| self.by_name.get(&c).copied())
    }

    /// The include as the emitted class should write it: qualified when a
    /// test file declares the module under one of the nesting scopes.
    fn qualify(&self, r: &IncludeRef, nesting: &[String]) -> String {
        candidates(r, nesting).into_iter().find(|c| self.known.contains(c)).unwrap_or_else(|| r.written())
    }

    fn build(&self, idx: usize, tables: &mut Vec<Option<Result<Table, String>>>, stack: &mut Vec<usize>) {
        if tables[idx].is_some() {
            return;
        }
        let m = &self.mixins[idx];
        if stack.contains(&idx) {
            tables[idx] = Some(Err(format!("shared test module `{}` includes itself", m.name)));
            return;
        }
        if let Some(fatal) = &m.fatal {
            tables[idx] = Some(Err(fatal.clone()));
            return;
        }
        stack.push(idx);
        let result = self.build_table(idx, tables, stack);
        stack.pop();
        tables[idx] = Some(result);
    }

    fn build_table(&self, idx: usize, tables: &mut Vec<Option<Result<Table, String>>>, stack: &mut Vec<usize>) -> Result<Table, String> {
        let m = &self.mixins[idx];
        let mut t = Table::default();
        let mut dsl = Dsl::None;
        for (i, item) in m.items.iter().enumerate() {
            match item {
                Item::ExtendConcern => t.concern = true,
                Item::ExtendDeclarative => {
                    if dsl < Dsl::Declarative {
                        dsl = Dsl::Declarative;
                    }
                }
                Item::Include(refs) => {
                    let resolved: Vec<Option<usize>> = refs.iter().map(|r| self.resolve(r, &m.nesting)).collect();
                    for k in resolved.iter().flatten() {
                        self.build(*k, tables, stack);
                    }
                    // `include A, B` appends B, then A.
                    for (r, k) in refs.iter().zip(&resolved).rev() {
                        let flat = match k {
                            Some(k) => {
                                let out = self.include_module(&mut t, *k, false, tables)?;
                                if out.provider {
                                    dsl = Dsl::SharedTests;
                                }
                                if t.deps.contains(k) {
                                    Vec::new()
                                } else {
                                    let mut flat = flat_passthrough(*k, tables);
                                    flat.push(OutInclude::Spliced(*k));
                                    flat
                                }
                            }
                            None => vec![OutInclude::Module(self.qualify(r, &m.nesting))],
                        };
                        for inc in flat {
                            if !t.passthrough.contains(&inc) {
                                t.passthrough.push(inc);
                            }
                        }
                    }
                }
                Item::Test { method_name, dsl: is_dsl, tags_arg, at, .. } => {
                    if *is_dsl {
                        match dsl {
                            Dsl::None => {
                                return Err(format!(
                                    "the `test` {at} of `{}` is Kernel#test, which raises when the file loads: the module neither includes SharedTests nor extends ActiveSupport::Testing::Declarative",
                                    m.name
                                ))
                            }
                            Dsl::Declarative if *tags_arg => {
                                return Err(format!(
                                    "the `test` {at} of `{}` passes tags, which ActiveSupport::Testing::Declarative#test does not take (it raises when the file loads)",
                                    m.name
                                ))
                            }
                            _ => {}
                        }
                        if defined_in(&t, method_name, tables) {
                            return Err(format!(
                                "{method_name} is already defined in {}: ActiveSupport raises this when the module body loads",
                                m.name
                            ));
                        }
                    }
                    t.set(method_name, Slot::Own(i));
                }
                Item::Setup(body, at) | Item::Teardown(body, at) => {
                    if dsl != Dsl::SharedTests {
                        return Err(format!(
                            "the `setup`/`teardown` {at} of `{}` is not SharedTests' (the module does not include it), so it raises when the file loads",
                            m.name
                        ));
                    }
                    if matches!(item, Item::Setup(..)) {
                        t.setup_fn = Some(body.clone());
                    } else {
                        t.teardown_fn = Some(body.clone());
                    }
                }
                Item::Tag(at) => {
                    if dsl != Dsl::SharedTests {
                        return Err(format!(
                            "the `tag` {at} of `{}` is TestTags', which the module does not have (it raises when the file loads)",
                            m.name
                        ));
                    }
                }
                Item::IncludedBlock(at) => {
                    if !t.concern {
                        return Err(format!(
                            "the `included do` {at} of `{}` is not an ActiveSupport::Concern's, which is not modeled",
                            m.name
                        ));
                    }
                }
            }
        }
        // A SharedTests module that is a Concern defers SharedTests itself:
        // it is not extended with `ClassMethods`, so it has no hook.
        if m.provider {
            t.concern = true;
        }
        Ok(t)
    }

    /// `target.include(k)`: ancestry, `Concern` dependencies and the
    /// `included` hook's wrappers.
    fn include_module(
        &self,
        target: &mut Table,
        k: usize,
        target_is_class: bool,
        tables: &Tables,
    ) -> Result<Included, String> {
        let kt = match table_of(tables, k) {
            Ok(kt) => kt,
            Err(why) => return Err(format!("includes shared tests `{}`, not modeled: {why}", self.mixins[k].name)),
        };
        let mut out = Included::default();
        // Concern#append_features on a Concern: deferred to its includer.
        if kt.concern && target.concern && !target_is_class {
            if !target.deps.contains(&k) {
                target.deps.push(k);
            }
            return Ok(out);
        }
        if kt.concern {
            // `return false if base < self`: already an ancestor, through
            // the class's own includes or its superclass's.
            if target.mro.contains(&k) || target.inherited.contains(&k) {
                return Ok(out);
            }
            for &d in &kt.deps {
                let inner = self.include_module(target, d, target_is_class, tables)?;
                out.blocks.extend(inner.blocks);
                out.provider |= inner.provider;
            }
        }
        let mut chain = vec![k];
        chain.extend(kt.mro.iter().copied());
        let fresh: Vec<usize> = chain.into_iter().filter(|m| !target.mro.contains(m)).collect();
        let mut mro = fresh;
        mro.extend(target.mro.iter().copied());
        target.mro = mro;
        if self.mixins[k].provider {
            out.provider = true;
            if !target_is_class {
                target.hook = true;
            }
        }
        if kt.hook {
            for name in instance_test_names(k, tables) {
                target.set(&name, Slot::Wrapper(k));
            }
        }
        if let Some(_offset) = self.mixins[k].included_block {
            if !target_is_class {
                return Err(format!(
                    "the `included do` block of `{}` would run on a module, which is not modeled",
                    self.mixins[k].name
                ));
            }
            out.blocks.push(k);
        }
        Ok(out)
    }

    /// Splice into `tm` the tests, helpers and constants it gets from the
    /// shared test modules it includes. `source` is the class's file.
    /// A class that cannot be built is refused by name.
    pub(super) fn splice(&self, tm: &mut TestModule, source: &[u8], file: &str) -> IngestResult<()> {
        let refuse = |why: String| IngestError::Unsupported {
            file: file.into(),
            message: format!("test class `{}` refused: {why}", tm.name.0),
        };
        let result = super::prism::parse_silent(source);
        let root = result.node();
        let Some((class, nesting)) = find_class(&root, &[], tm.name.0.as_str()) else {
            return Ok(());
        };
        let superclass = class.superclass().and_then(|n| IncludeRef::of(&n));
        let ancestry = self.ancestry(superclass.as_ref(), &nesting[1..]);
        let stmts = class.body().map(flatten_statements).unwrap_or_default();
        let own_modules = self.module_teardowns(&body_includes(&stmts), &nesting);
        if !self.may_include_shared(tm) {
            // A subclass of a spliced class: only its teardown matters.
            let own = class_def_teardown(tm).map(|b| ends_in_super(&b));
            return check_teardown_chain(tm.teardown_blocks, own, &own_modules, &ancestry).map_err(refuse);
        }
        let mut walk = ClassWalk {
            reg: self,
            tables: &self.tables,
            class: ClassId(tm.name.0.clone()),
            file: file.to_string(),
            nesting,
            t: Table { inherited: ancestry.mixins.clone(), ..Table::default() },
            own: Vec::new(),
            base_tags: TestTags::default(),
            tags: TestTags::default(),
            setups: Vec::new(),
            block_setup: false,
            teardowns: Vec::new(),
            block_teardown: false,
            closure_setups: Vec::new(),
            closure_teardowns: Vec::new(),
            block_helpers: Vec::new(),
            block_statements: Vec::new(),
            class_defs: Vec::new(),
            block_wins: Vec::new(),
            includes_out: Vec::new(),
            used: Vec::new(),
            block_owner: None,
            block_param: None,
        };
        walk.walk(stmts, file, false, Some(&class)).map_err(refuse)?;
        let own = walk.def_teardown(tm).map(|b| ends_in_super(&b));
        check_teardown_chain(!walk.teardowns.is_empty(), own, &own_modules, &ancestry).map_err(refuse)?;
        if walk.used.is_empty() {
            return Ok(());
        }
        let used = walk.used.clone();
        let out = walk.finish(tm).map_err(refuse)?;
        *tm = out;
        // Statements of the modules whose tests are not modeled.
        for k in used {
            if !self.reported.borrow_mut().insert(k) {
                continue;
            }
            let m = &self.mixins[k];
            for gap in &m.gaps {
                super::survey::unwrap_or_record::<()>(Err(IngestError::Unsupported {
                    file: m.file.clone(),
                    message: format!("shared test module `{}`: {gap}", m.name),
                }))?;
            }
        }
        Ok(())
    }
}

#[derive(Default)]
struct Included {
    /// Concerns whose `included do` block now runs on the class.
    blocks: Vec<usize>,
    /// `SharedTests` itself was included.
    provider: bool,
}

/// A test the class declares (in its body or in a concern's `included do`).
struct OwnTest {
    /// Its name the way `ingest_test_class` computed it, without what
    /// concerns contribute to the class's tags.
    base: String,
    /// Declared by a concern's block: not in `tm.tests`.
    block_test: Option<Test>,
}

struct ClassWalk<'a> {
    reg: &'a TestMixins,
    tables: &'a Tables,
    class: ClassId,
    /// The class's file.
    file: String,
    nesting: Vec<String>,
    t: Table,
    own: Vec<OwnTest>,
    base_tags: TestTags,
    tags: TestTags,
    setups: Vec<Expr>,
    block_setup: bool,
    /// Each `teardown do … end` callback the class registers, in the
    /// order Rails registers them: its body's where they stand, and a
    /// concern's `included do` block's where its include stands.
    teardowns: Vec<Expr>,
    /// One of `teardowns` comes from an `included do` block.
    block_teardown: bool,
    /// The `setups` / `teardowns` (by index) of an `included do` block that
    /// read one of the block's locals: each is a closure natively, so it is
    /// defined where the locals are (`closure_callback`).
    closure_setups: Vec<usize>,
    closure_teardowns: Vec<usize>,
    /// The methods `included do` blocks define on the class, each the
    /// last of its name, and only while no later class-body `def`
    /// replaced it.
    block_helpers: Vec<MethodDef>,
    /// The statements of `included do` blocks that declare tests ingest
    /// cannot list (a loop over a computed collection): Ruby runs them
    /// on the class where the `include` stands, so the emitted class runs
    /// them as it loads (`TestModule::tests_at_load`).
    block_statements: Vec<crate::dialect::TestBodyStatement>,
    /// The methods the class body has `def`ined so far, as (name,
    /// receiver): `def self.label` and `def label` are different methods.
    class_defs: Vec<(String, MethodReceiver)>,
    /// The class-body defs a later `included do` block's def replaced:
    /// the block's definition is the class's (`ingest_test_class` kept
    /// the class body's in `tm.helpers`).
    block_wins: Vec<(String, MethodReceiver)>,
    /// What the emitted class includes, in order (`OutInclude`).
    includes_out: Vec<OutInclude>,
    /// Every shared test module included, directly or not.
    used: Vec<usize>,
    /// The module whose `included do` block is running: its statements
    /// read constants in that module's lexical scope.
    block_owner: Option<usize>,
    /// The name the running `included do |base|` block gives the class, and
    /// whether a carried statement already bound it (`base = self`).
    block_param: Option<(String, bool)>,
}

impl ClassWalk<'_> {
    /// The class body's statements (`in_block`: a concern's `included do`
    /// block's, run on the class at the include; `class_node` is the
    /// class when they are its body).
    fn walk<'pr>(
        &mut self,
        stmts: Vec<Node<'pr>>,
        file: &str,
        in_block: bool,
        class_node: Option<&ruby_prism::ClassNode<'pr>>,
    ) -> Result<(), String> {
        let consts = TagConstants::of(&stmts);
        let mut items = Vec::new();
        class_body_items(stmts, file, &consts, &mut items).map_err(message)?;
        let mut saved: Vec<(Vec<(String, String)>, Vec<(String, String)>)> = Vec::new();
        // The default visibility of the block's defs: `class_eval` starts
        // public, and a bare `private` there makes the later defs private.
        let mut visibility = MethodVisibility::Public;
        for item in items {
            let stmt = match item {
                BodyItem::Stmt(stmt) => stmt,
                BodyItem::TagsBegin(tags) => {
                    saved.push((self.base_tags.tags.clone(), self.tags.tags.clone()));
                    if !in_block {
                        merge_tags(&mut self.base_tags.tags, &tags);
                    }
                    merge_tags(&mut self.tags.tags, &tags);
                    continue;
                }
                BodyItem::TagsEnd => {
                    let (base, tags) = saved.pop().unwrap_or_default();
                    self.base_tags.tags = base;
                    self.tags.tags = tags;
                    continue;
                }
            };
            if let Some((tags, None)) = tag_declaration(&stmt, file, &consts).map_err(message)? {
                if !in_block {
                    merge_tags(&mut self.base_tags.tags, &tags);
                    self.base_tags.persistent = true;
                }
                merge_tags(&mut self.tags.tags, &tags);
                self.tags.persistent = true;
                continue;
            }
            if is_test_declaration_shape(&stmt) {
                let Some(test) = ingest_test_declaration(&stmt, file, &self.tags, &consts).map_err(message)? else { continue };
                if test.toggle_flags {
                    continue;
                }
                let base = if in_block {
                    String::new()
                } else if self.base_tags.tags == self.tags.tags {
                    test.method_name.clone()
                } else {
                    ingest_test_declaration(&stmt, file, &self.base_tags, &consts)
                        .map_err(message)?
                        .map(|t| t.method_name)
                        .unwrap_or_default()
                };
                let is_def = stmt.as_def_node().is_some();
                if !is_def {
                    self.refuse_redeclared(&test.method_name)?;
                }
                let name = test.method_name.clone();
                let block_test = in_block.then(|| {
                    let mut test = test;
                    self.qualify_block(&mut test.body);
                    test
                });
                self.own.push(OwnTest { base, block_test });
                self.t.set(&name, Slot::Own(self.own.len() - 1));
                continue;
            }
            // A literal loop's tests (`test_unroll`), each declared by
            // ActiveSupport's `test` where the loop stands.
            if let Some(class_node) = class_node.filter(|_| !in_block) {
                let unrolled = |tags: &TestTags| {
                    statement_tests(&stmt, class_node, &self.class, file, tags, &consts).map_err(message)
                };
                let declared = unrolled(&self.tags)?;
                if !declared.is_empty() {
                    let bases: Vec<String> = if self.base_tags.tags == self.tags.tags {
                        declared.iter().map(|t| t.method_name.clone()).collect()
                    } else {
                        unrolled(&self.base_tags)?.into_iter().map(|t| t.method_name).collect()
                    };
                    for (test, base) in declared.into_iter().zip(bases) {
                        if test.toggle_flags {
                            continue;
                        }
                        self.refuse_redeclared(&test.method_name)?;
                        let name = test.method_name;
                        self.own.push(OwnTest { base, block_test: None });
                        self.t.set(&name, Slot::Own(self.own.len() - 1));
                    }
                    continue;
                }
            }
            if let Some(call) = stmt.as_call_node() {
                if call.receiver().is_none() {
                    match constant_id_str(&call.name()) {
                        "include" => {
                            let args: Vec<Node<'_>> = call.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default();
                            let refs: Vec<IncludeRef> = args.iter().filter_map(IncludeRef::of).collect();
                            if in_block && refs.len() != args.len() {
                                return Err(format!(
                                    "the `included do` block {} includes a module that is not a constant, which the shared-test splice does not model",
                                    at(file, stmt.location().start_offset())
                                ));
                            }
                            self.include(refs)?;
                            continue;
                        }
                        "setup" if in_block && call.block().and_then(|b| b.as_block_node()).is_some() && is_prepend_option(&call) => {
                            // `setup(prepend: true) do … end` in an `included do`
                            // block: run on the class where the include stands,
                            // the runtime's `RoundhouseTestCallbacks` chain
                            // putting it before the callbacks registered earlier.
                            match super::test::carried_body_statement(&stmt, &self.class, file).map_err(message)? {
                                Some(mut expr) => {
                                    self.qualify_block(&mut expr);
                                    self.block_statements.push(crate::dialect::TestBodyStatement {
                                        expr,
                                        line: super::sources::line_at(file, stmt.location().start_offset()).unwrap_or(0),
                                        after_definitions: false,
                                        after_setup_blocks: false,
                                        after_teardown_blocks: false,
                                        source: None,
                                    });
                                    continue;
                                }
                                None => {
                                    return Err(format!(
                                        "the `setup(prepend: true)` {} is a statement ingest does not model",
                                        at(file, stmt.location().start_offset())
                                    ))
                                }
                            }
                        }
                        "setup" if call.block().is_some() => {
                            if call.arguments().is_some() || call.block().and_then(|b| b.as_block_node()).is_none() {
                                return Err(format!(
                                    "the `setup` {} passes arguments or a block argument (`prepend: true` runs it before the callbacks registered earlier), which the shared-test splice does not model",
                                    at(file, stmt.location().start_offset())
                                ));
                            }
                            if let Some(mut body) = ingest_setup_declaration(&stmt, file).map_err(message)? {
                                self.qualify_block(&mut body);
                                if in_block && reads_block_local(&body) {
                                    self.closure_setups.push(self.setups.len());
                                    self.bind_block_param(&body, file, 0);
                                }
                                self.setups.push(body);
                                self.block_setup |= in_block;
                            }
                            continue;
                        }
                        // `setup :name, …`: each a callback that calls the
                        // method, as `set_callback` registers a symbol.
                        "setup" => {
                            let args: Vec<Node<'_>> = call.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default();
                            let names: Vec<String> = args
                                .iter()
                                .filter_map(|a| a.as_symbol_node())
                                .map(|n| String::from_utf8_lossy(n.unescaped()).into_owned())
                                .collect();
                            if args.is_empty() || names.len() != args.len() {
                                return Err(format!(
                                    "the `setup` {} is not `setup do … end` or `setup :name, …`, which the shared-test splice does not model",
                                    at(file, stmt.location().start_offset())
                                ));
                            }
                            for name in names {
                                self.setups.push(self::call(&name));
                            }
                            // `ingest_test_class` kept no callback for it:
                            // the class's setup is rebuilt from `setups`.
                            self.block_setup = true;
                            continue;
                        }
                        // `teardown :name` and other forms are refused by
                        // `ingest_teardown_block` (the class body's already
                        // were, by `ingest_test_class`).
                        "teardown" => {
                            if let Some(mut body) = ingest_teardown_block(&stmt, file).map_err(message)? {
                                self.qualify_block(&mut body);
                                if in_block && reads_block_local(&body) {
                                    self.closure_teardowns.push(self.teardowns.len());
                                    self.bind_block_param(&body, file, 0);
                                }
                                self.teardowns.push(body);
                                self.block_teardown |= in_block;
                            }
                            continue;
                        }
                        _ => {}
                    }
                }
            }
            // A `def` replaces the class's method of that name, whichever
            // ran first: the class body's or an `included do` block's
            // (which `class_eval` runs on the class where the include
            // stands).
            if !in_block {
                if let Some(key) = body_def_key(&stmt) {
                    self.block_helpers.retain(|m| !(m.name.as_str() == key.0 && m.receiver == key.1));
                    self.block_wins.retain(|k| *k != key);
                    if !self.class_defs.contains(&key) {
                        self.class_defs.push(key);
                    }
                }
            }
            if in_block {
                if let Some(level) = bare_visibility(&stmt) {
                    visibility = level;
                    continue;
                }
                let def = match stmt.as_def_node() {
                    Some(def) => Some((def, None)),
                    None => instance_def(&stmt).map(|def| (def, stmt.as_call_node().and_then(|c| visibility_of(constant_id_str(&c.name()))))),
                };
                if let Some((def, written)) = def {
                    let mut method = ingest_library_method(&def, &self.class, file).map_err(message)?;
                    if method.receiver == MethodReceiver::Instance {
                        method.visibility = written.unwrap_or(visibility);
                    }
                    qualify_method(&mut method, &|e| self.qualify_block(e));
                    let key = (method.name.to_string(), method.receiver);
                    if self.class_defs.contains(&key) && !self.block_wins.contains(&key) {
                        self.block_wins.push(key);
                    }
                    self.block_helpers.retain(|m| !(m.name == method.name && m.receiver == method.receiver));
                    self.block_helpers.push(method);
                    continue;
                }
                if hidden_test_declaration(&stmt).is_some() {
                    // `class_eval` runs it on the class where the include
                    // stands: carried, declared as the class loads.
                    return match super::test::carried_body_statement(&stmt, &self.class, file).map_err(message)? {
                        Some(mut expr) => {
                            self.qualify_block(&mut expr);
                            self.block_statements.push(crate::dialect::TestBodyStatement {
                                expr,
                                line: super::sources::line_at(file, stmt.location().start_offset()).unwrap_or(0),
                                after_definitions: true,
                                after_setup_blocks: false,
                                after_teardown_blocks: false,
                                source: None,
                            });
                            continue;
                        }
                        None => Err(format!(
                            "a test declared {} by an `included do` statement ingest does not model",
                            at(file, stmt.location().start_offset())
                        )),
                    };
                }
                if is_extend_declarative(&stmt) {
                    // Every test class already has it (`ActiveSupport::TestCase`).
                    continue;
                }
                if is_factory_registration(&stmt) {
                    // FixtureFactory's `define_factories { … }`, alone or
                    // under an `if fixture_factory_definitions.empty?`
                    // guard: `class_eval` runs it on the class where the
                    // include stands, and the emitted class runs the same
                    // class method (`FixtureFactory::Registry::ClassMethods`)
                    // as it loads.
                    if let Some(mut expr) = super::test::carried_body_statement(&stmt, &self.class, file).map_err(message)? {
                        self.qualify_block(&mut expr);
                        self.block_statements.push(crate::dialect::TestBodyStatement {
                            expr,
                            line: super::sources::line_at(file, stmt.location().start_offset()).unwrap_or(0),
                            after_definitions: false,
                            after_setup_blocks: false,
                            after_teardown_blocks: false,
                            source: None,
                        });
                        continue;
                    }
                }
                // Any other class macro (`use_active_record_fixtures`,
                // `let(:x) do … end`, `documentation(…) do … end`) or a local
                // the macros after it read: `class_eval` runs it on the class
                // where the include stands, and the emitted class runs the
                // same statement as it loads. A constant or a nested class
                // or module would be defined in the module's lexical scope
                // (the block's), not the class's, so those stay refused.
                if carries_in_included_block(&stmt) {
                    if let Some(mut expr) = super::test::carried_body_statement(&stmt, &self.class, file).map_err(message)? {
                        self.qualify_block(&mut expr);
                        let line = super::sources::line_at(file, stmt.location().start_offset()).unwrap_or(0);
                        self.bind_block_param(&expr, file, line);
                        self.block_statements.push(crate::dialect::TestBodyStatement {
                            expr,
                            line,
                            after_definitions: false,
                            after_setup_blocks: false,
                            after_teardown_blocks: false,
                            source: Some(file.to_string()),
                        });
                    }
                    continue;
                }
                let text = String::from_utf8_lossy(stmt.location().as_slice()).into_owned();
                let first = text.lines().next().unwrap_or_default();
                let shown: String = first.chars().take(60).collect();
                return Err(format!(
                    "the `included do` block {} runs `{shown}{}` on the class, which the shared-test splice does not model",
                    at(file, stmt.location().start_offset()),
                    if shown.len() < text.len() { "…" } else { "" }
                ));
            }
        }
        Ok(())
    }

    /// A statement of `included do |base| … end` that reads `base` (the
    /// class) is carried with a local `base = self` before it, in the class
    /// body's scope as the block's own is: the first such statement of
    /// the block binds it, and the statements after it (and the blocks
    /// they pass) read it.
    fn bind_block_param(&mut self, expr: &Expr, file: &str, line: u32) {
        let Some((name, bound)) = self.block_param.as_mut() else { return };
        if *bound || !reads_var(expr, name) {
            return;
        }
        *bound = true;
        let span = expr.span;
        let target = crate::expr::LValue::Var { id: crate::ident::VarId(0), name: Symbol::from(name.as_str()) };
        let value = Expr::new(span, ExprNode::SelfRef);
        self.block_statements.push(crate::dialect::TestBodyStatement {
            expr: Expr::new(span, ExprNode::Assign { target, value }),
            line,
            after_definitions: false,
            after_setup_blocks: false,
            after_teardown_blocks: false,
            source: Some(file.to_string()),
        });
    }

    /// The body of the `def teardown` the class itself ends up with: an
    /// `included do` block's when no later class-body def replaced it,
    /// else the class body's own.
    fn def_teardown(&self, tm: &TestModule) -> Option<Expr> {
        match self.block_helpers.iter().find(|m| m.name.as_str() == "teardown" && m.receiver == MethodReceiver::Instance) {
            Some(m) => Some(m.body.clone()),
            None => class_def_teardown(tm),
        }
    }

    /// ActiveSupport's `test` of a name the class already has, from a
    /// shared test module it includes, raises as the body loads.
    fn refuse_redeclared(&self, method_name: &str) -> Result<(), String> {
        if !defined_in(&self.t, method_name, self.tables) {
            return Ok(());
        }
        let by = self.t.mro.iter().find(|&&m| table_of(self.tables, m).is_ok_and(|t| t.slot(method_name).is_some()));
        let by = by.map(|&m| format!(" (by shared tests `{}`)", self.reg.mixins[m].name)).unwrap_or_default();
        Err(format!(
            "{method_name} is already defined in {}{by}: ActiveSupport raises this when the class body loads",
            self.class.0
        ))
    }

    /// A statement of an `included do` block reads constants in its
    /// module's lexical scope (`class_eval` keeps the block's), not the
    /// class's (`qualify_lexical`); the class body's own are as written.
    fn qualify_block(&self, e: &mut Expr) {
        if let Some(k) = self.block_owner {
            let m = &self.reg.mixins[k];
            qualify_lexical(e, &m.nesting, &m.ancestry, &m.own_copied(), &|name| loaded(self.reg, &self.file, &self.used, name));
        }
    }

    fn include(&mut self, refs: Vec<IncludeRef>) -> Result<(), String> {
        // An `include` in an `included do` block names its module in that
        // module's lexical scope.
        let nesting = match self.block_owner {
            Some(k) => self.reg.mixins[k].nesting.clone(),
            None => self.nesting.clone(),
        };
        let resolved: Vec<Option<usize>> = refs.iter().map(|r| self.reg.resolve(r, &nesting)).collect();
        // The emitted class includes each module after the last: `include
        // A, B` puts A first in the lookup, so it is written last.
        for (r, k) in refs.iter().zip(&resolved).rev() {
            let flat = match k {
                Some(k) => match table_of(self.tables, *k) {
                    Ok(_) => {
                        let mut flat = flat_passthrough(*k, self.tables);
                        flat.push(OutInclude::Spliced(*k));
                        flat
                    }
                    Err(why) => {
                        return Err(format!("includes shared tests `{}`, not modeled: {why}", self.reg.mixins[*k].name))
                    }
                },
                None => {
                    let written = match self.block_owner {
                        Some(_) if !r.rooted => nesting
                            .iter()
                            .find(|s| loaded(self.reg, &self.file, &self.used, &format!("{s}::{}", r.path[0])))
                            .map(|s| format!("::{s}::{}", r.written()))
                            .unwrap_or_else(|| r.written()),
                        _ => r.written(),
                    };
                    vec![OutInclude::Module(written)]
                }
            };
            for inc in flat {
                if !self.includes_out.contains(&inc) {
                    self.includes_out.push(inc);
                }
            }
        }
        for k in resolved.into_iter().rev().flatten() {
            let mut t = std::mem::take(&mut self.t);
            let result = self.reg.include_module(&mut t, k, true, self.tables);
            self.t = t;
            let out = result?;
            for &m in &self.t.mro {
                if !self.used.contains(&m) {
                    self.used.push(m);
                }
            }
            if !self.used.contains(&k) {
                self.used.push(k);
            }
            for block in out.blocks {
                self.run_included_block(block)?;
            }
        }
        Ok(())
    }

    /// A concern's `included do` block, run on the class.
    fn run_included_block(&mut self, k: usize) -> Result<(), String> {
        let m = &self.reg.mixins[k];
        let (Some(offset), Some(source)) = (m.included_block, self.reg.sources.get(&m.file)) else {
            return Ok(());
        };
        if !self.used.contains(&k) {
            self.used.push(k);
        }
        let result = super::prism::parse_silent(source);
        let root = result.node();
        let Some(call) = find_call(&root, offset) else { return Ok(()) };
        let stmts = call
            .block()
            .and_then(|b| b.as_block_node())
            .and_then(|b| b.body())
            .map(flatten_statements)
            .unwrap_or_default();
        let file = m.file.clone();
        let param = call
            .block()
            .and_then(|b| b.as_block_node())
            .and_then(|b| b.parameters())
            .and_then(|p| p.as_block_parameters_node())
            .and_then(|p| p.parameters())
            .and_then(|p| p.requireds().iter().next())
            .and_then(|p| p.as_required_parameter_node().map(|r| constant_id_str(&r.name()).to_string()))
            .map(|name| (name, false));
        let outer = self.block_owner.replace(k);
        let outer_param = std::mem::replace(&mut self.block_param, param);
        let walked = self.walk(stmts, &file, true, None);
        self.block_owner = outer;
        self.block_param = outer_param;
        walked
    }

    /// The class as Minitest sees it: its tests under their method names.
    fn finish(self, tm: &TestModule) -> Result<TestModule, String> {
        let ClassWalk {
            reg,
            tables,
            class,
            mut t,
            own,
            base_tags,
            tags,
            setups,
            block_setup,
            teardowns,
            block_teardown,
            closure_setups,
            closure_teardowns,
            mut block_helpers,
            mut block_statements,
            block_wins,
            includes_out,
            file,
            used,
            ..
        } = self;
        // A body spliced out of module `m` reads constants in its lexical
        // scope (`qualify_lexical`), as loaded where the class runs.
        let q = |m: usize, e: &mut Expr| {
            let mixin = &reg.mixins[m];
            qualify_lexical(e, &mixin.nesting, &mixin.ancestry, &mixin.own_copied(), &|name| loaded(reg, &file, &used, name));
        };
        let table = |m: usize| -> &Table { table_of(tables, m).expect("an included module's table is built") };
        for &m in &t.mro {
            let mixin = &reg.mixins[m];
            if let Some(why) = reg.uncarried.borrow().get(&mixin.file).filter(|_| mixin.real) {
                return Err(format!(
                    "shared tests `{}` keep their own classes, modules and state in {}, which was not carried as a library file: {why}",
                    mixin.name, mixin.file
                ));
            }
        }
        let (mut class_helpers, teardown_def) =
            merge_teardowns(tm, teardowns, &closure_teardowns, &mut block_helpers, &block_wins, &class, &mut block_statements)?;
        let (setup, setup_frames) = frame_setups(setups, block_setup, &closure_setups, &class, &mut block_statements)?;
        class_helpers.extend(setup_frames);
        // TestTags' persistent hook renames what the class itself defines,
        // and undefines the old names, so an ancestor's are hidden too.
        let originals: Vec<String> = t.slots.iter().map(|(n, _)| n.clone()).collect();
        let hidden: Set<String> = originals.iter().cloned().collect();
        if tags.persistent && !tags.tags.is_empty() {
            for (name, _) in &mut t.slots {
                if !name.contains("tag:") {
                    *name = name_with_tags(name, &tags.tags);
                }
            }
        }
        let rename_base = |name: &str| -> String {
            if base_tags.persistent && !base_tags.tags.is_empty() && !name.contains("tag:") {
                name_with_tags(name, &base_tags.tags)
            } else {
                name.to_string()
            }
        };

        // Helpers the wrappers call: `respond_to?` answers for the first
        // definition in the lookup, and only when it is public.
        let mut first: Map<String, bool> = Map::default();
        {
            let mut note = |name: String, public: bool| {
                first.entry(name).or_insert(public);
            };
            let public = |h: &MethodDef| h.visibility == MethodVisibility::Public;
            for h in class_helpers.iter().chain(block_helpers.iter()).filter(|h| h.receiver == MethodReceiver::Instance) {
                note(h.name.to_string(), public(h));
            }
            for &m in &t.mro {
                let mt = table(m);
                for h in &reg.mixins[m].helpers {
                    note(h.name.to_string(), public(h));
                }
                // `define_method` from `SharedTests.setup`: public.
                let prefix = method_prefix(&reg.mixins[m].name);
                if mt.setup_fn.is_some() {
                    note(format!("{prefix}_setup"), true);
                }
                if mt.teardown_fn.is_some() {
                    note(format!("{prefix}_teardown"), true);
                }
            }
        }
        let defined: Set<String> = first.into_iter().filter(|(_, public)| *public).map(|(name, _)| name).collect();
        let wrap = |k: usize, inner: Expr| -> Expr {
            let prefix = method_prefix(&reg.mixins[k].name);
            let (setup, teardown) = (format!("{prefix}_setup"), format!("{prefix}_teardown"));
            let (has_setup, has_teardown) = (defined.contains(&setup), defined.contains(&teardown));
            if !has_setup && !has_teardown {
                return inner;
            }
            let mut exprs = Vec::new();
            if has_setup {
                exprs.push(call(&setup));
            }
            match &*inner.node {
                ExprNode::Seq { exprs: inner_exprs } => exprs.extend(inner_exprs.iter().cloned()),
                _ => exprs.push(inner),
            }
            let body = Expr::new(Span::synthetic(), ExprNode::Seq { exprs });
            if !has_teardown {
                return body;
            }
            Expr::new(
                Span::synthetic(),
                ExprNode::BeginRescue { body, rescues: Vec::new(), else_branch: None, ensure: Some(call(&teardown)), implicit: false },
            )
        };
        // `super()` from a wrapper: the next definition in the ancestry.
        fn resolve(
            name: &str,
            mro: &[usize],
            reg: &TestMixins,
            tables: &Tables,
            wrap: &dyn Fn(usize, Expr) -> Expr,
            q: &dyn Fn(usize, &mut Expr),
        ) -> Option<(String, Expr)> {
            for (pos, &m) in mro.iter().enumerate() {
                match table_of(tables, m).ok()?.slot(name) {
                    None => continue,
                    Some(Slot::Own(i)) => {
                        let Item::Test { name, body, .. } = &reg.mixins[m].items[i] else { return None };
                        let mut body = body.clone();
                        q(m, &mut body);
                        return Some((name.clone(), body));
                    }
                    Some(Slot::Wrapper(k)) => {
                        let (display, inner) = resolve(name, &mro[pos + 1..], reg, tables, wrap, q)?;
                        return Some((display, wrap(k, inner)));
                    }
                }
            }
            None
        }

        let mut tests: Vec<Test> = Vec::new();
        let mut seen: Set<String> = Set::default();
        for ((name, slot), original) in t.slots.iter().zip(originals.iter()) {
            // The persistent rename aliased an earlier test onto a name a
            // later one, declared under the tag, has: the alias replaced
            // it, so the earlier test is the one left (as in
            // `ingest_test_class`).
            if seen.contains(name) {
                continue;
            }
            let test = match slot {
                Slot::Own(j) => {
                    let o = &own[*j];
                    match &o.block_test {
                        Some(test) => Test { method_name: name.clone(), ..test.clone() },
                        None => {
                            let base = rename_base(&o.base);
                            let Some(found) = tm.tests.iter().find(|t| !t.toggle_flags && t.method_name == base) else {
                                return Err(format!("its test {base} is not where the shared-test splice expects it"));
                            };
                            Test { method_name: name.clone(), ..found.clone() }
                        }
                    }
                }
                Slot::Wrapper(k) => {
                    let Some((display, inner)) = resolve(original, &t.mro, reg, tables, &wrap, &q) else {
                        return Err(format!("the wrapper SharedTests defines for {original} has no test to call"));
                    };
                    Test { name: display, body: wrap(*k, inner), method_name: name.clone(), toggle_flags: false }
                }
            };
            seen.insert(name.clone());
            tests.push(test);
        }
        for (pos, &m) in t.mro.iter().enumerate() {
            for (name, _) in &table(m).slots {
                if hidden.contains(name) || seen.contains(name) {
                    continue;
                }
                let Some((display, body)) = resolve(name, &t.mro[pos..], reg, tables, &wrap, &q) else { continue };
                seen.insert(name.clone());
                tests.push(Test { name: display, body, method_name: name.clone(), toggle_flags: false });
            }
        }
        tests.extend(tm.tests.iter().filter(|t| t.toggle_flags).cloned());
        for test in &tests {
            if calls_super(&test.body) {
                return Err(format!("its test {} calls `super`, which splicing the shared tests does not model", test.method_name));
            }
        }

        // Helpers: the class's own, then a concern block's, then each module's
        // in lookup order; the first definition of a name is the class's.
        // One that calls `super` reaches the next definition: that one is
        // kept in a module of its own the class includes, in the same
        // lookup order, so the chain runs as it does natively. A library
        // module (`Mixin::real`) keeps its own copies too (`library_member`)
        // for what reads the module itself; the class's are these, lowered
        // as its test code, and its `RoundhouseSuper…` comes before the
        // module in the lookup, so a `super` never reaches the module's.
        let mut helpers = class_helpers.clone();
        // Keyed by (name, receiver): `def self.label` is not `def label`.
        type Chains = Vec<((String, MethodReceiver), Vec<(Option<usize>, MethodDef)>)>;
        let mut chains: Chains = Vec::new();
        let add = |owner: Option<usize>, h: MethodDef, chains: &mut Chains| {
            let key = (h.name.to_string(), h.receiver);
            match chains.iter_mut().find(|(k, _)| *k == key) {
                Some((_, defs)) => defs.push((owner, h)),
                None => chains.push((key, vec![(owner, h)])),
            }
        };
        for h in &class_helpers {
            add(None, h.clone(), &mut chains);
        }
        // A concern block's defs are the class's own (`class_eval` ran
        // them on it), unless a later class-body def replaced them.
        for h in block_helpers {
            if !class_helpers.iter().any(|own| own.name == h.name && own.receiver == h.receiver) {
                helpers.push(h.clone());
                add(None, h, &mut chains);
            }
        }
        for &m in &t.mro {
            let mixin = &reg.mixins[m];
            for h in &mixin.helpers {
                let mut h = h.clone();
                qualify_method(&mut h, &|e| q(m, e));
                add(Some(m), h, &mut chains);
            }
            let prefix = method_prefix(&mixin.name);
            let mt = table(m);
            for (suffix, body) in [("setup", &mt.setup_fn), ("teardown", &mt.teardown_fn)] {
                if let Some(body) = body {
                    let mut body = body.clone();
                    q(m, &mut body);
                    add(Some(m), synth_method(&format!("{prefix}_{suffix}"), body, &class), &mut chains);
                }
            }
        }
        let mut supers: Vec<(usize, Vec<MethodDef>)> = Vec::new();
        for ((name, receiver), defs) in chains {
            if receiver == MethodReceiver::Instance && seen.contains(&name) {
                continue;
            }
            let (winner_owner, winner) = &defs[0];
            if winner_owner.is_some() {
                let mut h = winner.clone();
                h.enclosing_class = Some(class.0.clone());
                helpers.push(h);
            }
            let mut i = 0;
            while calls_super(&defs[i].1.body) && i + 1 < defs.len() {
                i += 1;
                let (Some(owner), def) = &defs[i] else { break };
                match supers.iter_mut().find(|(m, _)| m == owner) {
                    Some((_, list)) => list.push(def.clone()),
                    None => supers.push((*owner, vec![def.clone()])),
                }
            }
        }
        let mut inner_classes = tm.inner_classes.clone();
        let mut super_includes: Vec<(usize, String)> = Vec::new();
        for &m in t.mro.iter().rev() {
            let Some((_, defs)) = supers.iter().find(|(o, _)| *o == m) else { continue };
            let short = format!("RoundhouseSuper{}", reg.mixins[m].name.replace("::", "_"));
            let full = ClassId(Symbol::from(format!("{}::{short}", class.0).as_str()));
            let methods = defs
                .iter()
                .cloned()
                .map(|mut d| {
                    d.enclosing_class = Some(full.0.clone());
                    d
                })
                .collect();
            inner_classes.push(super_module(full, methods));
            super_includes.push((m, short));
        }

        // Constants: a module's own, read by bare name in its bodies.
        let mut constants = tm.constants.clone();
        let mut qualified: Vec<(String, Symbol)> = Vec::new();
        for &m in &t.mro {
            let mixin = &reg.mixins[m];
            for (name, value) in &mixin.constants {
                if constants.iter().any(|(n, _)| n == name) {
                    return Err(format!(
                        "the constant {name} of shared tests `{}` is also defined by the class or another module it includes; the spliced tests would read the wrong one",
                        mixin.name
                    ));
                }
                let mut value = value.clone();
                q(m, &mut value);
                constants.push((name.clone(), value));
                qualified.push((mixin.name.clone(), name.clone()));
            }
        }
        if !qualified.is_empty() {
            for test in &mut tests {
                unqualify(&mut test.body, &qualified);
            }
            for h in &mut helpers {
                unqualify(&mut h.body, &qualified);
            }
            for statement in &mut block_statements {
                unqualify(&mut statement.expr, &qualified);
            }
        }

        let mut out = tm.clone();
        // What the class has defined when a subclass's body loads (before
        // the persistent rename): a concern block's `test`s, declared by
        // ActiveSupport's `test` on the class, and the wrappers and
        // module methods, which a subclass's `test` of the same name
        // finds too (`refuse_inherited_test_redefinitions`).
        let mut defined = |name: &str, declared: bool| {
            if !out.defined_tests.iter().any(|(n, _)| n == name) {
                out.defined_tests.push((name.to_string(), declared));
            }
        };
        for ((_, slot), original) in t.slots.iter().zip(originals.iter()) {
            match slot {
                Slot::Own(j) if own[*j].block_test.is_some() => defined(original, true),
                Slot::Own(_) => {}
                Slot::Wrapper(_) => defined(original, false),
            }
        }
        for &m in &t.mro {
            for (name, _) in &table(m).slots {
                defined(name, false);
            }
        }
        out.tests = tests;
        out.helpers = helpers;
        out.constants = constants;
        if !block_statements.is_empty() {
            out.body_statements.extend(block_statements);
            out.tests_at_load = true;
        }
        // Each `RoundhouseSuper…` where its shared module's include stands,
        // so a plain module included after the shared one still comes
        // first in the lookup; one placed nowhere is included last.
        let mut includes: Vec<String> = Vec::new();
        for inc in includes_out {
            match inc {
                OutInclude::Module(name) => includes.push(name),
                OutInclude::Spliced(m) => {
                    // A library module of its own (`Mixin::real`): what
                    // it keeps (its classes, modules, constants) the
                    // class finds through its ancestry, as natively. Its
                    // `RoundhouseSuper…`, included after it, comes first.
                    if reg.mixins[m].real || reg.mixins[m].marker {
                        includes.push(format!("::{}", reg.mixins[m].name));
                    }
                    if let Some(i) = super_includes.iter().position(|(o, _)| *o == m) {
                        includes.push(super_includes.remove(i).1);
                    }
                }
            }
        }
        includes.extend(super_includes.into_iter().map(|(_, short)| short));
        for &m in &t.mro {
            let real = format!("::{}", reg.mixins[m].name);
            if (reg.mixins[m].real || reg.mixins[m].marker) && !includes.contains(&real) {
                includes.push(real);
            }
        }
        out.includes = includes.into_iter().map(|i| ClassId(Symbol::from(i.as_str()))).collect();
        out.inner_classes = inner_classes;
        if setup.is_some() {
            out.setup = setup;
        }
        out.teardown_blocks |= block_teardown;
        out.teardown_def = teardown_def;
        Ok(out)
    }
}

/// The class's `teardown` with every callback it registers, as Rails
/// runs them after each test. Minitest calls the `def teardown` the
/// class's lookup finds; then ActiveSupport's `after_teardown` runs the
/// `teardown do … end` callbacks, the latest registered first, whether
/// or not that def raised. A concern's `included do` block registers its
/// own on the class where the `include` stands. `ingest_test_class`
/// merged the class body's alone (its own def's body, then its blocks
/// reversed); this rebuilds the method from all of them:
///
/// ```ruby
/// def teardown
///   begin
///     <def part>
///   ensure
///     __roundhouse_teardown_<Class>_2   # the callbacks, latest first
///     __roundhouse_teardown_<Class>_1
///   end
/// end
/// ```
///
/// The def part is the `def teardown` Ruby finds on the class itself
/// (the class body's, or an `included do` block's that ran later), else
/// `super`: whatever an ancestor defines (a module the class includes, a
/// base class, or TestBase's empty one) still runs, as it does natively.
/// Each callback is a method of its own (`callback_frame`), as each is
/// its own block natively: a `next` ends only that callback, and its
/// locals are its own. `ensure` keeps the callbacks running when the def
/// part raises; a callback that raises skips the ones after it, as
/// `run_callbacks` does. One difference is left: when the def part and a
/// callback both raise, Minitest records both, and the harness sees the
/// callback's.
///
/// `block_wins` are the class-body defs a later `included do` def
/// replaced. Returns the class's helpers (the rebuilt `teardown` and the
/// callbacks' methods among them) and whether its `teardown` reaches the
/// def chain (`TestModule::teardown_def`).
fn merge_teardowns(
    tm: &TestModule,
    teardowns: Vec<Expr>,
    closures: &[usize],
    block_helpers: &mut Vec<MethodDef>,
    block_wins: &[(String, MethodReceiver)],
    class: &ClassId,
    carried: &mut Vec<crate::dialect::TestBodyStatement>,
) -> Result<(Vec<MethodDef>, bool), String> {
    let mut helpers = tm.helpers.clone();
    helpers.retain(|h| !block_wins.iter().any(|(n, r)| n == h.name.as_str() && *r == h.receiver));
    if teardowns.is_empty() {
        return Ok((helpers, tm.teardown_def));
    }
    let is_teardown = |m: &MethodDef| m.name.as_str() == "teardown" && m.receiver == MethodReceiver::Instance;
    let mut name_span = Span::synthetic();
    let mut def_part: Option<Expr> = None;
    // The class body's `teardown` (`ingest_test_class` merged its blocks
    // after its def's body), unless a later block's def replaced it.
    if let Some(merged) = helpers.iter().position(is_teardown).map(|i| helpers.remove(i)) {
        name_span = merged.name_span;
        if tm.teardown_def {
            def_part = Some(if tm.teardown_blocks {
                match &*merged.body.node {
                    ExprNode::Seq { exprs } if !exprs.is_empty() => exprs[0].clone(),
                    _ => return Err("its merged `teardown` is not the shape the shared-test splice expects".into()),
                }
            } else {
                merged.body
            });
        }
    }
    if def_part.is_none() {
        if let Some(m) = block_helpers.iter().position(is_teardown).map(|i| block_helpers.remove(i)) {
            name_span = m.name_span;
            def_part = Some(m.body);
        }
    }
    let def_part = def_part.unwrap_or_else(|| Expr::new(Span::synthetic(), ExprNode::Super { args: None, block: None }));
    let prefix = frame_prefix("teardown", class);
    let mut frames: Vec<MethodDef> = Vec::new();
    let mut names: Vec<String> = Vec::new();
    for (i, body) in teardowns.into_iter().enumerate() {
        let (body, _) = callback_frame(body, "teardown")?;
        frames.push(synth_method(&format!("{prefix}{}", i + 1), body, class));
        if closures.contains(&i) {
            let frame = frames.pop().expect("the frame just pushed");
            carried.push(closure_callback(&frame));
            names.push(frame.name.to_string());
        } else {
            names.push(frames.last().expect("the frame just pushed").name.to_string());
        }
    }
    let calls: Vec<Expr> = names.iter().rev().map(|n| call(n)).collect();
    let body = Expr::new(
        Span::synthetic(),
        ExprNode::BeginRescue {
            body: def_part,
            rescues: Vec::new(),
            else_branch: None,
            ensure: Some(Expr::new(Span::synthetic(), ExprNode::Seq { exprs: calls })),
            implicit: false,
        },
    );
    let mut method = synth_method("teardown", body, class);
    method.name_span = name_span;
    helpers.push(method);
    helpers.extend(frames);
    Ok((helpers, true))
}

/// The class's `setup do … end` callbacks, in registration order, as
/// `TestModule::setup`: each inline, except one with a `next` of its own,
/// which `callback_frame` makes a method (inline, the `next` would be
/// invalid Ruby, or end the whole setup). Inline callbacks share the
/// merged body's locals, which natively are each block's own. `None`
/// when the class's `setup` (`ingest_test_class`'s) can stay: no
/// `included do` block registered one and none needs a method.
fn frame_setups(
    setups: Vec<Expr>,
    block_setup: bool,
    closures: &[usize],
    class: &ClassId,
    carried: &mut Vec<crate::dialect::TestBodyStatement>,
) -> Result<(Option<Expr>, Vec<MethodDef>), String> {
    let prefix = frame_prefix("setup", class);
    let mut exprs: Vec<Expr> = Vec::new();
    let mut frames: Vec<MethodDef> = Vec::new();
    for (i, body) in setups.into_iter().enumerate() {
        let (framed, own_next) = callback_frame(body.clone(), "setup")?;
        if closures.contains(&i) {
            let frame = synth_method(&format!("{prefix}{}", i + 1), framed, class);
            exprs.push(call(frame.name.as_str()));
            carried.push(closure_callback(&frame));
        } else if own_next {
            let name = format!("{prefix}{}", i + 1);
            frames.push(synth_method(&name, framed, class));
            exprs.push(call(&name));
        } else {
            exprs.push(body);
        }
    }
    let rebuilt = (block_setup || !frames.is_empty()).then(|| Expr::new(Span::synthetic(), ExprNode::Seq { exprs }));
    Ok((rebuilt, frames))
}

/// The name prefix of the methods a class's callbacks become. The class
/// is in it, so a subclass's do not override a base class's (a base's
/// `__roundhouse_before_setup`, reached through `super`, calls its own).
fn frame_prefix(kind: &str, class: &ClassId) -> String {
    format!("__roundhouse_{kind}_{}_", class.0.as_str().replace("::", "__"))
}

/// A `setup do … end` / `teardown do … end` body as the body of a method
/// of its own. ActiveSupport runs each callback block with
/// `instance_exec`, so a `next` at the block's own level ends that
/// callback only: in a method, that is `return`. A `return`, `break` or
/// `redo` at that level, or a `yield` or `super` anywhere in it, raises
/// natively (LocalJumpError, or `super` outside of a method: the block's
/// home is the class body), which neither a method nor inlining
/// reproduces, so it is refused by name. Returns the body and whether it
/// had such a `next`.
fn callback_frame(mut body: Expr, kind: &str) -> Result<(Expr, bool), String> {
    let mut own_next = false;
    frame_flow(&mut body, Frame { nested: false, lambda: false }, &mut own_next).map_err(|what| {
        format!(
            "a `{kind} do … end` callback uses `{what}` where it leaves the callback's own block, which is not modeled (natively it raises)"
        )
    })?;
    Ok((body, own_next))
}

/// Where an expression of a callback body sits.
#[derive(Clone, Copy)]
struct Frame {
    /// Inside a block or loop of its own, which a `next`, `break` or
    /// `redo` there belongs to.
    nested: bool,
    /// Inside a lambda, which a `return` there returns from.
    lambda: bool,
}

fn frame_flow(e: &mut Expr, at: Frame, own_next: &mut bool) -> Result<(), &'static str> {
    let block = |at: Frame, is_lambda: bool| Frame { nested: true, lambda: at.lambda || is_lambda };
    match &mut *e.node {
        // A method of its own: nothing in it leaves the callback.
        ExprNode::MethodDefExpr { .. } => return Ok(()),
        ExprNode::Next { value } if !at.nested => {
            if let Some(v) = value {
                frame_flow(v, at, own_next)?;
            }
            let value = value.take().unwrap_or_else(|| Expr::new(Span::synthetic(), ExprNode::Lit { value: crate::expr::Literal::Nil }));
            *e.node = ExprNode::Return { value };
            *own_next = true;
            return Ok(());
        }
        ExprNode::Break { .. } if !at.nested => return Err("break"),
        ExprNode::Redo if !at.nested => return Err("redo"),
        ExprNode::Return { .. } if !at.lambda => return Err("return"),
        ExprNode::Yield { .. } => return Err("yield"),
        ExprNode::Super { .. } => return Err("super"),
        ExprNode::Send { recv, method, args, block: Some(b), .. } => {
            if let Some(r) = recv {
                frame_flow(r, at, own_next)?;
            }
            for a in args {
                frame_flow(a, at, own_next)?;
            }
            // `lambda { … }` is a lambda; any other block a proc.
            let inner = block(at, recv.is_none() && method.as_str() == "lambda");
            return match &mut *b.node {
                ExprNode::Lambda { body, .. } => frame_flow(body, inner, own_next),
                _ => frame_flow(b, at, own_next),
            };
        }
        ExprNode::Apply { fun, args, block: Some(b) } => {
            frame_flow(fun, at, own_next)?;
            for a in args {
                frame_flow(a, at, own_next)?;
            }
            return match &mut *b.node {
                ExprNode::Lambda { body, .. } => frame_flow(body, block(at, false), own_next),
                _ => frame_flow(b, at, own_next),
            };
        }
        // `-> { … }`, not in a block slot.
        ExprNode::Lambda { body, .. } => return frame_flow(body, block(at, true), own_next),
        ExprNode::While { cond, body, .. } => {
            frame_flow(cond, at, own_next)?;
            return frame_flow(body, block(at, false), own_next);
        }
        _ => {}
    }
    let mut failed = None;
    e.node.for_each_child_mut(&mut |c| {
        if failed.is_none() {
            if let Err(what) = frame_flow(c, at, own_next) {
                failed = Some(what);
            }
        }
    });
    failed.map_or(Ok(()), Err)
}

/// The body of the class body's own `def teardown`, as `ingest_test_class`
/// kept it (first in its merged `teardown` when it has blocks too).
fn class_def_teardown(tm: &TestModule) -> Option<Expr> {
    if !tm.teardown_def {
        return None;
    }
    let m = tm.helpers.iter().find(|m| m.name.as_str() == "teardown")?;
    if !tm.teardown_blocks {
        return Some(m.body.clone());
    }
    match &*m.body.node {
        ExprNode::Seq { exprs } => exprs.first().cloned(),
        _ => None,
    }
}

/// The method's last statement is `super`: what its ancestors' teardown
/// runs after it still runs last.
fn ends_in_super(e: &Expr) -> bool {
    match &*e.node {
        ExprNode::Super { .. } => true,
        ExprNode::Seq { exprs } => exprs.last().is_some_and(ends_in_super),
        _ => false,
    }
}

/// Whether the class's teardown, as built, still runs what its ancestors
/// register as Rails runs it. Rails runs the `def teardown` chain, then
/// every callback in the chain: the class's, then each base's, nearest
/// first. A spliced class's `teardown` runs its def part (or `super`),
/// then its own callbacks; a base's runs within the `super`. So only one
/// class in the chain may register callbacks, every `def teardown` below
/// it must end by calling `super`, and one that `ingest_test_class`
/// merged (a class with no shared modules) must not replace a def above
/// it. The modules included below that class sit in the def chain before
/// its teardown too, so each `def teardown` they define must also end in
/// `super`; one no test file declares may define one, so it is refused
/// too, unless it is one of `NO_TEARDOWN`. A module the class with the
/// callbacks, or a class above it, already includes is not included again
/// (Ruby skips an ancestor), so it does not count below. `src/analyze/
/// test_teardown.rs` says the same of the classes this does not see; here
/// the class is refused by name, so none of its ids is listed.
/// `own_callbacks`: the class registers callbacks; `own_def`: its own
/// `def teardown`, as `BodyTeardown::def`; `own_modules`: the modules its
/// body includes (`TestMixins::module_teardowns`).
fn check_teardown_chain(
    own_callbacks: bool,
    own_def: Option<bool>,
    own_modules: &[ModuleTeardown],
    ancestry: &Ancestry,
) -> Result<(), String> {
    let Some(k) = ancestry.classes.iter().position(|a| a.callbacks) else { return Ok(()) };
    let base = &ancestry.classes[k];
    if own_callbacks {
        return Err(format!(
            "it and its base class `{}` both register teardown callbacks: Rails runs this class's, then the base's, after the whole `def teardown` chain, which the merged teardown methods do not reproduce",
            base.name
        ));
    }
    if let Some(other) = ancestry.classes[k + 1..].iter().find(|a| a.callbacks) {
        return Err(format!(
            "its base classes `{}` and `{}` both register teardown callbacks, which the merged teardown methods do not run in Rails' order",
            base.name, other.name
        ));
    }
    if own_def == Some(false) {
        return Err(format!(
            "its `def teardown` does not end by calling `super`, so the teardown callbacks of its base class `{}` would not run after it as Rails runs them",
            base.name
        ));
    }
    if let Some(a) = ancestry.classes[..k].iter().find(|a| a.def == Some(false)) {
        return Err(format!(
            "the `def teardown` of its base class `{}` does not end by calling `super`, so the teardown callbacks of `{}` would not run after it as Rails runs them",
            a.name, base.name
        ));
    }
    let above: Vec<&ModuleTeardown> = ancestry.classes[k..].iter().flat_map(|a| a.modules.iter()).collect();
    let below = std::iter::once((None, own_modules)).chain(ancestry.classes[..k].iter().map(|a| (Some(a.name.as_str()), a.modules.as_slice())));
    for (owner, modules) in below {
        let by = |m: &ModuleTeardown| match owner {
            None => format!("`{}`, which it includes,", m.name),
            Some(c) => format!("`{}`, which its base class `{c}` includes,", m.name),
        };
        for m in modules.iter().filter(|m| !above.iter().any(|a| a.name == m.name)) {
            if !m.known {
                return Err(format!(
                    "the module {} is not one a test file declares, so whether a `def teardown` of it lets the teardown callbacks of its base class `{}` run after the whole `def teardown` chain, as Rails runs them, is not known",
                    by(m),
                    base.name
                ));
            }
            if m.def == Some(false) {
                return Err(format!(
                    "the `def teardown` of the module {} does not end by calling `super`, so the teardown callbacks of its base class `{}` would not run after the whole `def teardown` chain as Rails runs them",
                    by(m),
                    base.name
                ));
            }
        }
    }
    if !base.spliced && base.def.is_none() {
        if let Some(a) = ancestry.classes[k + 1..].iter().find(|a| a.def.is_some()) {
            return Err(format!(
                "the teardown blocks of its base class `{}` replace the `def teardown` of `{}`, which Rails would still run after each test",
                base.name, a.name
            ));
        }
        if let Some(m) = above.iter().find(|m| m.def.is_some()) {
            return Err(format!(
                "the teardown blocks of its base class `{}` replace the `def teardown` of the module `{}`, which Rails would still run after each test",
                base.name, m.name
            ));
        }
    }
    Ok(())
}

/// A class body's `def` as (name, receiver): an instance method
/// (`instance_def`) or `def self.…`.
fn body_def_key(stmt: &Node<'_>) -> Option<(String, MethodReceiver)> {
    if let Some(def) = instance_def(stmt) {
        return Some((constant_id_str(&def.name()).to_string(), MethodReceiver::Instance));
    }
    let def = stmt.as_def_node()?;
    def.receiver()?.as_self_node()?;
    Some((constant_id_str(&def.name()).to_string(), MethodReceiver::Class))
}

fn visibility_of(name: &str) -> Option<MethodVisibility> {
    match name {
        "private" => Some(MethodVisibility::Private),
        "protected" => Some(MethodVisibility::Protected),
        "public" => Some(MethodVisibility::Public),
        _ => None,
    }
}

/// `extend ActiveSupport::Testing::Declarative`.
fn is_extend_declarative(stmt: &Node<'_>) -> bool {
    let Some(call) = stmt.as_call_node().filter(|c| c.receiver().is_none() && c.block().is_none()) else { return false };
    if constant_id_str(&call.name()) != "extend" {
        return false;
    }
    let args: Vec<Node<'_>> = call.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default();
    match args.as_slice() {
        [arg] => IncludeRef::of(arg).is_some_and(|r| r.written() == "ActiveSupport::Testing::Declarative"),
        _ => false,
    }
}

/// A statement of an `included do` block that registers FixtureFactory
/// factories on the class: `define_factories { … }` / `factory(…)`, or an
/// `if`/`unless` on `fixture_factory_definitions` (the registry the
/// class already has) whose branches are all such statements.
fn is_factory_registration(stmt: &Node<'_>) -> bool {
    fn branch(statements: Option<ruby_prism::StatementsNode<'_>>) -> bool {
        statements.is_some_and(|s| {
            let body: Vec<Node<'_>> = s.body().iter().collect();
            !body.is_empty() && body.iter().all(is_factory_registration)
        })
    }
    fn else_of(node: Option<Node<'_>>) -> bool {
        match node {
            None => true,
            Some(n) => match n.as_else_node() {
                Some(e) => branch(e.statements()),
                None => is_factory_registration(&n),
            },
        }
    }
    // Only a read of the class's registry decides the branch.
    fn registry_condition(node: &Node<'_>) -> bool {
        use ruby_prism::Visit;
        struct Reads {
            ok: bool,
            registry: bool,
        }
        impl<'pr> Visit<'pr> for Reads {
            fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
                match (node.receiver(), constant_id_str(&node.name())) {
                    (None, "fixture_factory_definitions") => self.registry = true,
                    (Some(_), "empty?" | "any?" | "blank?" | "present?" | "nil?" | "!") => {}
                    _ => self.ok = false,
                }
                ruby_prism::visit_call_node(self, node);
            }
        }
        let mut reads = Reads { ok: true, registry: false };
        reads.visit(node);
        reads.ok && reads.registry
    }
    if let Some(call) = stmt.as_call_node() {
        let name = constant_id_str(&call.name());
        if call.receiver().is_none() {
            return matches!(name, "define_factories" | "factory");
        }
        // `Factories.define(self)`: a factory module registering on the class.
        let args: Vec<Node<'_>> = call.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default();
        return name == "define"
            && call.receiver().is_some_and(|r| constant_path_of(&r).is_some())
            && call.block().is_none()
            && matches!(args.as_slice(), [arg] if arg.as_self_node().is_some());
    }
    if let Some(node) = stmt.as_if_node() {
        return registry_condition(&node.predicate()) && branch(node.statements()) && else_of(node.subsequent());
    }
    if let Some(node) = stmt.as_unless_node() {
        return registry_condition(&node.predicate())
            && branch(node.statements())
            && node.else_clause().is_none_or(|e| branch(e.statements()));
    }
    false
}

/// The modules a class body's `include` statements name.
fn body_includes(stmts: &[Node<'_>]) -> Vec<IncludeRef> {
    let mut out = Vec::new();
    for stmt in stmts {
        let Some(call) = stmt.as_call_node().filter(|c| c.receiver().is_none()) else { continue };
        if constant_id_str(&call.name()) == "include" {
            if let Some(args) = call.arguments() {
                out.extend(args.arguments().iter().filter_map(|n| IncludeRef::of(&n)));
            }
        }
    }
    out
}

/// A class or module body's instance-method `def`: bare, or written
/// `private def …` (`protected`, `public`).
fn instance_def<'pr>(stmt: &Node<'pr>) -> Option<ruby_prism::DefNode<'pr>> {
    if let Some(def) = stmt.as_def_node() {
        return def.receiver().is_none().then_some(def);
    }
    let call = stmt.as_call_node()?;
    if call.receiver().is_some() || !matches!(constant_id_str(&call.name()), "private" | "protected" | "public") {
        return None;
    }
    let args: Vec<Node<'pr>> = call.arguments()?.arguments().iter().collect();
    match args.as_slice() {
        [arg] => arg.as_def_node().filter(|d| d.receiver().is_none()),
        _ => None,
    }
}

/// A `super` or `super(…)` anywhere in `def`'s body.
fn def_calls_super(def: &ruby_prism::DefNode<'_>) -> bool {
    use ruby_prism::Visit;
    struct Find(bool);
    impl<'pr> Visit<'pr> for Find {
        fn visit_super_node(&mut self, _: &ruby_prism::SuperNode<'pr>) {
            self.0 = true;
        }
        fn visit_forwarding_super_node(&mut self, _: &ruby_prism::ForwardingSuperNode<'pr>) {
            self.0 = true;
        }
    }
    let mut find = Find(false);
    if let Some(body) = def.body() {
        find.visit(&body);
    }
    find.0
}

/// A `def`'s last statement is `super` (or bare `super`).
fn def_ends_in_super(def: &ruby_prism::DefNode<'_>) -> bool {
    let Some(body) = def.body() else { return false };
    let Some(stmts) = body.as_statements_node() else { return false };
    stmts.body().iter().last().is_some_and(|s| s.as_super_node().is_some() || s.as_forwarding_super_node().is_some())
}

/// The modules other than shared test ones that including `k` brings,
/// in the order the emitted class includes them, with where each shared
/// test module stands among them (`k` itself last, which the caller
/// adds): a Concern's deferred dependencies first, as
/// `Concern#append_features` includes them first.
fn flat_passthrough(k: usize, tables: &Tables) -> Vec<OutInclude> {
    let Ok(kt) = table_of(tables, k) else { return Vec::new() };
    let mut out: Vec<OutInclude> = Vec::new();
    for &d in &kt.deps {
        for inc in flat_passthrough(d, tables).into_iter().chain([OutInclude::Spliced(d)]) {
            if !out.contains(&inc) {
                out.push(inc);
            }
        }
    }
    for inc in &kt.passthrough {
        if !out.contains(inc) {
            out.push(inc.clone());
        }
    }
    out
}

/// Whether `name` is a method of `t`'s owner or of an ancestor
/// (`method_defined?`).
fn defined_in(t: &Table, name: &str, tables: &Tables) -> bool {
    t.slot(name).is_some() || t.mro.iter().any(|&m| table_of(tables, m).is_ok_and(|mt| mt.slot(name).is_some()))
}

/// `k.instance_methods.grep(/^test_/)`: its own and its ancestors'.
fn instance_test_names(k: usize, tables: &Tables) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let Ok(kt) = table_of(tables, k) else { return out };
    let mut push = |t: &Table| {
        for (name, _) in &t.slots {
            if !out.contains(name) {
                out.push(name.clone());
            }
        }
    };
    push(kt);
    for &m in &kt.mro {
        if let Ok(mt) = table_of(tables, m) {
            push(mt);
        }
    }
    out
}

/// Where Ruby looks a constant written in a body with this nesting up.
fn candidates(r: &IncludeRef, nesting: &[String]) -> Vec<String> {
    let written = r.written();
    if r.rooted {
        return vec![written];
    }
    let mut out: Vec<String> = nesting.iter().map(|n| format!("{n}::{written}")).collect();
    out.push(written);
    out
}

fn scan_node(
    node: &Node<'_>,
    outer: &[String],
    file: &str,
    out: &mut Vec<Decl>,
    classes: &mut Vec<ClassRef>,
    bases: &mut Vec<ClassRef>,
    known: &mut Set<String>,
    declared: &mut Set<String>,
) {
    if let Some(c) = node.as_class_node() {
        let Some(written) = class_name_path(&c) else { return };
        let name = match outer.first() {
            Some(o) => format!("{o}::{}", written.join("::")),
            None => written.join("::"),
        };
        let mut nesting = vec![name.clone()];
        nesting.extend(outer.iter().cloned());
        declared.insert(name.clone());
        let stmts = c.body().map(flatten_statements).unwrap_or_default();
        let mut includes = Vec::new();
        let mut extends = Vec::new();
        let mut teardown = BodyTeardown::default();
        for stmt in &stmts {
            if stmt.as_class_node().is_some() || stmt.as_module_node().is_some() {
                scan_node(stmt, &nesting, file, out, classes, bases, known, declared);
                continue;
            }
            if let Some(cw) = stmt.as_constant_write_node() {
                declared.insert(format!("{name}::{}", constant_id_str(&cw.name())));
            }
            if let Some(call) = stmt.as_call_node() {
                if call.receiver().is_none() {
                    match constant_id_str(&call.name()) {
                        "include" => {
                            if let Some(args) = call.arguments() {
                                includes.extend(args.arguments().iter().filter_map(|n| IncludeRef::of(&n)));
                            }
                        }
                        "teardown" => teardown.callbacks = true,
                        _ => {}
                    }
                }
                if call.receiver().is_none() && constant_id_str(&call.name()) == "extend" {
                    if let Some(args) = call.arguments() {
                        let at = call.location().start_offset();
                        extends.extend(args.arguments().iter().filter_map(|n| IncludeRef::of(&n)).map(|r| (r, at)));
                    }
                }
            }
            if let Some(def) = instance_def(stmt).filter(|d| d.name().as_slice() == b"teardown") {
                teardown.def = Some(def_ends_in_super(&def));
            }
        }
        if let Some(superclass) = c.superclass() {
            classes.push(ClassRef {
                name,
                nesting,
                includes,
                extends,
                file: file.to_string(),
                superclass: IncludeRef::of(&superclass),
                teardown,
            });
        } else if name == "ActiveSupport::TestCase" {
            bases.push(ClassRef { name, nesting, includes, extends, file: file.to_string(), superclass: None, teardown });
        }
        return;
    }
    let Some(m) = node.as_module_node() else { return };
    let Some(written) = module_name_path(&m) else { return };
    let name = match outer.first() {
        Some(o) => format!("{o}::{}", written.join("::")),
        None => written.join("::"),
    };
    let mut nesting = vec![name.clone()];
    nesting.extend(outer.iter().cloned());
    known.insert(name.clone());
    declared.insert(name.clone());
    let stmts = m.body().map(flatten_statements).unwrap_or_default();
    let mut decl = Decl {
        name: name.clone(),
        nesting: nesting.clone(),
        file: file.to_string(),
        offset: m.location().start_offset(),
        includes: Vec::new(),
        content: false,
        declares: false,
        provider: name == "SharedTests" && is_shared_tests_provider(&stmts),
        macros: Vec::new(),
        teardown: None,
        block_includes: Vec::new(),
        members: false,
        concern: false,
    };
    let mut class_defs: Vec<ruby_prism::DefNode<'_>> = Vec::new();
    let in_class_methods = written.last().map(String::as_str) == Some("ClassMethods");
    // A `def test_…` after a bare `private`/`protected`, or written as
    // `private def test_…`, is a helper: Minitest runs public methods only
    // (`public_instance_methods` in Core's `methods_matching`).
    let mut hidden = false;
    for stmt in &stmts {
        if let Some(inner) = stmt.as_module_node() {
            if module_name_path(&inner).as_deref() == Some(&["ClassMethods".to_string()][..]) {
                class_defs.extend(inner.body().map(flatten_statements).unwrap_or_default().iter().filter_map(|n| n.as_def_node()));
            } else {
                decl.members = true;
            }
            scan_node(stmt, &nesting, file, out, classes, bases, known, declared);
            continue;
        }
        if stmt.as_class_node().is_some() {
            decl.members = true;
            scan_node(stmt, &nesting, file, out, classes, bases, known, declared);
            continue;
        }
        decl.content = true;
        if module_member(stmt) {
            decl.members = true;
        }
        if let Some(cw) = stmt.as_constant_write_node() {
            declared.insert(format!("{name}::{}", constant_id_str(&cw.name())));
        }
        if let Some(level) = bare_visibility(stmt) {
            hidden = level != MethodVisibility::Public;
        }
        // A `def test_…` of a `ClassMethods` module is a class method of the
        // includer (`test_input_field_authorization(…)`, a test macro), not
        // a test: the module is the app's, not a shared test module.
        let class_method_def = in_class_methods && stmt.as_def_node().is_some();
        if !class_method_def && !non_public_def(stmt, hidden) && (is_test_declaration_shape(stmt) || hidden_test_declaration(stmt).is_some()) {
            decl.declares = true;
        }
        if let Some(def) = instance_def(stmt) {
            note_teardown(&def, &mut decl.teardown);
        }
        if let Some(call) = stmt.as_call_node() {
            if call.receiver().is_none() {
                match constant_id_str(&call.name()) {
                    "include" => {
                        if let Some(args) = call.arguments() {
                            decl.includes.extend(args.arguments().iter().filter_map(|n| IncludeRef::of(&n)));
                        }
                    }
                    "included" if call.block().is_some() => {
                        let body = call.block().and_then(|b| b.as_block_node()).and_then(|b| b.body());
                        for s in body.map(flatten_statements).unwrap_or_default() {
                            if let Some(def) = instance_def(&s) {
                                note_teardown(&def, &mut decl.teardown);
                            }
                            let Some(c) = s.as_call_node().filter(|c| c.receiver().is_none()) else { continue };
                            if constant_id_str(&c.name()) == "include" {
                                if let Some(args) = c.arguments() {
                                    decl.block_includes.extend(args.arguments().iter().filter_map(|n| IncludeRef::of(&n)));
                                }
                            }
                        }
                        if included_block_declares(&call) {
                            decl.declares = true;
                        }
                    }
                    "extend" => {
                        let args = call.arguments().map(|a| a.arguments().iter().collect::<Vec<_>>()).unwrap_or_default();
                        if args.iter().any(|a| constant_path_of(a).is_some_and(|p| p.join("::") == "ActiveSupport::Concern")) {
                            decl.concern = true;
                        }
                    }
                    "class_methods" => {
                        let body = call.block().and_then(|b| b.as_block_node()).and_then(|b| b.body());
                        class_defs.extend(body.map(flatten_statements).unwrap_or_default().iter().filter_map(|n| n.as_def_node()));
                    }
                    "tag" | "setup" | "teardown" | "generate_tests" if call.block().is_some() => {
                        decl.declares = true;
                    }
                    _ => {}
                }
            }
        }
    }
    decl.macros = test_macros(&class_defs);
    out.push(decl);
}

/// A module-body statement that belongs to the module itself
/// (`Decl::members`, besides a nested class or module): `class << self`,
/// `def self.…`, or a write of the module's own instance variable.
fn module_member(stmt: &Node<'_>) -> bool {
    stmt.as_singleton_class_node().is_some()
        || stmt.as_def_node().is_some_and(|d| d.receiver().is_some_and(|r| r.as_self_node().is_some()))
        || stmt.as_instance_variable_write_node().is_some()
        || stmt.as_instance_variable_or_write_node().is_some()
        || stmt.as_instance_variable_and_write_node().is_some()
        || stmt.as_instance_variable_operator_write_node().is_some()
}

/// Whether `e` reads a local variable nothing in it binds: one of the
/// `included do` block it was written in (a callback body is a closure
/// there), or the block's `base`.
fn reads_block_local(e: &Expr) -> bool {
    fn walk(e: &Expr, reads: &mut Vec<String>, binds: &mut Set<String>) {
        match &*e.node {
            ExprNode::Var { name, .. } => reads.push(name.to_string()),
            ExprNode::Assign { target: crate::expr::LValue::Var { name, .. }, .. }
            | ExprNode::OpAssign { target: crate::expr::LValue::Var { name, .. }, .. } => {
                binds.insert(name.to_string());
            }
            ExprNode::MultiAssign { targets, .. } => {
                for t in targets {
                    if let crate::expr::LValue::Var { name, .. } = t {
                        binds.insert(name.to_string());
                    }
                }
            }
            ExprNode::Let { name, .. } => {
                binds.insert(name.to_string());
            }
            ExprNode::Lambda { params, rest_param, extra_params, block_param, .. } => {
                binds.extend(params.iter().map(|p| p.to_string()));
                binds.extend(rest_param.iter().chain(block_param.iter()).map(|p| p.to_string()));
                binds.extend(extra_params.iter().map(|p| p.name.to_string()));
            }
            ExprNode::BeginRescue { rescues, .. } => {
                binds.extend(rescues.iter().filter_map(|r| r.binding.as_ref()).map(|b| b.to_string()));
            }
            _ => {}
        }
        e.node.for_each_child(&mut |c| walk(c, reads, binds));
    }
    let (mut reads, mut binds) = (Vec::new(), Set::default());
    walk(e, &mut reads, &mut binds);
    reads.iter().any(|r| !binds.contains(r))
}

/// A callback method as a statement of the class body that defines it with
/// `define_method`, so its body is a closure over the class body's locals
/// (the `included do` block's), as the `setup do … end` block is natively.
fn closure_callback(frame: &MethodDef) -> crate::dialect::TestBodyStatement {
    let span = Span::synthetic();
    let block = Expr::new(
        span,
        ExprNode::Lambda {
            params: Vec::new(),
            rest_param: None,
            extra_params: Vec::new(),
            block_param: None,
            body: frame.body.clone(),
            block_style: crate::expr::BlockStyle::Do,
            signature: crate::expr::BlockSignature::default(),
        },
    );
    let name = Expr::new(span, ExprNode::Lit { value: crate::expr::Literal::Sym { value: frame.name.clone() } });
    crate::dialect::TestBodyStatement {
        expr: Expr::new(
            span,
            ExprNode::Send { recv: None, method: Symbol::from("define_method"), args: vec![name], block: Some(block), parenthesized: true },
        ),
        line: 0,
        after_definitions: false,
        after_setup_blocks: false,
        after_teardown_blocks: false,
        source: None,
    }
}

/// A statement of an `included do` block the splice carries as written
/// (`ClassWalk::walk`): a call, a local variable write, or a conditional
/// around either.
fn carries_in_included_block(stmt: &Node<'_>) -> bool {
    stmt.as_call_node().is_some()
        || stmt.as_local_variable_write_node().is_some()
        || stmt.as_local_variable_or_write_node().is_some()
        || stmt.as_if_node().is_some()
        || stmt.as_unless_node().is_some()
}

/// Whether `e` reads the local variable `name`.
fn reads_var(e: &Expr, name: &str) -> bool {
    if matches!(&*e.node, ExprNode::Var { name: n, .. } if n.as_str() == name) {
        return true;
    }
    let mut found = false;
    e.node.for_each_child(&mut |c| found |= reads_var(c, name));
    found
}

/// Whether `call`'s only argument is the keyword `prepend: true|false`
/// (`setup(prepend: true) do … end`).
fn is_prepend_option(call: &ruby_prism::CallNode<'_>) -> bool {
    let Some(args) = call.arguments() else { return false };
    let args: Vec<Node<'_>> = args.arguments().iter().collect();
    let [arg] = args.as_slice() else { return false };
    let Some(hash) = arg.as_keyword_hash_node() else { return false };
    let elements: Vec<Node<'_>> = hash.elements().iter().collect();
    let [element] = elements.as_slice() else { return false };
    let Some(assoc) = element.as_assoc_node() else { return false };
    assoc.key().as_symbol_node().is_some_and(|k| k.unescaped() == b"prepend")
        && (assoc.value().as_true_node().is_some() || assoc.value().as_false_node().is_some())
}

/// The hooks Ruby calls on a module as it is included, extended or
/// prepended, which a module's singleton method of the name replaces.
const MODULE_HOOKS: &[&str] =
    &["included", "extended", "prepended", "append_features", "prepend_features", "extend_object", "method_added", "const_missing"];

/// A hook `stmt` defines on the module (`def self.included`, or one in
/// `class << self`): it would run on every includer.
fn module_hook(stmt: &Node<'_>) -> Option<String> {
    if let Some(def) = stmt.as_def_node().filter(|d| d.receiver().is_some_and(|r| r.as_self_node().is_some())) {
        let name = constant_id_str(&def.name());
        return MODULE_HOOKS.contains(&name).then(|| format!("self.{name}"));
    }
    let singleton = stmt.as_singleton_class_node()?;
    for s in singleton.body().map(flatten_statements).unwrap_or_default() {
        let def = s.as_def_node().or_else(|| instance_def(&s));
        if let Some(def) = def.filter(|d| d.receiver().is_none()) {
            let name = constant_id_str(&def.name());
            if MODULE_HOOKS.contains(&name) {
                return Some(format!("{name} (in `class << self`)"));
            }
        }
        if let Some(call) = s.as_call_node().filter(|c| c.receiver().is_none()) {
            let name = constant_id_str(&call.name());
            if matches!(name, "alias_method" | "define_method") {
                return Some(format!("`{name}` in `class << self`"));
            }
        }
    }
    None
}

/// The modules `root` reaches other than by the `include` of a test
/// class's or module's own body, by their written names, into
/// `out` (`TestMixins::reached`): a constant that receives a
/// `METHOD_REFLECTION` call, or that an `include`, `extend` or `prepend`
/// (also through `send`) names with a receiver, at the top level, in a
/// block, method or `class << …`, or in the body of a class without a
/// superclass, where the splice does not take the module's helpers.
fn reached_modules(root: &Node<'_>, out: &mut Set<String>, included: &mut Set<String>) {
    use ruby_prism::Visit;
    struct Reach<'s> {
        out: &'s mut Set<String>,
        /// `TestMixins::library_included`.
        included: &'s mut Set<String>,
        /// Each enclosing scope: whether a receiverless `include` in it is
        /// one the splice handles (a module's or a subclass's body).
        own: Vec<bool>,
        /// Each enclosing scope: whether it is a class body without a
        /// superclass.
        library_class: Vec<bool>,
    }
    impl Reach<'_> {
        fn note(&mut self, node: &Node<'_>) {
            if let Some(path) = constant_path_of(node).filter(|p| !p.is_empty()) {
                let rooted = node.as_constant_path_node().is_some_and(|p| constant_path_is_rooted(&p));
                self.out.insert(format!("{}{}", if rooted { "::" } else { "" }, path.join("::")));
            }
        }
    }
    impl<'pr> Visit<'pr> for Reach<'_> {
        fn visit_module_node(&mut self, node: &ruby_prism::ModuleNode<'pr>) {
            self.own.push(true);
            self.library_class.push(false);
            ruby_prism::visit_module_node(self, node);
            self.own.pop();
            self.library_class.pop();
        }
        fn visit_class_node(&mut self, node: &ruby_prism::ClassNode<'pr>) {
            self.own.push(node.superclass().is_some());
            self.library_class.push(node.superclass().is_none());
            ruby_prism::visit_class_node(self, node);
            self.own.pop();
            self.library_class.pop();
        }
        fn visit_singleton_class_node(&mut self, node: &ruby_prism::SingletonClassNode<'pr>) {
            self.own.push(false);
            self.library_class.push(false);
            ruby_prism::visit_singleton_class_node(self, node);
            self.own.pop();
            self.library_class.pop();
        }
        fn visit_def_node(&mut self, node: &ruby_prism::DefNode<'pr>) {
            self.own.push(false);
            self.library_class.push(false);
            ruby_prism::visit_def_node(self, node);
            self.own.pop();
            self.library_class.pop();
        }
        fn visit_block_node(&mut self, node: &ruby_prism::BlockNode<'pr>) {
            self.own.push(false);
            self.library_class.push(false);
            ruby_prism::visit_block_node(self, node);
            self.own.pop();
            self.library_class.pop();
        }
        fn visit_lambda_node(&mut self, node: &ruby_prism::LambdaNode<'pr>) {
            self.own.push(false);
            self.library_class.push(false);
            ruby_prism::visit_lambda_node(self, node);
            self.own.pop();
            self.library_class.pop();
        }
        fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
            let name = constant_id_str(&node.name());
            let args: Vec<Node<'pr>> = node.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default();
            if let Some(receiver) = node.receiver().filter(|_| METHOD_REFLECTION.contains(&name)) {
                self.note(&receiver);
            }
            let mixes = |n: &str| matches!(n, "include" | "extend" | "prepend");
            if mixes(name) && !(node.receiver().is_none() && self.own.last() == Some(&true)) {
                args.iter().for_each(|a| self.note(a));
            }
            if name == "include" && node.receiver().is_none() && self.library_class.last() == Some(&true) {
                for a in &args {
                    if let Some(path) = constant_path_of(a).filter(|p| !p.is_empty()) {
                        let rooted = a.as_constant_path_node().is_some_and(|p| constant_path_is_rooted(&p));
                        self.included.insert(format!("{}{}", if rooted { "::" } else { "" }, path.join("::")));
                    }
                }
            }
            let sent = args.first().and_then(|a| a.as_symbol_node()).is_some_and(|s| mixes(&String::from_utf8_lossy(s.unescaped())));
            if matches!(name, "send" | "public_send" | "__send__") && sent {
                args[1..].iter().for_each(|a| self.note(a));
            }
            ruby_prism::visit_call_node(self, node);
        }
    }
    Reach { out, included, own: Vec::new(), library_class: Vec::new() }.visit(root);
}

/// The constants `root` reads (`TestMixins::referenced`), by their written
/// names (`::` leads a rooted one), except the modules of a receiverless
/// `include`, `extend` or `prepend` in the body of a module or class, and
/// the names of the modules and classes it declares: the splice handles
/// those.
fn referenced_constants(root: &Node<'_>, out: &mut Set<String>) {
    use ruby_prism::Visit;
    struct Refs<'s> {
        out: &'s mut Set<String>,
        /// Whether the innermost scope is a module or class body.
        body: Vec<bool>,
    }
    impl Refs<'_> {
        fn scope<F: FnOnce(&mut Self)>(&mut self, body: bool, f: F) {
            self.body.push(body);
            f(self);
            self.body.pop();
        }
    }
    impl<'pr> Visit<'pr> for Refs<'_> {
        fn visit_module_node(&mut self, node: &ruby_prism::ModuleNode<'pr>) {
            self.scope(true, |v| node.body().iter().for_each(|b| v.visit(b)));
        }
        fn visit_class_node(&mut self, node: &ruby_prism::ClassNode<'pr>) {
            if let Some(superclass) = node.superclass() {
                self.visit(&superclass);
            }
            self.scope(true, |v| node.body().iter().for_each(|b| v.visit(b)));
        }
        fn visit_singleton_class_node(&mut self, node: &ruby_prism::SingletonClassNode<'pr>) {
            self.scope(false, |v| ruby_prism::visit_singleton_class_node(v, node));
        }
        fn visit_def_node(&mut self, node: &ruby_prism::DefNode<'pr>) {
            self.scope(false, |v| ruby_prism::visit_def_node(v, node));
        }
        fn visit_block_node(&mut self, node: &ruby_prism::BlockNode<'pr>) {
            self.scope(false, |v| ruby_prism::visit_block_node(v, node));
        }
        fn visit_lambda_node(&mut self, node: &ruby_prism::LambdaNode<'pr>) {
            self.scope(false, |v| ruby_prism::visit_lambda_node(v, node));
        }
        fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
            let name = constant_id_str(&node.name());
            if matches!(name, "include" | "extend" | "prepend") && node.receiver().is_none() && self.body.last() == Some(&true) {
                return;
            }
            ruby_prism::visit_call_node(self, node);
        }
        fn visit_constant_read_node(&mut self, node: &ruby_prism::ConstantReadNode<'pr>) {
            self.out.insert(constant_id_str(&node.name()).to_string());
        }
        fn visit_constant_path_node(&mut self, node: &ruby_prism::ConstantPathNode<'pr>) {
            match constant_path_of(&node.as_node()).filter(|p| !p.is_empty()) {
                Some(path) => {
                    let rooted = constant_path_is_rooted(node);
                    self.out.insert(format!("{}{}", if rooted { "::" } else { "" }, path.join("::")));
                }
                None => ruby_prism::visit_constant_path_node(self, node),
            }
        }
    }
    Refs { out, body: Vec::new() }.visit(root);
}

/// How a library module (`Mixin::real`) is carried (`library_source`).
#[derive(Clone, Copy)]
struct LibraryForm {
    /// Its instance helpers stay in its body (`Mixin::keeps_helpers`).
    helpers: bool,
    /// It stays a Concern with its `included do` (`Mixin::library_concern`).
    concern: bool,
}

/// `library_source`'s walk: the ranges to blank (`blank`), the bytes that
/// become a `\` line continuation (`continued`), and whether a
/// declaration is kept (`keeps`): a library module's member, or a class,
/// module or statement other than a shared module. `spliced` is each
/// shared module's form by offset (`None`: not a library module), `kept`
/// the `include`s library modules keep (`Mixin::kept_includes`).
struct LibraryBlanks<'a> {
    spliced: &'a Map<usize, Option<LibraryForm>>,
    /// The offsets of the shared modules kept as empty markers
    /// (`Mixin::marker`).
    markers: &'a Set<usize>,
    kept: &'a [(usize, Vec<bool>)],
    source: &'a [u8],
    blank: Vec<(usize, usize)>,
    continued: Vec<usize>,
    keeps: bool,
}

impl LibraryBlanks<'_> {
    /// `stmts`: a file's, or a module's or class's body (`nested` below
    /// the top level).
    fn walk(&mut self, stmts: &[Node<'_>], nested: bool) -> Result<(), String> {
        for stmt in stmts {
            let body = stmt
                .as_module_node()
                .and_then(|m| m.body())
                .or_else(|| stmt.as_class_node().and_then(|c| c.body()))
                .or_else(|| stmt.as_singleton_class_node().and_then(|c| c.body()));
            let declaration = stmt.as_module_node().is_some() || stmt.as_class_node().is_some();
            let Some(&form) = self.spliced.get(&stmt.location().start_offset()).filter(|_| stmt.as_module_node().is_some()) else {
                if declaration {
                    let inner = body.map(flatten_statements).unwrap_or_default();
                    let range = stmt.location().start_offset()..stmt.location().end_offset();
                    if self.spliced.keys().any(|at| range.contains(at)) {
                        self.walk(&inner, true)?;
                    } else {
                        self.keeps = true;
                    }
                } else if nested {
                    self.keeps = true;
                }
                continue;
            };
            self.keeps |= self.markers.contains(&stmt.location().start_offset());
            let members = body.map(flatten_statements).unwrap_or_default();
            // The instance methods the body defines, and the ones it
            // leaves to the splice: a visibility change keeps only the
            // names of methods the library body still defines.
            let (mut defined, mut left): (Set<String>, Set<String>) = (Set::default(), Set::default());
            if let Some(form) = form {
                for member in &members {
                    if let Some(def) = instance_def(member) {
                        let name = constant_id_str(&def.name()).to_string();
                        if form.helpers && !def_calls_super(&def) { defined.insert(name) } else { left.insert(name) };
                    }
                }
            }
            let dropped = |name: &str| left.contains(name) && !defined.contains(name);
            // Each member's verdict; a `sig` (`None`) goes with the def after it.
            let mut hidden = false;
            let mut verdicts: Vec<Option<bool>> = Vec::new();
            for member in &members {
                if member.as_module_node().is_some() || member.as_class_node().is_some() {
                    // A nested shared module is blanked on its own terms.
                    let at = member.location().start_offset();
                    if member.as_module_node().is_some() && self.spliced.contains_key(&at) {
                        self.walk(std::slice::from_ref(member), true)?;
                    } else {
                        self.keeps |= form.is_some();
                    }
                    verdicts.push(Some(true));
                    continue;
                }
                let Some(form) = form else {
                    verdicts.push(Some(false));
                    continue;
                };
                let verdict = match library_member(member, form, self.kept, &dropped, &mut hidden) {
                    Member::Keep => Some(true),
                    Member::Drop => Some(false),
                    Member::Sig => None,
                    Member::Some(keep) => {
                        let call = member.as_call_node().expect("a call keeps some arguments");
                        self.blank_args(&call, &keep)?;
                        Some(true)
                    }
                };
                verdicts.push(verdict);
            }
            let mut next = false;
            for (member, verdict) in members.iter().zip(verdicts.iter_mut()).rev() {
                let keep = verdict.unwrap_or(next);
                *verdict = Some(keep);
                next = keep;
                if member.as_module_node().is_some() || member.as_class_node().is_some() {
                    continue;
                }
                if keep {
                    self.keeps = true;
                } else {
                    self.blank.push(blank_range(member, self.source));
                }
            }
        }
        Ok(())
    }

    /// Blank the arguments of `call` that `keep` drops (some, not all):
    /// with the separators after them, and before the first one kept a
    /// line break in a call without parentheses is continued with `\`, so
    /// the kept ones stay its arguments. `include A,\n  B` keeping `B` is
    /// `include   \\\n  B`. A line with no byte to continue refuses.
    fn blank_args(&mut self, call: &ruby_prism::CallNode<'_>, keep: &[bool]) -> Result<(), String> {
        let args: Vec<Node<'_>> = call.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default();
        let span = |n: &Node<'_>| (n.location().start_offset(), n.location().end_offset());
        let kept: Vec<usize> = (0..args.len()).filter(|&i| keep.get(i).copied().unwrap_or(true)).collect();
        let (Some(&first), Some(&last)) = (kept.first(), kept.last()) else { return Ok(()) };
        // Before the first kept argument.
        let lead = (span(&args[0]).0, span(&args[first]).0);
        if lead.0 < lead.1 {
            self.blank.push(lead);
            if call.opening_loc().is_none() {
                for at in lead.0..lead.1 {
                    if self.source[at] == b'\n' {
                        if at == lead.0 || self.source[at - 1] == b'\n' {
                            let text = String::from_utf8_lossy(call.location().as_slice()).into_owned();
                            let shown: String = text.lines().next().unwrap_or_default().chars().take(60).collect();
                            return Err(format!(
                                "`{shown}` keeps only some of its arguments in the library module, and its line breaks leave no room to continue the call"
                            ));
                        }
                        self.continued.push(at - 1);
                    }
                }
            }
        }
        // Between kept arguments: a dropped one and the separator after
        // it; the separator after a kept one stays.
        for pair in kept.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            if b > a + 1 {
                self.blank.push((span(&args[a + 1]).0, span(&args[b]).0));
            }
        }
        // After the last kept argument.
        if let Some(end) = args.last().map(|n| span(n).1).filter(|&end| end > span(&args[last]).1) {
            self.blank.push((span(&args[last]).1, end));
        }
        Ok(())
    }
}

/// What a library module's body keeps of one member (`library_member`).
enum Member {
    Keep,
    Drop,
    /// A `sig`: as the def after it.
    Sig,
    /// The call, with only these of its arguments.
    Some(Vec<bool>),
}

/// What a library module (`Mixin::real`) keeps of `member` in its own
/// body: what belongs to the module itself (`module_member`, constants,
/// `extend T::…`), its attributes, its helper methods that do not call
/// `super` when its form keeps helpers (each includer still gets its own
/// copy, `ClassWalk::finish`), a visibility change of the methods it still
/// defines (`dropped` names one left to the splice), and the modules of
/// the `include`s it keeps (`kept`). What the splice gives each includer
/// is blanked: its tests, callbacks, `included do` block, tags, the
/// `extend`s of `ActiveSupport::Concern` and `Declarative`, and the
/// modules it defers.
fn library_member(member: &Node<'_>, form: LibraryForm, kept: &[(usize, Vec<bool>)], dropped: &dyn Fn(&str) -> bool, hidden: &mut bool) -> Member {
    let all = |keep: Vec<bool>| match (keep.iter().all(|&k| k), keep.iter().any(|&k| k)) {
        (true, _) => Member::Keep,
        (false, true) => Member::Some(keep),
        (false, false) => Member::Drop,
    };
    let yes = |keep: bool| if keep { Member::Keep } else { Member::Drop };
    if module_member(member) || member.as_constant_write_node().is_some() {
        return Member::Keep;
    }
    if let Some(level) = bare_visibility(member) {
        *hidden = level != MethodVisibility::Public;
        return Member::Keep;
    }
    if is_test_declaration_shape(member) && !non_public_def(member, *hidden) {
        return Member::Drop;
    }
    // A def that calls `super` is left to the splice: the includer's
    // chain runs through its `RoundhouseSuper…` copies only.
    if let Some(def) = instance_def(member) {
        return yes(form.helpers && !def_calls_super(&def));
    }
    let Some(call) = member.as_call_node().filter(|c| c.receiver().is_none()) else { return Member::Drop };
    let args: Vec<Node<'_>> = call.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default();
    match constant_id_str(&call.name()) {
        "sig" => Member::Sig,
        "include" => {
            let at = call.location().start_offset();
            kept.iter().find(|(o, _)| *o == at).map_or(Member::Drop, |(_, keep)| all(keep.clone()))
        }
        "extend" => yes(args.iter().all(|n| match constant_path_of(n).map(|p| p.join("::")).as_deref() {
            Some("T::Helpers" | "T::Sig" | "T::Generic") => true,
            Some("ActiveSupport::Concern") => form.concern,
            _ => false,
        })),
        "included" => yes(form.concern && call.block().is_some() && args.is_empty()),
        "attr_reader" | "attr_writer" | "attr_accessor" => yes(call.block().is_none() && args.iter().all(|a| a.as_symbol_node().is_some())),
        "private" | "protected" | "public" if call.block().is_none() && args.iter().all(|a| a.as_symbol_node().is_some()) => {
            all(args.iter().map(|a| a.as_symbol_node().is_some_and(|s| !dropped(&String::from_utf8_lossy(s.unescaped())))).collect())
        }
        _ => Member::Drop,
    }
}

/// The bytes a statement takes: from the comment lines right above it
/// (a `#:` signature, a doc comment) to its end, or to the end of a
/// heredoc it opens, whose body follows its last line.
fn blank_range(stmt: &Node<'_>, source: &[u8]) -> (usize, usize) {
    use ruby_prism::Visit;
    struct Heredocs(usize);
    impl Heredocs {
        fn note(&mut self, opening: Option<&[u8]>, closing: Option<ruby_prism::Location<'_>>) {
            if opening.is_some_and(|o| o.starts_with(b"<<")) {
                if let Some(c) = closing {
                    self.0 = self.0.max(c.end_offset());
                }
            }
        }
    }
    impl<'pr> Visit<'pr> for Heredocs {
        fn visit_string_node(&mut self, node: &ruby_prism::StringNode<'pr>) {
            self.note(node.opening_loc().map(|l| l.as_slice()), node.closing_loc());
        }
        fn visit_interpolated_string_node(&mut self, node: &ruby_prism::InterpolatedStringNode<'pr>) {
            self.note(node.opening_loc().map(|l| l.as_slice()), node.closing_loc());
            ruby_prism::visit_interpolated_string_node(self, node);
        }
        fn visit_x_string_node(&mut self, node: &ruby_prism::XStringNode<'pr>) {
            self.note(Some(node.opening_loc().as_slice()), Some(node.closing_loc()));
        }
        fn visit_interpolated_x_string_node(&mut self, node: &ruby_prism::InterpolatedXStringNode<'pr>) {
            self.note(Some(node.opening_loc().as_slice()), Some(node.closing_loc()));
            ruby_prism::visit_interpolated_x_string_node(self, node);
        }
    }
    let mut heredocs = Heredocs(stmt.location().end_offset());
    heredocs.visit(stmt);
    let mut start = stmt.location().start_offset();
    // Back over the indentation, then each whole comment line above.
    let line_start = |at: usize| source[..at].iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
    let here = line_start(start);
    if source[here..start].iter().all(|b| b.is_ascii_whitespace()) {
        start = here;
        while start > 0 {
            let above = line_start(start - 1);
            let text = &source[above..start - 1];
            let trimmed: Vec<u8> = text.iter().copied().skip_while(|b| b.is_ascii_whitespace()).collect();
            if !trimmed.starts_with(b"#") {
                break;
            }
            start = above;
        }
    }
    (start, heredocs.0)
}

/// A module's `def teardown` (`def`), as `Decl::teardown`: every one it
/// or its `included do` block defines must end in `super`.
fn note_teardown(def: &ruby_prism::DefNode<'_>, teardown: &mut Option<bool>) {
    if def.name().as_slice() == b"teardown" {
        *teardown = Some(teardown.unwrap_or(true) && def_ends_in_super(def));
    }
}

/// The methods of `defs` (a module's class methods for its includers)
/// that declare tests when called: a `test`, `generate_test` or
/// `define_method("test_…")` in the body, or a call to another of them.
fn test_macros(defs: &[ruby_prism::DefNode<'_>]) -> Vec<String> {
    let calls: Vec<(String, Vec<String>)> = defs
        .iter()
        .filter(|d| d.receiver().is_none())
        .map(|d| {
            let called = d.body().map(|b| self_calls(&b).into_iter().map(|(n, _)| n).collect()).unwrap_or_default();
            (constant_id_str(&d.name()).to_string(), called)
        })
        .collect();
    let mut found: Vec<String> = Vec::new();
    loop {
        let before = found.len();
        for (name, called) in &calls {
            if !found.contains(name) && called.iter().any(|c| matches!(c.as_str(), "test" | "generate_test") || found.contains(c)) {
                found.push(name.clone());
            }
        }
        if found.len() == before {
            return found;
        }
    }
}

/// The calls `node` makes on `self` (receiverless or `self.`), as (name,
/// offset), outside the bodies of nested methods, classes and modules. A
/// `define_method` whose name starts `test_` is listed as `test`.
fn self_calls(node: &Node<'_>) -> Vec<(String, usize)> {
    use ruby_prism::Visit;
    struct Calls(Vec<(String, usize)>);
    impl<'pr> Visit<'pr> for Calls {
        fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
            if node.receiver().is_none_or(|r| r.as_self_node().is_some()) {
                let mut name = constant_id_str(&node.name()).to_string();
                if name == "define_method" {
                    let first = node.arguments().and_then(|a| a.arguments().iter().next());
                    let text = first.map(|n| String::from_utf8_lossy(n.location().as_slice()).into_owned()).unwrap_or_default();
                    if text.trim_start_matches([':', '"', '\'']).starts_with("test_") {
                        name = "test".into();
                    }
                }
                self.0.push((name, node.location().start_offset()));
            }
            ruby_prism::visit_call_node(self, node);
        }
        fn visit_def_node(&mut self, _: &ruby_prism::DefNode<'pr>) {}
        fn visit_class_node(&mut self, _: &ruby_prism::ClassNode<'pr>) {}
        fn visit_module_node(&mut self, _: &ruby_prism::ModuleNode<'pr>) {}
        fn visit_singleton_class_node(&mut self, _: &ruby_prism::SingletonClassNode<'pr>) {}
    }
    let mut calls = Calls(Vec::new());
    calls.visit(node);
    calls.0
}

/// The level a bare `private` / `protected` / `public` sets for the defs
/// after it.
fn bare_visibility(stmt: &Node<'_>) -> Option<MethodVisibility> {
    let call = stmt.as_call_node()?;
    if call.receiver().is_some() || call.arguments().is_some() || call.block().is_some() {
        return None;
    }
    match constant_id_str(&call.name()) {
        "private" => Some(MethodVisibility::Private),
        "protected" => Some(MethodVisibility::Protected),
        "public" => Some(MethodVisibility::Public),
        _ => None,
    }
}

/// A def Minitest does not collect: one under a bare `private` or
/// `protected` (`hidden`), or `private def …` / `protected def …`.
fn non_public_def(stmt: &Node<'_>, hidden: bool) -> bool {
    if stmt.as_def_node().is_some() {
        return hidden;
    }
    stmt.as_call_node().is_some_and(|call| {
        call.receiver().is_none()
            && call.block().is_none()
            && matches!(constant_id_str(&call.name()), "private" | "protected")
            && call.arguments().is_some_and(|a| {
                let args: Vec<Node<'_>> = a.arguments().iter().collect();
                matches!(args.as_slice(), [arg] if arg.as_def_node().is_some())
            })
    })
}

/// An `included do` block that declares tests, tags or setup for the class.
fn included_block_declares(call: &ruby_prism::CallNode<'_>) -> bool {
    let stmts = call.block().and_then(|b| b.as_block_node()).and_then(|b| b.body()).map(flatten_statements).unwrap_or_default();
    stmts.iter().any(|s| {
        if is_test_declaration_shape(s) || hidden_test_declaration(s).is_some() {
            return true;
        }
        s.as_call_node().is_some_and(|c| {
            c.receiver().is_none() && matches!(constant_id_str(&c.name()), "tag" | "setup" | "teardown")
        })
    })
}

/// Core's `SharedTests` (components/platform/essentials/test/support/
/// shared_tests.rb): a `ClassMethods` defining `included` and
/// `method_prefix`, whose wrappers this module models.
fn is_shared_tests_provider(stmts: &[Node<'_>]) -> bool {
    stmts.iter().any(|s| {
        let Some(m) = s.as_module_node() else { return false };
        if module_name_path(&m).as_deref() != Some(&["ClassMethods".to_string()][..]) {
            return false;
        }
        let defs: Vec<String> = m
            .body()
            .map(flatten_statements)
            .unwrap_or_default()
            .iter()
            .filter_map(|n| n.as_def_node().map(|d| constant_id_str(&d.name()).to_string()))
            .collect();
        ["included", "method_prefix", "setup", "teardown"].iter().all(|d| defs.iter().any(|n| n == d))
    })
}

fn at(file: &str, offset: usize) -> String {
    match super::sources::line_at(file, offset) {
        Some(line) => format!("at {file}:{line}"),
        None => format!("in {file}"),
    }
}

fn message(err: IngestError) -> String {
    match err {
        IngestError::Unsupported { message, .. } => message,
        IngestError::Parse { message, .. } => format!("parse error: {message}"),
        IngestError::Io(e) => format!("io error: {e}"),
    }
}

/// The `def`s of a concern's `module ClassMethods` (or `class_methods do`):
/// ActiveSupport extends each includer with them, so each is a class method
/// the includer's tests and `included do` block find (`MethodReceiver::Class`
/// helpers, as the module's `def self.` ones would be in an `included do`
/// block). Anything else in it is refused by name.
fn ingest_class_methods(mixin: &mut Mixin, body: &[Node<'_>], owner: &ClassId, file: &str, site: &str) -> IngestResult<()> {
    for stmt in body {
        let Some(def) = stmt.as_def_node().filter(|d| d.receiver().is_none()) else {
            let text = String::from_utf8_lossy(stmt.location().as_slice()).into_owned();
            let shown: String = text.lines().next().unwrap_or_default().chars().take(60).collect();
            return Err(IngestError::Unsupported { file: file.into(), message: format!(
                "its `ClassMethods` {site} has `{shown}`, which would extend each includer and the shared-test splice does not model"
            ) });
        };
        let mut method = ingest_library_method(&def, owner, file)?;
        method.receiver = MethodReceiver::Class;
        mixin.helpers.retain(|h| !(h.name == method.name && h.receiver == MethodReceiver::Class));
        mixin.helpers.push(method);
    }
    Ok(())
}

/// Ingest one shared test module's body.
fn ingest_mixin(decl: &Decl, source: &[u8], real: bool) -> Mixin {
    let mut mixin = Mixin {
        name: decl.name.clone(),
        file: decl.file.clone(),
        nesting: decl.nesting.clone(),
        provider: decl.provider,
        items: Vec::new(),
        helpers: Vec::new(),
        constants: Vec::new(),
        included_block: None,
        fatal: None,
        gaps: Vec::new(),
        real,
        body_includes: Vec::new(),
        kept_includes: Vec::new(),
        keeps_helpers: false,
        library_concern: false,
        ancestry: Vec::new(),
        marker: false,
        private_constants: Vec::new(),
    };
    let result = super::prism::parse_silent(source);
    let root = result.node();
    let Some(module) = find_module(&root, decl.offset) else {
        mixin.fatal = Some(format!("module `{}` was not found again in {}", decl.name, decl.file));
        return mixin;
    };
    let stmts = module.body().map(flatten_statements).unwrap_or_default();
    if let Err(err) = walk_mixin(&mut mixin, stmts) {
        mixin.fatal = Some(message(err));
    }
    mixin
}

fn walk_mixin(mixin: &mut Mixin, stmts: Vec<Node<'_>>) -> IngestResult<()> {
    let file = mixin.file.clone();
    let file = file.as_str();
    let owner = ClassId(Symbol::from(mixin.name.as_str()));
    let unsupported = |message: String| IngestError::Unsupported { file: file.into(), message };
    let mut tags = TestTags::default();
    let mut saved: Vec<Vec<(String, String)>> = Vec::new();
    let mut visibility = MethodVisibility::Public;
    let consts = TagConstants::of(&stmts);
    let mut items = Vec::new();
    class_body_items(stmts, file, &consts, &mut items)?;
    for item in items {
        let stmt = match item {
            BodyItem::Stmt(stmt) => stmt,
            BodyItem::TagsBegin(t) => {
                saved.push(tags.tags.clone());
                merge_tags(&mut tags.tags, &t);
                mixin.items.push(Item::Tag("(a `tag … do` block)".into()));
                continue;
            }
            BodyItem::TagsEnd => {
                tags.tags = saved.pop().unwrap_or_default();
                continue;
            }
        };
        let site = at(file, stmt.location().start_offset());
        if let Some((t, None)) = tag_declaration(&stmt, file, &consts)? {
            merge_tags(&mut tags.tags, &t);
            mixin.items.push(Item::Tag(site));
            continue;
        }
        let class_methods =
            stmt.as_module_node().is_some_and(|m| module_name_path(&m).as_deref() == Some(&["ClassMethods".to_string()][..]));
        // A library module's own members stay in it (`Mixin::real`).
        if mixin.real {
            if let Some(hook) = module_hook(&stmt) {
                return Err(unsupported(format!(
                    "it defines `{hook}` {site}, a hook Ruby runs on each includer, which the shared-test splice does not model"
                )));
            }
            if class_methods && !mixin.provider {
                return Err(unsupported(format!(
                    "its `ClassMethods` {site} would extend each includer (ActiveSupport::Concern), which the shared-test splice does not model"
                )));
            }
            if module_member(&stmt) || stmt.as_constant_write_node().is_some() || stmt.as_module_node().is_some() || stmt.as_class_node().is_some() {
                continue;
            }
            if let Some(call) = stmt.as_call_node().filter(|c| c.receiver().is_none() && constant_id_str(&c.name()) == "private_constant") {
                // Every name is read bare from a spliced body, so a symbol or
                // string literal is all it needs; a computed one cannot be told.
                for arg in call.arguments().map(|a| a.arguments().iter().collect::<Vec<_>>()).unwrap_or_default() {
                    let name = arg
                        .as_symbol_node()
                        .map(|s| String::from_utf8_lossy(s.unescaped()).into_owned())
                        .or_else(|| arg.as_string_node().map(|s| String::from_utf8_lossy(s.unescaped()).into_owned()));
                    let Some(name) = name else {
                        return Err(unsupported(format!("`private_constant` {site} with a computed name is not modeled")));
                    };
                    mixin.private_constants.push(Symbol::from(name.as_str()));
                }
                continue;
            }
        }
        if stmt.as_module_node().is_some() || stmt.as_class_node().is_some() {
            if mixin.provider && class_methods {
                continue;
            }
            if let Some(module) = stmt.as_module_node().filter(|_| class_methods) {
                let body = module.body().map(flatten_statements).unwrap_or_default();
                ingest_class_methods(mixin, &body, &owner, file, &site)?;
                continue;
            }
            let name = stmt
                .as_module_node()
                .and_then(|m| module_name_path(&m))
                .or_else(|| stmt.as_class_node().and_then(|c| class_name_path(&c)))
                .map(|p| p.join("::"))
                .unwrap_or_else(|| "?".into());
            return Err(unsupported(format!(
                "it declares `{name}` {site}, a nested class or module the spliced tests would not find"
            )));
        }
        let mut def_visibility = visibility;
        let mut def_node = stmt.as_def_node();
        if let Some(call) = stmt.as_call_node() {
            if call.receiver().is_none() {
                let name = constant_id_str(&call.name());
                let args: Vec<Node<'_>> = call.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default();
                let level = match name {
                    "private" => Some(MethodVisibility::Private),
                    "protected" => Some(MethodVisibility::Protected),
                    "public" => Some(MethodVisibility::Public),
                    _ => None,
                };
                if let Some(level) = level {
                    if args.is_empty() {
                        visibility = level;
                        continue;
                    }
                    if let [arg] = args.as_slice() {
                        if let Some(def) = arg.as_def_node() {
                            def_visibility = level;
                            def_node = Some(def);
                        }
                    }
                    if def_node.is_none() {
                        for arg in &args {
                            let Some(sym) = arg.as_symbol_node() else {
                                return Err(unsupported(format!("`{name}` {site} with a non-symbol argument is not modeled")));
                            };
                            let target = String::from_utf8_lossy(sym.unescaped()).into_owned();
                            if target.starts_with("test_") {
                                return Err(unsupported(format!("`{name} :{target}` {site} changes a test's visibility, which is not modeled")));
                            }
                            for h in mixin.helpers.iter_mut().filter(|h| h.name.as_str() == target) {
                                h.visibility = level;
                            }
                        }
                        continue;
                    }
                } else {
                    match name {
                        "include" => {
                            let refs: Vec<IncludeRef> = args.iter().filter_map(IncludeRef::of).collect();
                            if refs.len() != args.len() {
                                return Err(unsupported(format!("an `include` {site} of something other than a constant is not modeled")));
                            }
                            let after_concern = mixin.items.iter().any(|i| matches!(i, Item::ExtendConcern));
                            mixin.body_includes.push((refs.clone(), call.location().start_offset(), after_concern, site.clone()));
                            mixin.items.push(Item::Include(refs));
                            continue;
                        }
                        "extend" => {
                            for arg in &args {
                                match constant_path_of(arg).map(|p| p.join("::")).as_deref() {
                                    Some("ActiveSupport::Concern") => mixin.items.push(Item::ExtendConcern),
                                    Some("ActiveSupport::Testing::Declarative") => mixin.items.push(Item::ExtendDeclarative),
                                    Some("T::Helpers" | "T::Sig" | "T::Generic") => {}
                                    Some(other) => {
                                        return Err(unsupported(format!(
                                            "`extend {other}` {site} is not modeled: its hooks and class methods would not reach the includer"
                                        )))
                                    }
                                    None => return Err(unsupported(format!("an `extend` {site} of something other than a constant is not modeled"))),
                                }
                            }
                            continue;
                        }
                        // Sorbet only: no effect on what runs.
                        "requires_ancestor" | "abstract!" | "interface!" | "sig" | "private_constant" => continue,
                        "attr_reader" | "attr_writer" | "attr_accessor" => {
                            let mut names = Vec::new();
                            for arg in &args {
                                let Some(sym) = arg.as_symbol_node() else {
                                    return Err(unsupported(format!("`{name}` {site} with a non-symbol argument is not modeled")));
                                };
                                names.push(Symbol::from(String::from_utf8_lossy(sym.unescaped()).as_ref()));
                            }
                            for attr in names {
                                let mut defs = Vec::new();
                                if name != "attr_writer" {
                                    defs.push(super::library_class::synth_attr_reader(&owner, &attr, MethodReceiver::Instance));
                                }
                                if name != "attr_reader" {
                                    defs.push(super::library_class::synth_attr_writer(&owner, &attr, MethodReceiver::Instance));
                                }
                                for mut def in defs {
                                    def.visibility = visibility;
                                    mixin.helpers.retain(|h| h.name != def.name);
                                    mixin.helpers.push(def);
                                }
                            }
                            continue;
                        }
                        "generate_tests" => {
                            mixin.gaps.push(format!(
                                "the tests `generate_tests` {site} declares are not modeled (SharedTests includes them through an anonymous module); they would not run"
                            ));
                            continue;
                        }
                        // `class_methods do … end` (ActiveSupport::Concern) is
                        // `module ClassMethods`, written inline.
                        "class_methods" if call.block().and_then(|b| b.as_block_node()).is_some() && args.is_empty() => {
                            let body = call.block().and_then(|b| b.as_block_node()).and_then(|b| b.body());
                            let body = body.map(flatten_statements).unwrap_or_default();
                            ingest_class_methods(mixin, &body, &owner, file, &site)?;
                            continue;
                        }
                        "included" if call.block().is_some() => {
                            if mixin.included_block.is_some() {
                                return Err(unsupported(format!("a second `included do` block {site} is not modeled")));
                            }
                            mixin.included_block = Some(call.location().start_offset());
                            mixin.items.push(Item::IncludedBlock(site));
                            continue;
                        }
                        "setup" | "teardown" if call.block().is_some() && call.arguments().is_none() => {
                            let body = if name == "setup" { ingest_setup_declaration(&stmt, file)? } else { ingest_teardown_block(&stmt, file)? };
                            if let Some(body) = body {
                                mixin.items.push(if name == "setup" { Item::Setup(body, site) } else { Item::Teardown(body, site) });
                                continue;
                            }
                        }
                        "setup" | "teardown" => {
                            return Err(unsupported(format!("`{name}` {site} in a shared test module is modeled only as `{name} do … end`")));
                        }
                        _ => {}
                    }
                }
            }
        }
        if let Some(def) = def_node {
            let def_name = constant_id_str(&def.name()).to_string();
            if def.receiver().is_none() && def_name.starts_with("test_") && def_visibility != MethodVisibility::Private {
                if def_visibility == MethodVisibility::Protected {
                    return Err(unsupported(format!("a protected `def {def_name}` {site} is not modeled")));
                }
                if let Some(test) = ingest_test_declaration(&def.as_node(), file, &tags, &consts)? {
                    mixin.items.push(Item::Test {
                        name: test.name,
                        method_name: test.method_name,
                        body: test.body,
                        dsl: false,
                        tags_arg: false,
                        at: site,
                    });
                }
                continue;
            }
            if def.receiver().is_some() {
                return Err(unsupported(format!(
                    "`def {}.{def_name}` {site} defines a method on the module, which the spliced tests would not find",
                    String::from_utf8_lossy(def.receiver().map(|r| r.location().as_slice()).unwrap_or_default())
                )));
            }
            let mut method = ingest_library_method(&def, &owner, file)?;
            method.visibility = def_visibility;
            mixin.helpers.retain(|h| !(h.name == method.name && h.receiver == MethodReceiver::Instance));
            mixin.helpers.push(method);
            continue;
        }
        if is_test_declaration_shape(&stmt) {
            match ingest_test_declaration(&stmt, file, &tags, &consts)? {
                Some(test) if test.toggle_flags => {
                    mixin.gaps.push(format!(
                        "the test {:?} {site} has a `flags:` tag, which is not modeled in a shared test module; it would not run",
                        test.name
                    ));
                }
                Some(test) => {
                    let tags_arg = stmt
                        .as_call_node()
                        .and_then(|c| c.arguments())
                        .is_some_and(|a| a.arguments().iter().count() > 1);
                    mixin.items.push(Item::Test {
                        name: test.name,
                        method_name: test.method_name,
                        body: test.body,
                        dsl: true,
                        tags_arg,
                        at: site,
                    });
                }
                None => mixin.gaps.push(format!(
                    "the test declared {site} has a computed name, non-literal tags or a block passed with `&`, which is not modeled; it would not run"
                )),
            }
            continue;
        }
        if let Some(cw) = stmt.as_constant_write_node() {
            let name = Symbol::from(constant_id_str(&cw.name()));
            if super::library_class::is_sorbet_type_alias(&cw.value()) {
                if let Some(value) = super::sorbet_check::alias_value(&cw.value(), file)? {
                    mixin.constants.push((name, value));
                }
                continue;
            }
            let value = ingest_expr(&cw.value(), file)?;
            mixin.constants.push((name, value));
            continue;
        }
        if hidden_test_declaration(&stmt).is_some() {
            mixin.gaps.push(format!(
                "a test declared {site} by a statement ingest does not model (a computed name, a loop or a block); it would not run"
            ));
            continue;
        }
        // Anything else would change what the includer runs or defines.
        let text = String::from_utf8_lossy(stmt.location().as_slice()).into_owned();
        let first = text.lines().next().unwrap_or_default();
        let shown: String = first.chars().take(60).collect();
        return Err(unsupported(format!("`{shown}` {site} is a statement shared test modules do not model")));
    }
    Ok(())
}

fn find_module<'pr>(node: &Node<'pr>, offset: usize) -> Option<ruby_prism::ModuleNode<'pr>> {
    use ruby_prism::Visit;
    struct Find<'pr>(usize, Option<ruby_prism::ModuleNode<'pr>>);
    impl<'pr> Visit<'pr> for Find<'pr> {
        fn visit_module_node(&mut self, node: &ruby_prism::ModuleNode<'pr>) {
            if self.1.is_none() && node.location().start_offset() == self.0 {
                self.1 = node.as_node().as_module_node();
                return;
            }
            ruby_prism::visit_module_node(self, node);
        }
    }
    let mut find = Find(offset, None);
    find.visit(node);
    find.1
}

fn find_call<'pr>(node: &Node<'pr>, offset: usize) -> Option<ruby_prism::CallNode<'pr>> {
    use ruby_prism::Visit;
    struct Find<'pr>(usize, Option<ruby_prism::CallNode<'pr>>);
    impl<'pr> Visit<'pr> for Find<'pr> {
        fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
            if self.1.is_none() && node.location().start_offset() == self.0 {
                self.1 = node.as_node().as_call_node();
                return;
            }
            ruby_prism::visit_call_node(self, node);
        }
    }
    let mut find = Find(offset, None);
    find.visit(node);
    find.1
}

/// The class named `target` (by its full name), and its `Module.nesting`.
fn find_class<'pr>(node: &Node<'pr>, outer: &[String], target: &str) -> Option<(ruby_prism::ClassNode<'pr>, Vec<String>)> {
    let stmts: Vec<Node<'pr>> = if let Some(p) = node.as_program_node() {
        p.statements().body().iter().collect()
    } else if let Some(s) = node.as_statements_node() {
        s.body().iter().collect()
    } else {
        Vec::new()
    };
    for stmt in stmts {
        let (written, body, is_class) = if let Some(c) = stmt.as_class_node() {
            (class_name_path(&c), c.body(), true)
        } else if let Some(m) = stmt.as_module_node() {
            (module_name_path(&m), m.body(), false)
        } else {
            continue;
        };
        let Some(written) = written else { continue };
        let name = match outer.first() {
            Some(o) => format!("{o}::{}", written.join("::")),
            None => written.join("::"),
        };
        let mut nesting = vec![name.clone()];
        nesting.extend(outer.iter().cloned());
        if is_class && name == target {
            return stmt.as_class_node().map(|c| (c, nesting));
        }
        if !target.starts_with(&format!("{name}::")) {
            continue;
        }
        if let Some(body) = body {
            if let Some(found) = find_class(&body, &nesting, target) {
                return Some(found);
            }
        }
    }
    None
}

/// `"#{name.gsub(/SharedTests$/, "").underscore.tr("/", "_")}"`
/// (`SharedTests::ClassMethods#method_prefix`), without inflection
/// acronyms: the name only has to agree with itself, the `<prefix>_setup`
/// the module defines and the call its wrappers make.
fn method_prefix(name: &str) -> String {
    let stem = name.strip_suffix("SharedTests").unwrap_or(name).replace("::", "/");
    let chars: Vec<char> = stem.chars().collect();
    let mut out = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if c.is_ascii_uppercase() && i > 0 {
            let prev = chars[i - 1];
            let next_lower = chars.get(i + 1).is_some_and(|n| n.is_ascii_lowercase());
            if prev.is_ascii_lowercase() || prev.is_ascii_digit() || (prev.is_ascii_uppercase() && next_lower) {
                out.push('_');
            }
        }
        out.push(c);
    }
    out.replace(['-', '/'], "_").to_lowercase()
}

fn call(name: &str) -> Expr {
    Expr::new(
        Span::synthetic(),
        ExprNode::Send { recv: None, method: Symbol::from(name), args: Vec::new(), block: None, parenthesized: false },
    )
}

fn synth_method(name: &str, body: Expr, class: &ClassId) -> MethodDef {
    MethodDef {
        name_span: Span::synthetic(),
        name: Symbol::from(name),
        receiver: MethodReceiver::Instance,
        visibility: MethodVisibility::Public,
        params: Vec::new(),
        unsupported_formals: None,
        has_anonymous_block: false,
        body,
        signature: None,
        effects: crate::effect::EffectSet::default(),
        enclosing_class: Some(class.0.clone()),
        kind: crate::dialect::AccessorKind::Method,
        is_async: false,
        mutates_self: false,
        block_param: None,
    }
}

/// A module holding the definitions a `super` in the class reaches.
fn super_module(name: ClassId, methods: Vec<MethodDef>) -> crate::dialect::LibraryClass {
    crate::dialect::LibraryClass {
        include_spans: Vec::new(),
        conditional_definitions: Vec::new(),
        method_aliases: Vec::new(),
        top_level_constants: Vec::new(),
        name,
        is_module: true,
        parent: None,
        parent_span: Default::default(),
        includes: Vec::new(),
        methods,
        nullable_columns: Vec::new(),
        origin: None,
        constants: Vec::new(),
        unknown_calls: Vec::new(),
        class_ivar_initializers: Vec::new(),
        visibility_overrides: Vec::new(),
    }
}

fn calls_super(e: &Expr) -> bool {
    if matches!(&*e.node, ExprNode::Super { .. }) {
        return true;
    }
    let mut found = false;
    e.node.for_each_child(&mut |c| found |= calls_super(c));
    found
}

/// `Module::CONST` → `CONST` for a constant spliced off `Module` (written
/// as the module's full name or a trailing part of it).
fn unqualify(e: &mut Expr, qualified: &[(String, Symbol)]) {
    if let ExprNode::Const { path } = &mut *e.node {
        if path.len() >= 2 {
            let (last, scope) = path.split_last().expect("two segments");
            let scope = scope.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("::");
            if qualified.iter().any(|(m, c)| c == last && (m == &scope || m.ends_with(&format!("::{scope}")))) {
                *path = vec![last.clone()];
            }
        }
    }
    e.node.for_each_child_mut(&mut |c| unqualify(c, qualified));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The parts of Core's `SharedTests` that identify it.
    const PROVIDER: &str = r##"
module SharedTests
  extend ActiveSupport::Concern
  module ClassMethods
    include ActiveSupport::Testing::Declarative
    def setup(*args, &block)
      return super if is_a?(Class)
      define_method("#{method_prefix}_setup", &block)
    end
    def teardown(*args, &block)
      return super if is_a?(Class)
      define_method("#{method_prefix}_teardown", &block)
    end
    def method_prefix
      @method_prefix ||= name.gsub(/SharedTests$/, "").underscore.tr("/", "_").to_s
    end
    def included(test_case)
    end
  end
end
"##;

    const FOO: &str = r#"
module FooSharedTests
  include SharedTests
  setup do
    @a = 1
  end
  test "one" do
    assert_equal 1, @a
  end
  tag slow: true do
    test "two" do
      assert true
    end
  end
end
"#;

    /// Scan `files`, then ingest and splice the classes of `class_file`.
    fn run(files: &[(&str, &str)], class_file: &str) -> Result<Vec<TestModule>, String> {
        run_with(files, &[], class_file)
    }

    /// `run`, with `support` the test-path files without a test class
    /// (`set_library_context`).
    fn run_with(files: &[(&str, &str)], support: &[&str], class_file: &str) -> Result<Vec<TestModule>, String> {
        let mut mixins = TestMixins::default();
        for (file, source) in files {
            mixins.scan(source.as_bytes(), file);
        }
        mixins.set_library_context(support.iter().map(|s| s.to_string()), Vec::new());
        mixins.finish();
        let source = files.iter().find(|(f, _)| *f == class_file).expect("class file").1;
        let tms = crate::ingest::test::ingest_test_files(source.as_bytes(), class_file).map_err(|e| e.to_string())?;
        tms.into_iter()
            .map(|mut tm| {
                if mixins.applies(&tm) {
                    mixins.splice(&mut tm, source.as_bytes(), class_file).map_err(|e| e.to_string())?;
                }
                Ok(tm)
            })
            .collect()
    }

    fn names(tm: &TestModule) -> Vec<&str> {
        tm.tests.iter().map(|t| t.method_name.as_str()).collect()
    }

    #[test]
    fn module_tests_are_spliced_under_the_includers_names_with_the_wrapper() {
        let class = "class FooTest < ActiveSupport::TestCase\n  include FooSharedTests\nend\n";
        let tms = run(&[("shared_tests.rb", PROVIDER), ("foo_shared_tests.rb", FOO), ("foo_test.rb", class)], "foo_test.rb").unwrap();
        assert_eq!(tms.len(), 1);
        assert_eq!(names(&tms[0]), ["test_one", "test_two_tag:slow:true"]);
        // The wrapper calls `foo_setup`, which comes with the tests.
        let ExprNode::Seq { exprs } = &*tms[0].tests[0].body.node else { panic!("{:?}", tms[0].tests[0].body) };
        assert!(matches!(&*exprs[0].node, ExprNode::Send { method, .. } if method.as_str() == "foo_setup"), "{exprs:?}");
        assert!(tms[0].helpers.iter().any(|h| h.name.as_str() == "foo_setup"));
        // The module is flattened, not included.
        assert!(tms[0].includes.iter().all(|i| i.0.as_str() != "FooSharedTests"), "{:?}", tms[0].includes);
    }

    #[test]
    fn a_persistent_tag_renames_the_wrappers_and_the_includers_own_def_wins() {
        let class = "class FooTest < ActiveSupport::TestCase\n  include FooSharedTests\n  def test_one\n    assert true\n  end\n  tag pg: true\nend\n";
        let tms = run(&[("shared_tests.rb", PROVIDER), ("foo_shared_tests.rb", FOO), ("foo_test.rb", class)], "foo_test.rb").unwrap();
        assert_eq!(names(&tms[0]), ["test_one_tag:pg:true", "test_two_tag:slow:true"]);
        // The class's own `def test_one` replaced the wrapper: no setup call.
        assert!(!format!("{:?}", tms[0].tests[0].body).contains("foo_setup"));
    }

    // An includer's literal loop (`test_unroll`) declares its tests where
    // it stands: the splice keeps them among the shared ones, and one
    // that reuses a shared test's name raises as ActiveSupport does.
    #[test]
    fn an_includers_unrolled_loop_tests_are_kept_by_the_splice() {
        let class = "class FooTest < ActiveSupport::TestCase\n  include FooSharedTests\n  %w[a b].each do |x|\n    test \"loop #{x}\" do\n      assert_equal x, x\n    end\n  end\n  test \"own\" do\n  end\nend\n";
        let tms = run(&[("shared_tests.rb", PROVIDER), ("foo_shared_tests.rb", FOO), ("foo_test.rb", class)], "foo_test.rb").unwrap();
        assert_eq!(names(&tms[0]), ["test_one", "test_two_tag:slow:true", "test_loop_a", "test_loop_b", "test_own"]);
        assert!(format!("{:?}", tms[0].tests[2].body).contains("\"a\""), "{:?}", tms[0].tests[2].body);
        let class = "class FooTest < ActiveSupport::TestCase\n  include FooSharedTests\n  %w[one].each do |x|\n    test x do\n    end\n  end\nend\n";
        let err = run(&[("shared_tests.rb", PROVIDER), ("foo_shared_tests.rb", FOO), ("foo_test.rb", class)], "foo_test.rb").unwrap_err();
        assert!(err.contains("test_one is already defined in FooTest (by shared tests `FooSharedTests`)"), "{err}");
    }

    // A class that gets shared tests defines them for its subclasses: a
    // subclass's `test` of the same name raises
    // (`refuse_inherited_test_redefinitions` reads `defined_tests`).
    #[test]
    fn spliced_tests_are_defined_for_the_subclass_check() {
        let class = "class FooTest < ActiveSupport::TestCase\n  include FooSharedTests\nend\n";
        let tms = run(&[("shared_tests.rb", PROVIDER), ("foo_shared_tests.rb", FOO), ("foo_test.rb", class)], "foo_test.rb").unwrap();
        let defined: Vec<&str> = tms[0].defined_tests.iter().map(|(n, _)| n.as_str()).collect();
        assert!(defined.contains(&"test_one") && defined.contains(&"test_two_tag:slow:true"), "{defined:?}");
        assert!(tms[0].defined_tests.iter().all(|(_, declared)| !declared), "{:?}", tms[0].defined_tests);
    }

    #[test]
    fn a_helper_modules_private_test_named_def_declares_no_tests() {
        // StripePaymentsExtensions::StripeHelper: a nested class, then a
        // private `def test_handler`, which Minitest does not run.
        let helper = "module StripeHelper\n  private\n  def stub_it; end\n  class TestHandler\n  end\n  def test_handler\n    TestHandler\n  end\nend\n";
        let inline = "module InlineHelper\n  private def test_double\n    1\n  end\nend\n";
        let class = "class PayTest < ActiveSupport::TestCase\n  include StripeHelper\n  include InlineHelper\n  test \"pays\" do\n    assert true\n  end\nend\n";
        let tms = run(&[("stripe_helper.rb", helper), ("inline_helper.rb", inline), ("pay_test.rb", class)], "pay_test.rb").unwrap();
        assert_eq!(names(&tms[0]), ["test_pays"]);
        let public = "module PublicHelper\n  class TestHandler\n  end\n  def test_handler\n    TestHandler\n  end\nend\n";
        let class = "class PubTest < ActiveSupport::TestCase\n  include PublicHelper\nend\n";
        let err = run(&[("public_helper.rb", public), ("pub_test.rb", class)], "pub_test.rb").unwrap_err();
        assert!(err.contains("test class `PubTest` refused") && err.contains("TestHandler"), "{err}");
    }

    #[test]
    fn a_call_to_an_included_test_macro_runs_as_the_class_loads() {
        // AccessAndAuth::InputFieldsAuthorizationHelper: a concern whose
        // `ClassMethods#test_input_field_authorization` runs
        // `generate_test`, called in the includer's class body.
        let helper = "module AuthHelper\n  extend ActiveSupport::Concern\n  module ClassMethods\n    include TestGenerator\n    def test_field(key)\n      generate_test(\"checks #{key}\") do\n        assert true\n      end\n    end\n    def test_fields(*keys)\n      keys.each { |k| test_field(k) }\n    end\n    def label(x) = x.to_s\n  end\nend\n";
        let outer = "module OuterHelper\n  extend ActiveSupport::Concern\n  include AuthHelper\nend\n";
        let class = "class InputTest < ActiveSupport::TestCase\n  include OuterHelper\n  test \"own\" do\n    assert true\n  end\n  label(:x)\n  test_field(:vendor)\n  [:a].each do |k|\n    test_fields(k)\n  end\nend\n";
        let files = [("auth_helper.rb", helper), ("outer_helper.rb", outer), ("input_test.rb", class)];
        let mut mixins = TestMixins::default();
        for (file, source) in &files {
            mixins.scan(source.as_bytes(), file);
        }
        mixins.finish();
        let mut tms = crate::ingest::test::ingest_test_files(class.as_bytes(), "input_test.rb").unwrap();
        assert!(mixins.gives_test_macros(&tms[0]));
        assert_eq!(tms[0].body_statements.len(), 3, "{:#?}", tms[0].body_statements);
        assert!(!tms[0].tests_at_load);
        mixins.mark_test_macro_calls(&mut tms[0], class.as_bytes(), "input_test.rb");
        // The macro calls stay carried, and the class lists what they declare as it loads.
        assert!(tms[0].tests_at_load);
        assert_eq!(tms[0].body_statements.len(), 3, "{:#?}", tms[0].body_statements);
        assert_eq!(names(&tms[0]), ["test_own"]);
    }

    #[test]
    fn a_module_whose_test_is_kernel_test_refuses_its_includers() {
        let bare = "module BareTests\n  test \"x\" do\n    assert true\n  end\nend\n";
        let class = "class BareTest < ActiveSupport::TestCase\n  include BareTests\nend\n";
        let err = run(&[("bare_tests.rb", bare), ("bare_test.rb", class)], "bare_test.rb").unwrap_err();
        assert!(err.contains("test class `BareTest` refused") && err.contains("Kernel#test"), "{err}");
    }

    #[test]
    fn a_support_modules_test_macros_are_class_methods_not_shared_tests() {
        let helper = "module AuthHelper\n  extend ActiveSupport::Concern\n  module ClassMethods\n    include TestGenerator\n    def test_field(key)\n      generate_test(\"checks #{key}\") do\n        assert true\n      end\n    end\n  end\nend\n";
        let class = "class AuthTest < ActiveSupport::TestCase\n  include AuthHelper\n  test_field(:vendor)\nend\n";
        let mut mixins = TestMixins::default();
        for (file, source) in [("helper.rb", helper), ("auth_test.rb", class)] {
            mixins.scan(source.as_bytes(), file);
        }
        mixins.set_library_context(["helper.rb".to_string()], Vec::new());
        mixins.finish();
        // The module is no shared test module (`ClassMethods#test_field` is a
        // class method, not a test): no part of it is blanked, the file is
        // carried whole, and the class's macro call is the app's.
        assert!(mixins.library_source("helper.rb", helper.as_bytes()).is_none());
        assert!(mixins.shared_files.is_empty(), "{:?}", mixins.shared_files);
        let tm = crate::ingest::test::ingest_test_files(class.as_bytes(), "auth_test.rb").unwrap().remove(0);
        assert!(mixins.gives_test_macros(&tm));
    }

    #[test]
    fn redeclaring_a_shared_test_is_refused_as_activesupport_raises() {
        let class = "class FooTest < ActiveSupport::TestCase\n  include FooSharedTests\n  test \"one\" do\n    assert true\n  end\nend\n";
        let err = run(&[("shared_tests.rb", PROVIDER), ("foo_shared_tests.rb", FOO), ("foo_test.rb", class)], "foo_test.rb").unwrap_err();
        assert!(err.contains("test_one is already defined in FooTest (by shared tests `FooSharedTests`)"), "{err}");
    }

    #[test]
    fn generated_tests_are_a_named_gap() {
        let generated = "module GenSharedTests\n  include SharedTests\n  generate_tests do |klass|\n    test \"made\" do\n    end\n  end\n  test \"kept\" do\n    assert true\n  end\nend\n";
        let class = "class GenTest < ActiveSupport::TestCase\n  include GenSharedTests\nend\n";
        let err = run(&[("shared_tests.rb", PROVIDER), ("gen_shared_tests.rb", generated), ("gen_test.rb", class)], "gen_test.rb").unwrap_err();
        assert!(err.contains("shared test module `GenSharedTests`") && err.contains("generate_tests"), "{err}");
    }
    #[test]
    fn a_statement_the_splice_does_not_model_refuses_the_includers_by_name() {
        let odd = "module OddSharedTests\n  include SharedTests\n  attr_accessor :x\n  class << self\n    def made; end\n  end\n  test \"one\" do\n    assert true\n  end\nend\n";
        let class = "class OddTest < ActiveSupport::TestCase\n  include OddSharedTests\nend\n";
        let err = run(&[("shared_tests.rb", PROVIDER), ("odd_shared_tests.rb", odd), ("odd_test.rb", class)], "odd_test.rb").unwrap_err();
        assert!(err.contains("test class `OddTest` refused") && err.contains("`class << self`") && err.contains("odd_shared_tests.rb"), "{err}");
    }

    #[test]
    fn attr_accessors_come_with_the_tests() {
        let attrs = "module AttrSharedTests\n  include SharedTests\n  attr_accessor :x\n  private\n  attr_reader :y\n  public\n  test \"one\" do\n    assert_nil x\n  end\nend\n";
        let class = "class AttrTest < ActiveSupport::TestCase\n  include AttrSharedTests\nend\n";
        let tms = run(&[("shared_tests.rb", PROVIDER), ("attr_shared_tests.rb", attrs), ("attr_test.rb", class)], "attr_test.rb").unwrap();
        let helpers: Vec<(&str, MethodVisibility)> = tms[0].helpers.iter().map(|h| (h.name.as_str(), h.visibility)).collect();
        assert!(helpers.contains(&("x", MethodVisibility::Public)) && helpers.contains(&("x=", MethodVisibility::Public)), "{helpers:?}");
        assert!(helpers.contains(&("y", MethodVisibility::Private)), "{helpers:?}");
    }

    /// A concern whose `included do` block registers both callbacks.
    const LOGGING: &str = "module Logging\n  extend ActiveSupport::Concern\n  included do\n    setup do\n      @log = [:setup]\n    end\n    teardown do\n      @log << :teardown\n    end\n  end\nend\n";

    fn helper<'a>(tm: &'a TestModule, name: &str) -> Option<&'a MethodDef> {
        tm.helpers.iter().find(|h| h.name.as_str() == name)
    }

    #[test]
    fn a_spliced_teardown_reaches_the_def_chain_through_super_and_ensures_its_callbacks() {
        let class = "class LogTest < ActiveSupport::TestCase\n  include Logging\n  test \"x\" do\n    assert true\n  end\nend\n";
        let tms = run(&[("logging.rb", LOGGING), ("log_test.rb", class)], "log_test.rb").unwrap();
        let teardown = helper(&tms[0], "teardown").expect("a teardown");
        let ExprNode::BeginRescue { body, ensure: Some(ensure), .. } = &*teardown.body.node else { panic!("{:?}", teardown.body) };
        assert!(matches!(&*body.node, ExprNode::Super { args: None, .. }), "{body:?}");
        let ExprNode::Seq { exprs } = &*ensure.node else { panic!("{ensure:?}") };
        assert!(matches!(&*exprs[0].node, ExprNode::Send { method, .. } if method.as_str() == "__roundhouse_teardown_LogTest_1"), "{exprs:?}");
        assert!(helper(&tms[0], "__roundhouse_teardown_LogTest_1").is_some());
        assert!(tms[0].teardown_def && tms[0].teardown_blocks);
    }

    #[test]
    fn a_concern_the_superclass_included_registers_nothing_again() {
        let class = "class BaseLogTest < ActiveSupport::TestCase\n  include Logging\nend\n\nclass SubLogTest < BaseLogTest\n  include Logging\n  test \"x\" do\n    assert true\n  end\nend\n";
        let tms = run(&[("logging.rb", LOGGING), ("log_test.rb", class)], "log_test.rb").unwrap();
        let sub = tms.iter().find(|t| t.name.0.as_str() == "SubLogTest").unwrap();
        assert!(sub.setup.is_none(), "{:?}", sub.setup);
        assert!(helper(sub, "teardown").is_none() && !sub.teardown_blocks, "{:?}", sub.helpers);
        let base = tms.iter().find(|t| t.name.0.as_str() == "BaseLogTest").unwrap();
        assert!(base.setup.is_some() && helper(base, "teardown").is_some());
    }

    #[test]
    fn teardown_callbacks_in_two_classes_of_a_chain_are_refused_by_name() {
        let files = |class: &'static str| [("logging.rb", LOGGING), ("log_test.rb", class)];
        // The subclass's own block.
        let class = "class BaseLogTest < ActiveSupport::TestCase\n  include Logging\nend\n\nclass SubLogTest < BaseLogTest\n  teardown do\n    @log << :sub\n  end\nend\n";
        let err = run(&files(class), "log_test.rb").unwrap_err();
        assert!(err.contains("test class `SubLogTest` refused") && err.contains("its base class `BaseLogTest` both register teardown callbacks"), "{err}");
        // An include in the subclass, under an unspliced base's block.
        let class = "class BaseLogTest < ActiveSupport::TestCase\n  teardown do\n    @log << :base\n  end\nend\n\nclass SubLogTest < BaseLogTest\n  include Logging\nend\n";
        let err = run(&files(class), "log_test.rb").unwrap_err();
        assert!(err.contains("test class `SubLogTest` refused") && err.contains("`BaseLogTest` both register"), "{err}");
        // A subclass def that does not end in `super` would skip them.
        let class = "class BaseLogTest < ActiveSupport::TestCase\n  include Logging\nend\n\nclass SubLogTest < BaseLogTest\n  def teardown\n    @log << :sub\n  end\nend\n";
        let err = run(&files(class), "log_test.rb").unwrap_err();
        assert!(err.contains("test class `SubLogTest` refused") && err.contains("does not end by calling `super`"), "{err}");
        // One that does keeps them running last.
        let class = "class BaseLogTest < ActiveSupport::TestCase\n  include Logging\nend\n\nclass SubLogTest < BaseLogTest\n  def teardown\n    @log << :sub\n    super\n  end\n  test \"x\" do\n    assert true\n  end\nend\n";
        run(&files(class), "log_test.rb").unwrap();
    }

    #[test]
    fn next_ends_only_its_own_callback_and_other_jumps_are_refused() {
        let skipping = "module Skipping\n  extend ActiveSupport::Concern\n  included do\n    setup do\n      @log = []\n      next if @log.empty?\n      @log << :late\n    end\n    teardown do\n      [1].each do |i|\n        next if i == 1\n      end\n      next :done if @log.empty?\n      @log << :late\n    end\n  end\nend\n";
        let class = "class SkipTest < ActiveSupport::TestCase\n  include Skipping\n  test \"x\" do\n    assert true\n  end\nend\n";
        let tms = run(&[("skipping.rb", skipping), ("skip_test.rb", class)], "skip_test.rb").unwrap();
        let text = |m: &MethodDef| format!("{:?}", m.body);
        let setup = helper(&tms[0], "__roundhouse_setup_SkipTest_1").expect("a framed setup");
        assert!(text(setup).contains("Return") && !text(setup).contains("Next"), "{}", text(setup));
        assert!(format!("{:?}", tms[0].setup).contains("__roundhouse_setup_SkipTest_1"), "{:?}", tms[0].setup);
        let teardown = helper(&tms[0], "__roundhouse_teardown_SkipTest_1").expect("a framed teardown");
        // The inner block's `next` stays a `next`; the callback's own returns.
        assert!(text(teardown).contains("Next") && text(teardown).contains("Return"), "{}", text(teardown));
        for (jump, what) in [("return if @log.empty?", "return"), ("break", "break"), ("yield", "yield")] {
            let module = format!("module Jumping\n  extend ActiveSupport::Concern\n  included do\n    teardown do\n      {jump}\n    end\n  end\nend\n");
            let class = "class JumpTest < ActiveSupport::TestCase\n  include Jumping\nend\n";
            let err = run(&[("jumping.rb", &module), ("jump_test.rb", class)], "jump_test.rb").unwrap_err();
            assert!(err.contains("test class `JumpTest` refused") && err.contains(&format!("callback uses `{what}`")), "{err}");
        }
        // A `return` in a lambda returns from the lambda.
        let module = "module Lambdas\n  extend ActiveSupport::Concern\n  included do\n    teardown do\n      f = -> { return 1 }\n      f.call\n    end\n  end\nend\n";
        let class = "class LambdaTest < ActiveSupport::TestCase\n  include Lambdas\nend\n";
        run(&[("lambdas.rb", module), ("lambda_test.rb", class)], "lambda_test.rb").unwrap();
    }

    #[test]
    fn a_class_def_and_an_included_def_teardown_replace_each_other_in_source_order() {
        let block_def = "module BlockDef\n  extend ActiveSupport::Concern\n  included do\n    def teardown\n      @from = :from_included_block\n    end\n    teardown do\n      @cb = true\n    end\n  end\nend\n";
        let def_part = |class: &str| -> String {
            let tms = run(&[("block_def.rb", block_def), ("def_test.rb", class)], "def_test.rb").unwrap();
            let teardown = helper(&tms[0], "teardown").expect("a teardown");
            let ExprNode::BeginRescue { body, .. } = &*teardown.body.node else { panic!("{:?}", teardown.body) };
            format!("{body:?}")
        };
        let later = def_part("class DefTest < ActiveSupport::TestCase\n  def teardown\n    @from = :from_class_body\n  end\n  include BlockDef\nend\n");
        assert!(later.contains("from_included_block") && !later.contains("from_class_body"), "{later}");
        let earlier = def_part("class DefTest < ActiveSupport::TestCase\n  include BlockDef\n  def teardown\n    @from = :from_class_body\n  end\nend\n");
        assert!(earlier.contains("from_class_body") && !earlier.contains("from_included_block"), "{earlier}");
    }

    #[test]
    fn a_module_below_the_callbacks_needs_a_def_teardown_that_ends_in_super() {
        let modules = "module BadCleanup\n  def teardown\n    super\n    @log << :bad\n  end\nend\n\nmodule GoodCleanup\n  def teardown\n    @log << :good\n    super\n  end\nend\n\nmodule Wrapper\n  include BadCleanup\nend\n";
        let check = |sub: &str| {
            let class = format!("class BaseLogTest < ActiveSupport::TestCase\n  include Logging\nend\n\n{sub}");
            run(&[("logging.rb", LOGGING), ("cleanup.rb", modules), ("log_test.rb", class.as_str())], "log_test.rb")
        };
        let err = |sub: &str| check(sub).unwrap_err();
        let ok = |sub: &str| check(sub).unwrap();
        // Its def runs code after `super`: after the base's callbacks.
        let e = err("class SubLogTest < BaseLogTest\n  include BadCleanup\nend\n");
        assert!(e.contains("test class `SubLogTest` refused") && e.contains("the module `BadCleanup`, which it includes, does not end by calling `super`"), "{e}");
        // Through a module that includes it.
        let e = err("class SubLogTest < BaseLogTest\n  include Wrapper\nend\n");
        assert!(e.contains("the module `BadCleanup`, which it includes"), "{e}");
        // In a class between it and the base (refused itself as well).
        let e = err("class MidLogTest < BaseLogTest\n  include BadCleanup\nend\n");
        assert!(e.contains("test class `MidLogTest` refused"), "{e}");
        let ancestor = |name: &str, callbacks: bool, modules: Vec<ModuleTeardown>| AncestorTeardown {
            name: name.into(),
            callbacks,
            spliced: callbacks,
            def: None,
            modules,
        };
        let bad = ModuleTeardown { name: "BadCleanup".into(), known: true, def: Some(false) };
        let ancestry = Ancestry {
            mixins: Vec::new(),
            classes: vec![ancestor("MidLogTest", false, vec![bad.clone()]), ancestor("BaseLogTest", true, Vec::new())],
        };
        let e = check_teardown_chain(false, None, &[], &ancestry).unwrap_err();
        assert!(e.contains("the module `BadCleanup`, which its base class `MidLogTest` includes, does not end"), "{e}");
        // Not when the class with the callbacks includes it already.
        let ancestry = Ancestry {
            mixins: Vec::new(),
            classes: vec![ancestor("MidLogTest", false, vec![bad.clone()]), ancestor("BaseLogTest", true, vec![bad])],
        };
        check_teardown_chain(false, None, &[], &ancestry).unwrap();
        // One no test file declares may have such a def.
        let e = err("class SubLogTest < BaseLogTest\n  include LibCleanup\nend\n");
        assert!(e.contains("the module `LibCleanup`, which it includes, is not one a test file declares"), "{e}");
        // A def ending in `super`, a gem module without one, and a module
        // the base already includes (Ruby does not include it again) keep
        // the base's callbacks last.
        ok("class SubLogTest < BaseLogTest\n  include GoodCleanup\n  include ActiveSupport::Testing::TimeHelpers\n  test \"x\" do\n    assert true\n  end\nend\n");
        run(
            &[
                ("logging.rb", LOGGING),
                ("cleanup.rb", modules),
                ("log_test.rb", "class BaseLogTest < ActiveSupport::TestCase\n  include BadCleanup\n  include Logging\nend\n\nclass SubLogTest < BaseLogTest\n  include BadCleanup\nend\n"),
            ],
            "log_test.rb",
        )
        .unwrap();
    }

    #[test]
    fn a_concern_def_teardown_below_the_callbacks_is_checked_too() {
        let shared = "module SharedDef\n  extend ActiveSupport::Concern\n  included do\n    setup do\n      @log << :shared\n    end\n  end\n  def teardown\n    super\n    @log << :shared_def\n  end\nend\n";
        let class = "class BaseLogTest < ActiveSupport::TestCase\n  include Logging\nend\n\nclass SubLogTest < BaseLogTest\n  include SharedDef\n  test \"x\" do\n    assert true\n  end\nend\n";
        let err = run(&[("logging.rb", LOGGING), ("shared_def.rb", shared), ("log_test.rb", class)], "log_test.rb").unwrap_err();
        assert!(err.contains("test class `SubLogTest` refused") && err.contains("the module `SharedDef`, which it includes, does not end"), "{err}");
    }

    #[test]
    fn an_included_singleton_def_leaves_the_instance_def_of_its_name() {
        let label = "module SelfLabel\n  extend ActiveSupport::Concern\n  included do\n    setup do\n      @labelled = true\n    end\n    def self.label\n      :class_method\n    end\n  end\nend\n";
        let class = "class LabelTest < ActiveSupport::TestCase\n  def label\n    :instance\n  end\n  include SelfLabel\n  test \"x\" do\n    assert_equal :instance, label\n  end\nend\n";
        let tms = run(&[("self_label.rb", label), ("label_test.rb", class)], "label_test.rb").unwrap();
        let labels: Vec<MethodReceiver> = tms[0].helpers.iter().filter(|h| h.name.as_str() == "label").map(|h| h.receiver).collect();
        assert!(labels.contains(&MethodReceiver::Instance) && labels.contains(&MethodReceiver::Class), "{:?}", tms[0].helpers);
    }

    #[test]
    fn a_super_module_is_included_where_its_shared_module_stands() {
        let shared = "module SharedDef\n  extend ActiveSupport::Concern\n  included do\n    setup do\n      @log = [:setup]\n    end\n  end\n  def teardown\n    super\n    @log << :shared_def\n  end\nend\n";
        let class = "class OrderTest < ActiveSupport::TestCase\n  include SharedDef\n  include PlainDef\n  teardown do\n    @done = true\n  end\n  test \"x\" do\n    assert true\n  end\nend\n";
        let tms = run(&[("shared_def.rb", shared), ("order_test.rb", class)], "order_test.rb").unwrap();
        let includes: Vec<&str> = tms[0].includes.iter().map(|i| i.0.as_str()).collect();
        // `include PlainDef` last: it comes first in the lookup, as natively.
        assert_eq!(includes, ["RoundhouseSuperSharedDef", "PlainDef"]);
    }

    #[test]
    fn setup_symbols_and_visibility_in_an_included_block_are_the_classs() {
        let module = "module SymSetup\n  extend ActiveSupport::Concern\n  included do\n    extend ActiveSupport::Testing::Declarative\n    setup :first_step, :second_step\n    setup(:third_step)\n    test \"from the block\" do\n      assert true\n    end\n    private\n    def hidden\n      :hidden\n    end\n    public def shown\n      :shown\n    end\n    private def other\n      :other\n    end\n  end\n  def first_step\n    @steps = [:first]\n  end\n  def second_step\n    @steps << :second\n  end\n  def third_step\n    @steps << :third\n  end\nend\n";
        let class = "class SymTest < ActiveSupport::TestCase\n  include SymSetup\nend\n";
        let tms = run(&[("sym_setup.rb", module), ("sym_test.rb", class)], "sym_test.rb").unwrap();
        let setup = format!("{:?}", tms[0].setup.as_ref().expect("a rebuilt setup"));
        let at = |name: &str| setup.find(&format!("method: Symbol({name:?})")).unwrap_or_else(|| panic!("{name} in {setup}"));
        assert!(at("first_step") < at("second_step") && at("second_step") < at("third_step"), "{setup}");
        let visibility = |name: &str| helper(&tms[0], name).map(|h| h.visibility);
        assert_eq!(visibility("hidden"), Some(MethodVisibility::Private));
        assert_eq!(visibility("shown"), Some(MethodVisibility::Public));
        assert_eq!(visibility("other"), Some(MethodVisibility::Private));
    }

    #[test]
    fn a_prepended_setup_in_an_included_block_runs_as_the_class_body_loads() {
        let module = "module Early\n  extend ActiveSupport::Concern\n  included do\n    setup do\n      @a = 1\n    end\n    setup(prepend: true) do\n      next unless tagged?\n\n      @b = 2\n    end\n  end\n  def tagged?\n    true\n  end\nend\n";
        let class = "class EarlyTest < ActiveSupport::TestCase\n  include Early\n  test \"x\" do\n    assert true\n  end\nend\n";
        let tms = run(&[("early.rb", module), ("early_test.rb", class)], "early_test.rb").unwrap();
        // The plain `setup` is the class's own setup; the prepended one is
        // carried as written, ahead of the callbacks registered earlier.
        assert!(tms[0].setup.is_some(), "{:?}", tms[0].setup);
        let [statement] = tms[0].body_statements.as_slice() else { panic!("{:#?}", tms[0].body_statements) };
        assert!(!statement.after_setup_blocks && !statement.after_definitions, "{statement:?}");
        let ExprNode::Send { method, args, block, .. } = &*statement.expr.node else { panic!("{:?}", statement.expr) };
        assert_eq!(method.as_str(), "setup");
        assert_eq!(args.len(), 1, "{args:?}");
        assert!(block.is_some());
    }

    #[test]
    fn unmodeled_included_block_statements_are_refused_by_name() {
        for (stmt, why) in [
            ("FLAVOR = 1", "runs `FLAVOR = 1` on the class"),
            ("class Inner\n    end", "runs `class Inner…` on the class"),
            ("setup(prepend: flag) do\n      @x = 1\n    end", "passes arguments or a block argument"),
            ("include helpers_module", "includes a module that is not a constant"),
            ("setup \"named\"", "is not `setup do … end` or `setup :name, …`"),
        ] {
            let module = format!("module Odd\n  extend ActiveSupport::Concern\n  included do\n    setup do\n      @a = 1\n    end\n    {stmt}\n  end\nend\n");
            let class = "class OddTest < ActiveSupport::TestCase\n  include Odd\n  test \"x\" do\n    assert true\n  end\nend\n";
            let err = run(&[("odd.rb", module.as_str()), ("odd_test.rb", class)], "odd_test.rb").unwrap_err();
            assert!(err.contains("test class `OddTest` refused") && err.contains(why), "{stmt}: {err}");
        }
    }
    // `class_eval` runs each statement of an `included do |base|` block on the
    // class where the include stands: a class macro, with or without a block,
    // and the locals after it, are carried as the class body's own, `base` the
    // class itself.
    #[test]
    fn an_included_blocks_class_macros_run_as_the_class_body_loads() {
        let module = "module Shop\n  module ApiHelper\n    extend ActiveSupport::Concern\n\n    included do |base|\n      setup do\n        @a = 1\n      end\n      use_active_record_fixtures\n      let(:thing) do\n        make_thing\n      end\n      documentation(\"Thing\", gid: \"x\") do |controller|\n        controller.ownership(team: \"t\")\n      end\n      klass = base #: as Class[top]\n      Detector.triggered!(klass, :fixtures)\n    end\n  end\n\n  module Detector\n  end\nend\n";
        let class = "class ApiTest < ActiveSupport::TestCase\n  include Shop::ApiHelper\n  test \"x\" do\n    assert true\n  end\nend\n";
        let files = [("support/api_helper.rb", module), ("api_test.rb", class)];
        let tms = run_with(&files, &["support/api_helper.rb"], "api_test.rb").unwrap();
        let shown: Vec<String> = tms[0].body_statements.iter().map(|s| format!("{:?}", s.expr.node)).collect();
        let methods: Vec<Option<&str>> = tms[0]
            .body_statements
            .iter()
            .map(|s| match &*s.expr.node {
                ExprNode::Send { method, .. } => Some(method.as_str()),
                _ => None,
            })
            .collect();
        // `base = self` first (the statement after it reads it), then the macros in order.
        assert!(matches!(&*tms[0].body_statements[0].expr.node, ExprNode::Assign { value, .. } if matches!(&*value.node, ExprNode::SelfRef)) || methods[0] == Some("use_active_record_fixtures"), "{shown:?}");
        let order: Vec<&str> = methods.iter().flatten().copied().collect();
        assert_eq!(order, ["use_active_record_fixtures", "let", "documentation", "triggered!"], "{shown:?}");
        assert!(tms[0].body_statements.iter().any(|s| matches!(&*s.expr.node, ExprNode::Assign { target: crate::expr::LValue::Var { name, .. }, value }
            if name.as_str() == "klass" && matches!(&*value.node, ExprNode::Var { name, .. } if name.as_str() == "base"))), "{shown:?}");
        assert!(tms[0].body_statements.iter().any(|s| matches!(&*s.expr.node, ExprNode::Assign { target: crate::expr::LValue::Var { name, .. }, value }
            if name.as_str() == "base" && matches!(&*value.node, ExprNode::SelfRef))), "{shown:?}");
        // The constant the macro reads is the module's lexical one.
        assert!(tms[0].body_statements.iter().any(|s| consts(&s.expr).iter().any(|c| c == "::Shop::Detector")), "{shown:?}");
        assert!(tms[0].tests_at_load);
    }

    // A concern's `ClassMethods` extends each includer: its defs are class
    // methods of the class the tests are spliced into, and the `included do`
    // block (which runs on the class) calls them.
    const VERIFIED: &str = "module Shop\n  module Verified\n    extend ActiveSupport::Concern\n\n    included do\n      cattr_accessor :entries\n\n      if self < ActiveJob::TestCase\n        self #: as ClassMethods\n          .allowlist do\n            :jobs\n          end\n      end\n\n      old_enabled = Verifier.enabled\n\n      setup do\n        Verifier.enabled = true\n      end\n\n      teardown do\n        Verifier.enabled = old_enabled\n      end\n    end\n\n    # @requires_ancestor: singleton(Object)\n    module ClassMethods\n      def allowlist(&block)\n        class_eval do\n          self.entries ||= []\n          self.entries << block\n        end\n      end\n    end\n  end\n\n  module Verifier\n  end\nend\n";

    #[test]
    fn a_concerns_class_methods_are_class_helpers_of_each_includer() {
        let class = "class VerifiedTest < ActiveSupport::TestCase\n  include Shop::Verified\n  test \"x\" do\n    assert true\n  end\nend\n";
        let files = [("support/verified.rb", VERIFIED), ("verified_test.rb", class)];
        let tms = run_with(&files, &["support/verified.rb"], "verified_test.rb").unwrap();
        let tm = &tms[0];
        let allowlist = tm.helpers.iter().find(|h| h.name.as_str() == "allowlist").expect("the class helper");
        assert_eq!(allowlist.receiver, MethodReceiver::Class);
        // The block's `self.allowlist { … }` guard runs as the class loads.
        let shown: Vec<String> = tm.body_statements.iter().map(|s| format!("{:?}", s.expr.node)).collect();
        assert!(tm.body_statements.iter().any(|s| matches!(&*s.expr.node, ExprNode::If { .. })), "{shown:?}");
        // The teardown reads the block's local `old_enabled`: a closure, so it is
        // defined with `define_method` after the local's write, not a method.
        let write = tm.body_statements.iter().position(|s| matches!(&*s.expr.node, ExprNode::Assign { target: crate::expr::LValue::Var { name, .. }, .. } if name.as_str() == "old_enabled"));
        let define = tm.body_statements.iter().position(|s| matches!(&*s.expr.node, ExprNode::Send { method, .. } if method.as_str() == "define_method"));
        assert!(write.is_some() && define.is_some() && write < define, "{shown:?}");
        assert!(!tm.helpers.iter().any(|h| h.name.as_str().starts_with("__roundhouse_teardown_")), "{:?}", tm.helpers.iter().map(|h| h.name.as_str()).collect::<Vec<_>>());
        let teardown = tm.helpers.iter().find(|h| h.name.as_str() == "teardown").expect("the merged teardown");
        assert!(format!("{:?}", teardown.body).contains("__roundhouse_teardown_VerifiedTest_1"), "{:?}", teardown.body);
        // `class_methods do … end` is the same module, written inline.
        let inline = VERIFIED.replace("module ClassMethods", "class_methods do");
        let files = [("support/verified.rb", inline.as_str()), ("verified_test.rb", class)];
        let tms = run_with(&files, &["support/verified.rb"], "verified_test.rb").unwrap();
        assert!(tms[0].helpers.iter().any(|h| h.name.as_str() == "allowlist" && h.receiver == MethodReceiver::Class));
    }

    #[test]
    fn something_other_than_a_def_in_class_methods_is_refused_by_name() {
        let module = VERIFIED.replace("      def allowlist(&block)", "      include Helper\n\n      def allowlist(&block)");
        let class = "class VerifiedTest < ActiveSupport::TestCase\n  include Shop::Verified\n  test \"x\" do\n    assert true\n  end\nend\n";
        let err = run_with(&[("support/verified.rb", module.as_str()), ("verified_test.rb", class)], &["support/verified.rb"], "verified_test.rb").unwrap_err();
        assert!(err.contains("its `ClassMethods`") && err.contains("include Helper"), "{err}");
    }

    #[test]
    fn an_included_blocks_factory_registration_runs_as_the_class_body_loads() {
        let module = "module Shop\n  module FactoryHelper\n    extend ActiveSupport::Concern\n\n    module Factories\n      class << self\n        def define(klass)\n          klass.factory(:shop, class_name: \"Shop\")\n        end\n      end\n    end\n\n    included do\n      setup do\n        @a = 1\n      end\n      if fixture_factory_definitions.empty?\n        define_factories { Factories.define(self) }\n      else\n        Factories.define(self)\n      end\n    end\n  end\nend\n";
        let class = "class FactoryTest < ActiveSupport::TestCase\n  include Shop::FactoryHelper\n  test \"x\" do\n    assert true\n  end\nend\n";
        let files = [("support/factory_helper.rb", module), ("factory_test.rb", class)];
        let tms = run_with(&files, &["support/factory_helper.rb"], "factory_test.rb").unwrap();
        let [statement] = tms[0].body_statements.as_slice() else { panic!("{:#?}", tms[0].body_statements) };
        assert!(matches!(&*statement.expr.node, ExprNode::If { .. }), "{:?}", statement.expr);
        assert!(!statement.after_definitions, "{statement:?}");
        assert!(consts(&statement.expr).iter().all(|c| c == "::Shop::FactoryHelper::Factories"), "{:?}", consts(&statement.expr));
    }

    // A guard of any other kind is the same statement the class would run
    // itself, carried as written.
    #[test]
    fn a_factory_guard_that_reads_anything_else_is_carried_as_written() {
        let module = "module Odd\n  extend ActiveSupport::Concern\n  included do\n    setup do\n      @a = 1\n    end\n    if ENV[\"X\"]\n      define_factories { }\n    end\n  end\nend\n";
        let class = "class OddTest < ActiveSupport::TestCase\n  include Odd\n  test \"x\" do\n    assert true\n  end\nend\n";
        let tms = run(&[("odd.rb", module), ("odd_test.rb", class)], "odd_test.rb").unwrap();
        let [statement] = tms[0].body_statements.as_slice() else { panic!("{:#?}", tms[0].body_statements) };
        assert!(matches!(&*statement.expr.node, ExprNode::If { .. }), "{:?}", statement.expr);
    }

    /// `Merchandising::CatalogTestHelper`'s shape, in a support file.
    const CATALOG: &str = "module Shop\n  COLORS = [:red].freeze\n\n  module CatalogHelper\n    extend ActiveSupport::Concern\n\n    LIMIT = 3\n\n    #: -> void\n    def build_catalog(limit: LIMIT)\n      Factories.define(COLORS, limit)\n    end\n\n    module Factories\n      class << self\n        def define(colors, limit)\n          [colors, limit]\n        end\n      end\n    end\n\n    @loaded = false\n\n    included do\n      setup do\n        @catalog = Factories.define(COLORS, LIMIT)\n      end\n    end\n  end\nend\n";

    fn consts(e: &Expr) -> Vec<String> {
        let mut out = Vec::new();
        fn walk(e: &Expr, out: &mut Vec<String>) {
            if let ExprNode::Const { path } = &*e.node {
                out.push(path.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("::"));
            }
            e.node.for_each_child(&mut |c| walk(c, out));
        }
        walk(e, &mut out);
        out
    }

    #[test]
    fn a_support_files_module_keeps_its_members_and_its_bodies_read_its_lexical_constants() {
        let class = "module Other\n  COLORS = [:wrong].freeze\n  class CatalogTest < ActiveSupport::TestCase\n    include Shop::CatalogHelper\n    test \"x\" do\n      assert build_catalog\n    end\n  end\nend\n";
        let files = [("support/catalog_helper.rb", CATALOG), ("catalog_test.rb", class)];
        let tms = run_with(&files, &["support/catalog_helper.rb"], "catalog_test.rb").unwrap();
        let tm = &tms[0];
        assert_eq!(tm.includes.iter().map(|i| i.0.as_str()).collect::<Vec<_>>(), ["::Shop::CatalogHelper"]);
        // The module's constants stay in it, not copied to the class.
        assert!(tm.constants.is_empty(), "{:?}", tm.constants);
        let h = helper(tm, "build_catalog").expect("the spliced helper");
        assert_eq!(consts(&h.body), ["::Shop::CatalogHelper::Factories", "::Shop::COLORS"]);
        let default = h.params[0].default.as_ref().expect("a default");
        assert_eq!(consts(default), ["::Shop::CatalogHelper::LIMIT"]);
        let setup = tm.setup.as_ref().expect("the block's setup");
        assert_eq!(consts(setup), ["::Shop::CatalogHelper::Factories", "::Shop::COLORS", "::Shop::CatalogHelper::LIMIT"]);
    }

    #[test]
    fn a_module_with_members_outside_a_support_file_is_still_refused_by_name() {
        let class = "class CatalogTest < ActiveSupport::TestCase\n  include Shop::CatalogHelper\n  test \"x\" do\n    assert true\n  end\nend\n";
        let files = [("support/catalog_helper.rb", CATALOG), ("catalog_test.rb", class)];
        let err = run(&files, "catalog_test.rb").unwrap_err();
        assert!(err.contains("it declares `Factories`") && err.contains("a nested class or module"), "{err}");
    }

    // Core's test-support initializer reopens `ActiveSupport::TestCase` to include
    // `ActiveRecordTestHelper`, whose `included do` block registers `teardown(:sym)`.
    // A test class that includes it again includes nothing (`Concern#append_features`
    // returns early when `base < self`), so that block never runs on it and the
    // class is not refused for a teardown form the splice does not model.
    #[test]
    fn including_a_helper_the_base_test_case_already_includes_is_a_no_op() {
        let helper = "module PodHelper\n  extend ActiveSupport::Concern\n  included do\n    setup(:default_pod)\n    teardown(:reset_pod)\n  end\n\n  def reset_pod; end\nend\n";
        let base = "module ActiveSupport\n  class TestCase\n    include PodHelper\n  end\nend\n";
        let class = "class PodTest < ActiveSupport::TestCase\n  include PodHelper\n\n  test \"x\" do\n    assert true\n  end\nend\n";
        let tms = run(&[("pod_helper.rb", helper), ("base.rb", base), ("pod_test.rb", class)], "pod_test.rb").unwrap();
        assert_eq!(names(&tms[0]), ["test_x"]);
        // Without the base including it, the block runs on the class and `teardown :sym`
        // is refused by name.
        let err = run(&[("pod_helper.rb", helper), ("pod_test.rb", class)], "pod_test.rb").unwrap_err();
        assert!(err.contains("test class `PodTest` refused") && err.contains("teardown"), "{err}");
        // Nor does a class outside the Rails test cases inherit the base's includes.
        let plain = class.replace("ActiveSupport::TestCase", "Minitest::Test");
        let err = run(&[("pod_helper.rb", helper), ("base.rb", base), ("pod_test.rb", plain.as_str())], "pod_test.rb").unwrap_err();
        assert!(err.contains("teardown"), "{err}");
    }

    #[test]
    fn a_library_modules_hooks_and_private_constants_refuse_its_includers_by_name() {
        for (stmt, why) in [
            ("def self.included(base)\n    super\n  end", "it defines `self.included`"),
            ("class << self\n    def extended(base)\n    end\n  end", "it defines `extended (in `class << self`)`"),
            ("module ClassMethods\n    def shout\n    end\n  end", "its `ClassMethods`"),
            ("private_constant Inner.name", "`private_constant` "),
        ] {
            let module = format!("module Hooked\n  extend ActiveSupport::Concern\n  class Inner\n  end\n  {stmt}\n  included do\n    setup do\n      @a = 1\n    end\n  end\nend\n");
            let class = "class HookedTest < ActiveSupport::TestCase\n  include Hooked\n  test \"x\" do\n    assert true\n  end\nend\n";
            let err = run_with(&[("hooked.rb", module.as_str()), ("hooked_test.rb", class)], &["hooked.rb"], "hooked_test.rb").unwrap_err();
            assert!(err.contains("test class `HookedTest` refused") && err.contains(why), "{stmt}: {err}");
        }
    }

    // `private_constant` makes Ruby refuse the scoped read `::Shop::Runner::Stub`
    // a spliced body would get, so the private constant stays a bare name, which
    // the includer's ancestry (the module included) resolves, as the module's
    // own methods read it. The public one is still rooted.
    #[test]
    fn a_library_modules_private_constants_stay_bare_in_spliced_bodies() {
        let module = "module Shop\n  module Runner\n    extend ActiveSupport::Concern\n\n    Stub = Struct.new(:result)\n    Public = Struct.new(:value)\n    private_constant(:Stub)\n\n    def stubbed\n      [Stub.new(1), Public.new(2)]\n    end\n\n    class Builder\n    end\n\n    included do\n      setup do\n        @stub = Stub.new(3)\n      end\n    end\n  end\nend\n";
        let class = "class RunnerTest < ActiveSupport::TestCase\n  include Shop::Runner\n  test \"x\" do\n    assert stubbed\n  end\nend\n";
        let tms = run_with(&[("runner.rb", module), ("runner_test.rb", class)], &["runner.rb"], "runner_test.rb").unwrap();
        let helper = format!("{:?}", tms[0].helpers.iter().find(|h| h.name.as_str() == "stubbed").expect("the helper").body);
        let bare = |s: &str, name: &str| s.contains(&format!("Symbol(\"{name}\")")) && !s.contains(&format!("Symbol(\"Runner\"), Symbol(\"{name}\")"));
        assert!(bare(&helper, "Stub"), "{helper}");
        assert!(helper.contains("Symbol(\"Runner\"), Symbol(\"Public\")"), "{helper}");
        let setup = format!("{:?}", tms[0].setup);
        assert!(bare(&setup, "Stub"), "{setup}");
    }

    #[test]
    fn a_library_modules_file_keeps_its_members_and_blanks_what_is_spliced() {
        let mut mixins = TestMixins::default();
        let shared = "require \"x\"\n\nmodule Plain\n  def plain; end\nend\n\nmodule Shop\n  module Spliced\n    extend ActiveSupport::Concern\n    included do\n      setup do\n        @a = <<~TXT\n          text\n        TXT\n      end\n    end\n  end\nend\n";
        mixins.scan(CATALOG.as_bytes(), "catalog_helper.rb");
        mixins.scan(shared.as_bytes(), "shared.rb");
        mixins.scan(FOO.as_bytes(), "foo_shared_tests.rb");
        mixins.set_library_context(["catalog_helper.rb", "shared.rb", "foo_shared_tests.rb"].map(String::from), Vec::new());
        mixins.finish();
        let library = String::from_utf8(mixins.library_source("catalog_helper.rb", CATALOG.as_bytes()).expect("a library file")).unwrap();
        assert_eq!(library.lines().count(), CATALOG.lines().count(), "{library}");
        let kept: Vec<&str> = library.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
        assert_eq!(
            kept,
            [
                "module Shop",
                "COLORS = [:red].freeze",
                "module CatalogHelper",
                "LIMIT = 3",
                "module Factories",
                "class << self",
                "def define(colors, limit)",
                "[colors, limit]",
                "end",
                "end",
                "end",
                "@loaded = false",
                "end",
                "end",
            ],
            "{library}"
        );
        // A file whose shared modules have no members of their own, with
        // another module beside them, is carried for that one, its shared
        // module left empty (the heredoc's body too).
        let other = String::from_utf8(mixins.library_source("shared.rb", shared.as_bytes()).expect("a library file")).unwrap();
        let kept: Vec<&str> = other.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
        assert_eq!(kept, ["require \"x\"", "module Plain", "def plain; end", "end", "module Shop", "module Spliced", "end", "end"], "{other}");
        // Neither: spliced only, as before.
        assert!(mixins.library_source("foo_shared_tests.rb", FOO.as_bytes()).is_none());

        // A test that reflects on the module's own methods reaches its
        // library body: the helper stays there too, its `#:` with it. A
        // module of the same last name elsewhere does not.
        let helper = "    #: -> void\n    def build_catalog(limit: LIMIT)\n      Factories.define(COLORS, limit)\n    end\n";
        for (receiver, kept) in [("Shop::CatalogHelper", true), ("::Shop::CatalogHelper", true), ("CatalogHelper", true), ("::CatalogHelper", false), ("Other::CatalogHelper", false)] {
            let mut mixins = TestMixins::default();
            let reflects = format!("class ReflectTest < ActiveSupport::TestCase\n  test \"r\" do\n    assert {receiver}.method_defined?(:build_catalog)\n  end\nend\n");
            mixins.scan(CATALOG.as_bytes(), "catalog_helper.rb");
            mixins.scan(reflects.as_bytes(), "reflect_test.rb");
            mixins.set_library_context(["catalog_helper.rb".to_string()], Vec::new());
            mixins.finish();
            let library = String::from_utf8(mixins.library_source("catalog_helper.rb", CATALOG.as_bytes()).expect("a library file")).unwrap();
            assert_eq!(library.contains(helper), kept, "{receiver}: {library}");
        }
    }

    #[test]
    fn a_module_is_reached_by_reflection_and_by_uses_the_splice_does_not_see() {
        let reached = |source: &str| {
            let result = super::super::prism::parse_silent(source.as_bytes());
            let mut out = Set::default();
            reached_modules(&result.node(), &mut out, &mut Set::default());
            let mut out: Vec<String> = out.into_iter().collect();
            out.sort();
            out
        };
        // A test class's and a module's own `include`s are the splice's.
        assert!(reached("module A\n  include Helper\nend\nclass T < ActiveSupport::TestCase\n  include Shop::Helper\nend\n").is_empty());
        for (source, name) in [
            ("class T < ActiveSupport::TestCase\n  test \"x\" do\n    Shop::Helper.instance_methods(false)\n  end\nend\n", "Shop::Helper"),
            ("class T < ActiveSupport::TestCase\n  def x\n    Object.new.extend(::Shop::Helper)\n  end\nend\n", "::Shop::Helper"),
            ("class Fake\n  include Helper\nend\n", "Helper"),
            ("klass = Class.new do\n  include Shop::Helper\nend\n", "Shop::Helper"),
            ("klass.send(:include, Shop::Helper)\n", "Shop::Helper"),
            ("Helper.private_method_defined?(:x)\n", "Helper"),
        ] {
            assert_eq!(reached(source), [name], "{source}");
        }
    }

    #[test]
    fn a_call_keeps_only_some_of_its_arguments_in_place() {
        // (statement, which arguments stay, the statement after)
        for (call, keep, after) in [
            ("include A, B, C", vec![true, false, true], "include A,    C"),
            ("include A, B", vec![false, true], "include    B"),
            ("include A, B", vec![true, false], "include A   "),
            ("include A,\n  B", vec![false, true], "include  \\\n  B"),
            ("include(A,\n  B)", vec![false, true], "include(  \n  B)"),
            ("private :a,\n  :b, # c\n  :d", vec![true, false, true], "private :a,\n         \n  :d"),
        ] {
            let source = format!("{call}\n");
            let result = super::super::prism::parse_silent(source.as_bytes());
            let root = result.node();
            let stmt = root.as_program_node().unwrap().statements().body().iter().next().unwrap();
            let (spliced, kept) = (Map::default(), Vec::new());
            let mut walk = LibraryBlanks { spliced: &spliced, markers: &Set::default(), kept: &kept, source: source.as_bytes(), blank: Vec::new(), continued: Vec::new(), keeps: false };
            walk.blank_args(&stmt.as_call_node().unwrap(), &keep).unwrap();
            let mut out = source.clone().into_bytes();
            for (start, end) in walk.blank {
                out[start..end].iter_mut().filter(|b| **b != b'\n').for_each(|b| *b = b' ');
            }
            walk.continued.iter().for_each(|&at| out[at] = b'\\');
            let out = String::from_utf8(out).unwrap();
            assert_eq!(out.trim_end_matches('\n'), after, "{call}");
            // Still one call of the kept arguments.
            let reparsed = super::super::prism::parse_silent(out.as_bytes());
            let root = reparsed.node();
            let body: Vec<Node<'_>> = root.as_program_node().unwrap().statements().body().iter().collect();
            assert_eq!(body.len(), 1, "{out}");
            let count = body[0].as_call_node().and_then(|c| c.arguments()).map_or(0, |a| a.arguments().iter().count());
            assert_eq!(count, keep.iter().filter(|&&k| k).count(), "{out}");
        }
        // An empty line before the first kept argument leaves nothing to
        // continue the call with.
        let source = "include A,\n\n  B\n";
        let result = super::super::prism::parse_silent(source.as_bytes());
        let root = result.node();
        let stmt = root.as_program_node().unwrap().statements().body().iter().next().unwrap();
        let (spliced, kept) = (Map::default(), Vec::new());
        let mut walk = LibraryBlanks { spliced: &spliced, markers: &Set::default(), kept: &kept, source: source.as_bytes(), blank: Vec::new(), continued: Vec::new(), keeps: false };
        let err = walk.blank_args(&stmt.as_call_node().unwrap(), &[false, true]).unwrap_err();
        assert!(err.contains("keeps only some of its arguments"), "{err}");
    }

    /// The review's shape: a library module that includes a plain module
    /// (in its own ancestry natively) and a concern (deferred to each
    /// includer), with a helper and test of its own.
    const INC: &str = "module Shop\n  module IncHelper\n    extend ActiveSupport::Concern\n    extend ActiveSupport::Testing::Declarative\n    include Consts\n    include DepConcern\n\n    def self.depth\n      DEPTH\n    end\n\n    included do\n      setup do\n        @depth = IncHelper.depth\n      end\n    end\n\n    test \"spliced\" do\n      assert true\n    end\n\n    sig { returns(Integer) }\n    def helper_depth\n      DEPTH\n    end\n\n    def with_super\n      super\n    end\n\n    private\n\n    def test_named_helper\n      DEPTH\n    end\n  end\nend\n";
    const CONSTS: &str = "module Shop\n  module Consts\n    DEPTH = 7\n  end\n\n  module DepConcern\n    extend ActiveSupport::Concern\n    included do\n      setup do\n        @dep = true\n      end\n    end\n  end\nend\n";
    const INC_TEST: &str = "class IncTest < ActiveSupport::TestCase\n  include Shop::IncHelper\n  test \"x\" do\n    assert true\n  end\n\n  def helper_depth\n    super + 1\n  end\nend\n";

    /// `include`s of modules the app and its gems declare (not in a test
    /// file): a plain one stays in the shared module's body, a concern
    /// after `extend ActiveSupport::Concern` is deferred to each includer.
    /// Without the app's modules the include is refused by name.
    #[test]
    fn a_library_module_includes_the_apps_plain_modules_and_defers_its_concerns() {
        let helper = "module Shop\n  module AppIncHelper\n    extend ActiveSupport::Concern\n    include Markets::TestHelper\n    include FixtureFactory::Registry\n\n    @loaded = false\n\n    included do\n      setup do\n        @shop = true\n      end\n    end\n\n    def shop_helper\n      true\n    end\n  end\nend\n";
        let class = "class AppIncTest < ActiveSupport::TestCase\n  include Shop::AppIncHelper\n  test \"x\" do\n    assert shop_helper\n  end\nend\n";
        let scan = |known: bool| {
            let mut mixins = TestMixins::default();
            for (file, source) in [("helper.rb", helper), ("app_inc_test.rb", class)] {
                mixins.scan(source.as_bytes(), file);
            }
            mixins.set_library_context(["helper.rb".to_string()], Vec::new());
            if known {
                mixins.set_app_modules([("Markets::TestHelper".to_string(), false), ("FixtureFactory::Registry".to_string(), true)]);
            }
            mixins.finish();
            let tms = crate::ingest::test::ingest_test_files(class.as_bytes(), "app_inc_test.rb").unwrap();
            let spliced = tms.into_iter().map(|mut tm| mixins.splice(&mut tm, class.as_bytes(), "app_inc_test.rb").map(|_| tm)).collect::<Result<Vec<_>, _>>();
            (mixins, spliced)
        };
        let (_, refused) = scan(false);
        let err = refused.unwrap_err().to_string();
        assert!(err.contains("includes shared tests `Shop::AppIncHelper`") && err.contains("`Markets::TestHelper`"), "{err}");

        let (mixins, spliced) = scan(true);
        let tms = spliced.unwrap();
        assert_eq!(names(&tms[0]), ["test_x"]);
        // The plain module stays in the library body; the concern is the
        // includers' (it comes with the include of the shared module).
        let library = String::from_utf8(mixins.library_source("helper.rb", helper.as_bytes()).expect("a library file")).unwrap();
        let kept: Vec<&str> = library.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
        assert!(kept.contains(&"include Markets::TestHelper"), "{library}");
        assert!(kept.iter().all(|l| !l.contains("FixtureFactory::Registry") || l.starts_with("include") && l.chars().skip(8).all(char::is_whitespace)), "{library}");
        // A concern included before its own `extend ActiveSupport::Concern`
        // is still refused.
        let before = helper.replace("    extend ActiveSupport::Concern\n    include Markets::TestHelper\n    include FixtureFactory::Registry\n", "    include FixtureFactory::Registry\n    extend ActiveSupport::Concern\n");
        let mut mixins = TestMixins::default();
        for (file, source) in [("helper.rb", before.as_str()), ("app_inc_test.rb", class)] {
            mixins.scan(source.as_bytes(), file);
        }
        mixins.set_library_context(["helper.rb".to_string()], Vec::new());
        mixins.set_app_modules([("FixtureFactory::Registry".to_string(), true)]);
        mixins.finish();
        let mut tm = crate::ingest::test::ingest_test_files(class.as_bytes(), "app_inc_test.rb").unwrap().remove(0);
        let err = mixins.splice(&mut tm, class.as_bytes(), "app_inc_test.rb").unwrap_err().to_string();
        assert!(err.contains("comes before an `extend ActiveSupport::Concern`"), "{err}");
    }

    #[test]
    fn a_library_module_keeps_its_plain_includes_and_helpers_and_defers_its_concerns() {
        let mut mixins = TestMixins::default();
        for (file, source) in [("inc.rb", INC), ("consts.rb", CONSTS), ("inc_test.rb", INC_TEST)] {
            mixins.scan(source.as_bytes(), file);
        }
        mixins.set_library_context(["inc.rb", "consts.rb"].map(String::from), Vec::new());
        mixins.finish();
        let library = String::from_utf8(mixins.library_source("inc.rb", INC.as_bytes()).expect("a library file")).unwrap();
        let kept: Vec<&str> = library.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
        // Nothing reaches its helpers in the module: they are the
        // includers' only.
        assert_eq!(kept, ["module Shop", "module IncHelper", "include Consts", "def self.depth", "DEPTH", "end", "private", "end", "end"], "{library}");

        // Reflected on, it keeps the helpers that do not call `super`, and
        // a visibility change keeps the names of those only. A testing
        // gem's module is left to the includers.
        let inc = INC
            .replace("include Consts\n", "include ActiveJob::TestHelper, Consts\n")
            .replace("    private\n", "    private :with_super, :helper_depth\n    private\n");
        let reflects = format!("{INC_TEST}\nclass ReflectTest < ActiveSupport::TestCase\n  test \"r\" do\n    assert Shop::IncHelper.private_method_defined?(:helper_depth)\n  end\nend\n");
        let mut mixins = TestMixins::default();
        for (file, source) in [("inc.rb", inc.as_str()), ("consts.rb", CONSTS), ("inc_test.rb", reflects.as_str())] {
            mixins.scan(source.as_bytes(), file);
        }
        mixins.set_library_context(["inc.rb", "consts.rb"].map(String::from), Vec::new());
        mixins.finish();
        let library = String::from_utf8(mixins.library_source("inc.rb", inc.as_bytes()).expect("a library file")).unwrap();
        let kept: Vec<&str> = library.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
        assert_eq!(
            kept,
            [
                "module Shop",
                "module IncHelper",
                "include                        Consts",
                "def self.depth",
                "DEPTH",
                "end",
                "sig { returns(Integer) }",
                "def helper_depth",
                "DEPTH",
                "end",
                "private              :helper_depth",
                "private",
                "def test_named_helper",
                "DEPTH",
                "end",
                "end",
                "end",
            ],
            "{library}"
        );
        let files = [("inc.rb", inc.as_str()), ("consts.rb", CONSTS), ("inc_test.rb", INC_TEST)];
        let tms = run_with(&files, &["inc.rb", "consts.rb"], "inc_test.rb").unwrap();
        let includes: Vec<&str> = tms[0].includes.iter().map(|i| i.0.as_str()).collect();
        // `include A, B` is `include B; include A`: A comes first, as natively.
        assert_eq!(includes, ["Shop::Consts", "ActiveJob::TestHelper", "::Shop::IncHelper", "RoundhouseSuperShop_IncHelper"]);

        let files = [("inc.rb", INC), ("consts.rb", CONSTS), ("inc_test.rb", INC_TEST)];
        let tms = run_with(&files, &["inc.rb", "consts.rb"], "inc_test.rb").unwrap();
        let tm = &tms[0];
        assert_eq!(names(tm), ["test_x", "test_spliced"]);
        // The class has its own copies, lowered as its test code; the
        // `super` from its own def reaches the module's through its
        // `RoundhouseSuper…`, which comes before the module in the lookup.
        assert!(helper(tm, "test_named_helper").is_some() && helper(tm, "with_super").is_some(), "{:?}", tm.helpers);
        let includes: Vec<&str> = tm.includes.iter().map(|i| i.0.as_str()).collect();
        assert_eq!(includes, ["Shop::Consts", "::Shop::IncHelper", "RoundhouseSuperShop_IncHelper"]);
        // A bare `DEPTH` its bodies read is found through the module's own
        // ancestry (`include Consts`), also from the `RoundhouseSuper…`
        // module nested in the class, whose lexical scopes do not have it.
        assert_eq!(consts(&helper(tm, "test_named_helper").unwrap().body), ["::Shop::Consts::DEPTH"]);
        let sup = tm.inner_classes.iter().find(|c| c.name.0.as_str() == "IncTest::RoundhouseSuperShop_IncHelper").expect("its super module");
        let def = sup.methods.iter().find(|d| d.name.as_str() == "helper_depth").expect("the module's helper_depth");
        assert_eq!(consts(&def.body), ["::Shop::Consts::DEPTH"]);
    }

    #[test]
    fn a_library_modules_include_of_an_unknown_or_unmodeled_module_refuses_its_includers_by_name() {
        let plain_shared = "module Shop\n  module PlainShared\n    extend ActiveSupport::Testing::Declarative\n    test \"p\" do\n      assert true\n    end\n  end\nend\n";
        for (from, to, why) in [
            ("include Consts", "include Gems::Unknown", "whether `Gems::Unknown` is an ActiveSupport::Concern"),
            (
                "extend ActiveSupport::Concern\n    extend ActiveSupport::Testing::Declarative\n    include Consts\n    include DepConcern",
                "include DepConcern\n    extend ActiveSupport::Concern\n    extend ActiveSupport::Testing::Declarative\n    include Consts",
                "comes before an `extend ActiveSupport::Concern`",
            ),
            ("include Consts", "include PlainShared", "is spliced into each includer, not a library module of its own"),
        ] {
            let inc = INC.replace(from, to);
            assert_ne!(inc, INC);
            let files = [("inc.rb", inc.as_str()), ("consts.rb", CONSTS), ("plain.rb", plain_shared), ("inc_test.rb", INC_TEST)];
            let err = run_with(&files, &["inc.rb", "consts.rb", "plain.rb"], "inc_test.rb").unwrap_err();
            assert!(err.contains("test class `IncTest` refused") && err.contains(why), "{to}: {err}");
        }
        // A testing gem's module beside a plain one: the includer gets it.
        let inc = INC.replace("include Consts", "include Consts, ActiveJob::TestHelper");
        let files = [("inc.rb", inc.as_str()), ("consts.rb", CONSTS), ("inc_test.rb", INC_TEST)];
        run_with(&files, &["inc.rb", "consts.rb"], "inc_test.rb").unwrap();
    }

    #[test]
    fn a_shared_module_a_test_reads_by_name_stays_an_empty_marker_the_includer_includes() {
        let shared = "module JobTests\n  extend ActiveSupport::Concern\n\n  included do\n    test \"job\" do\n      assert true\n    end\n  end\nend\n";
        let helper = "module JobHelper\n  def job?\n    self.class.include?(JobTests)\n  end\nend\n";
        let class = "class FooTest < ActiveSupport::TestCase\n  include JobTests\nend\n";
        let build = |files: &[(&str, &str)]| {
            let mut mixins = TestMixins::default();
            for (file, source) in files {
                mixins.scan(source.as_bytes(), file);
            }
            mixins.set_library_context(["job_tests.rb", "job_helper.rb"].map(String::from), Vec::new());
            mixins.finish();
            mixins
        };
        let files = [("job_tests.rb", shared), ("job_helper.rb", helper), ("foo_test.rb", class)];
        let mixins = build(&files);
        // The file keeps the module with no tests, so the constant exists.
        let library = String::from_utf8(mixins.library_source("job_tests.rb", shared.as_bytes()).expect("a marker")).unwrap();
        assert!(library.contains("module JobTests") && !library.contains("test \"job\"") && !library.contains("included do"), "{library}");
        // The includer has the tests spliced once, and includes the marker.
        let tms = run_with(&files, &["job_tests.rb", "job_helper.rb"], "foo_test.rb").unwrap();
        assert_eq!(names(&tms[0]), ["test_job"]);
        assert_eq!(tms[0].includes.iter().map(|i| i.0.as_str()).collect::<Vec<_>>(), ["::JobTests"]);
        // Nothing reads the constant: no marker, no file, as before.
        let quiet = [("job_tests.rb", shared), ("foo_test.rb", class)];
        let mixins = build(&quiet);
        assert!(mixins.library_source("job_tests.rb", shared.as_bytes()).is_none());
        let tms = run_with(&quiet, &["job_tests.rb"], "foo_test.rb").unwrap();
        assert!(tms[0].includes.is_empty(), "{:?}", tms[0].includes);
    }

    #[test]
    fn a_spliced_body_does_not_read_a_constant_only_another_test_classs_file_declares() {
        let helper_file = "module Shop\n  module Thinged\n    extend ActiveSupport::Concern\n    included do\n      setup do\n        @thing = Thing.new\n      end\n    end\n  end\nend\n";
        let unrelated = "module Shop\n  class Thing\n  end\n\n  class UnrelatedTest < ActiveSupport::TestCase\n    test \"u\" do\n      assert true\n    end\n  end\nend\n";
        let class = "class ThingedTest < ActiveSupport::TestCase\n  include Shop::Thinged\n  test \"x\" do\n    assert true\n  end\nend\n";
        let files = [("thinged.rb", helper_file), ("unrelated_test.rb", unrelated), ("thinged_test.rb", class)];
        let tms = run_with(&files, &["thinged.rb"], "thinged_test.rb").unwrap();
        assert_eq!(consts(tms[0].setup.as_ref().expect("the block's setup")), ["Thing"]);
        // Declared in the includer's own file, it is the module's lexical one.
        let own = format!("{unrelated}\n{class}");
        let files = [("thinged.rb", helper_file), ("thinged_test.rb", own.as_str())];
        let tms = run_with(&files, &["thinged.rb"], "thinged_test.rb").unwrap();
        let tm = tms.iter().find(|t| t.name.0.as_str() == "ThingedTest").unwrap();
        assert_eq!(consts(tm.setup.as_ref().expect("the block's setup")), ["::Shop::Thing"]);
    }
}


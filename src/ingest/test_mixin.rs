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
//! What this does not model is said by name: a module whose tests cannot be
//! built, or whose body has a statement the splice does not model (a nested
//! class, `class << self`, an unknown `extend` or DSL call), gets its
//! includers refused; a statement whose tests are not modeled
//! (`generate_tests`, a computed name) records a gap on that module.

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
}

/// A class (with a superclass) whose body includes a module: a test class
/// may get all of its tests from shared test modules.
struct ClassRef {
    name: String,
    nesting: Vec<String>,
    includes: Vec<IncludeRef>,
    /// The constants its body `extend`s, in source order, with each
    /// call's offset.
    extends: Vec<(IncludeRef, usize)>,
    file: String,
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
    /// modules, in source order.
    passthrough: Vec<String>,
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
}

impl TestMixins {
    /// Read the module declarations of one test file.
    pub(super) fn scan(&mut self, source: &[u8], file: &str) {
        // A file with no module and no `include`/`extend` declares
        // nothing the scan records.
        let has = |word: &[u8]| source.windows(word.len()).any(|w| w == word);
        if !has(b"module ") && !has(b"include") && !has(b"extend") {
            return;
        }
        let result = super::prism::parse_silent(source);
        let root = result.node();
        let before = self.decls.len();
        let stmts = root.as_program_node().map(|p| p.statements().body().iter().collect::<Vec<_>>()).unwrap_or_default();
        for stmt in stmts {
            scan_node(&stmt, &[], file, &mut self.decls, &mut self.classes, &mut self.known);
        }
        if self.decls[before..].iter().any(|d| d.declares || d.provider || !d.includes.is_empty()) {
            self.sources.insert(file.to_string(), source.to_vec());
        }
    }

    /// After every test file is scanned: ingest the modules that matter to
    /// a test class's tests — the ones that declare tests, include such a
    /// module, or are `SharedTests` — and build their method tables.
    pub(super) fn finish(&mut self) {
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
            let mut mixin = match self.sources.get(&decl.file) {
                Some(source) => ingest_mixin(decl, source),
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
        let mut tables: Vec<Option<Result<Table, String>>> = vec![None; self.mixins.len()];
        for i in 0..self.mixins.len() {
            let mut stack = Vec::new();
            self.build(i, &mut tables, &mut stack);
        }
        self.tables = tables;

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

    /// Whether `file` declares a shared test module (one that declares
    /// tests, tags or setups for its includers, includes such a module,
    /// or is `SharedTests`). Its modules are spliced into the test
    /// classes that include them; a file of other modules only is a
    /// test-support library file (`ingest::app`).
    pub(super) fn declares_shared_tests(&self, file: &str) -> bool {
        self.shared_files.contains(file)
    }

    /// Each scanned class that includes or extends a module, with its
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
    /// declares tests (`refuse_test_macro_calls`).
    pub(super) fn gives_test_macros(&self, tm: &TestModule) -> bool {
        !self.macro_modules.is_empty() && !self.included_test_macros(tm).is_empty()
    }

    /// Each statement of `tm`'s class body that calls a test-declaring
    /// class method an included module gives it (`test_input_field_
    /// authorization(ProductInput, :vendor, …)`, whose `ClassMethods`
    /// body runs `generate_test`) declares tests ingest does not see: a
    /// named gap, so they are not lost without a word. Strict ingest fails.
    pub(super) fn refuse_test_macro_calls(&self, tm: &TestModule, source: &[u8], file: &str) -> IngestResult<()> {
        let macros = self.included_test_macros(tm);
        if macros.is_empty() {
            return Ok(());
        }
        let result = super::prism::parse_silent(source);
        let root = result.node();
        let Some((class, _)) = find_class(&root, &[], tm.name.0.as_str()) else { return Ok(()) };
        for stmt in class.body().map(flatten_statements).unwrap_or_default() {
            if stmt.as_def_node().is_some()
                || stmt.as_class_node().is_some()
                || stmt.as_module_node().is_some()
                || stmt.as_singleton_class_node().is_some()
            {
                continue;
            }
            let hit = self_calls(&stmt)
                .into_iter()
                .find_map(|(called, at)| macros.iter().find(|(m, _)| *m == called).map(|(m, owner)| (m.clone(), owner.clone(), at)));
            let Some((method, owner, at)) = hit else { continue };
            let line = super::sources::line_at(file, at).map(|l| l.to_string()).unwrap_or_else(|| "?".into());
            super::survey::continue_or_fail(IngestError::Unsupported {
                file: file.into(),
                message: format!(
                    "tests declared by a class method an included module gives the class are not modeled; they will not run (`{}` line {line}: `{method}` of `{owner}`)",
                    tm.name.0
                ),
            })?;
        }
        Ok(())
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
        let tm = super::test::ingest_test_class(&(scope, class), &[], file).map_err(|err| match err {
            IngestError::Unsupported { file, message } => {
                IngestError::Unsupported { message: format!("test class `{name}` refused: {message}"), file }
            }
            other => other,
        })?;
        Ok(Some(tm))
    }

    /// Whether `tm` may include a shared test module: one of its includes
    /// names one, by its last segments.
    pub(super) fn applies(&self, tm: &TestModule) -> bool {
        tm.includes.iter().any(|inc| {
            let written = inc.0.as_str();
            self.by_name.keys().any(|n| n == written || n.ends_with(&format!("::{written}")))
        })
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
                                    flat_passthrough(*k, tables)
                                }
                            }
                            None => vec![self.qualify(r, &m.nesting)],
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
            if target.mro.contains(&k) {
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
        let mut walk = ClassWalk {
            reg: self,
            tables: &self.tables,
            class: ClassId(tm.name.0.clone()),
            nesting,
            t: Table::default(),
            own: Vec::new(),
            base_tags: TestTags::default(),
            tags: TestTags::default(),
            setups: Vec::new(),
            block_setup: false,
            block_helpers: Vec::new(),
            includes_out: Vec::new(),
            used: Vec::new(),
        };
        let stmts = class.body().map(flatten_statements).unwrap_or_default();
        walk.walk(stmts, file, false, Some(&class)).map_err(refuse)?;
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
    nesting: Vec<String>,
    t: Table,
    own: Vec<OwnTest>,
    base_tags: TestTags,
    tags: TestTags,
    setups: Vec<Expr>,
    block_setup: bool,
    block_helpers: Vec<MethodDef>,
    includes_out: Vec<String>,
    /// Every shared test module included, directly or not.
    used: Vec<usize>,
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
                self.own.push(OwnTest { base, block_test: in_block.then_some(test) });
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
                            let refs: Vec<IncludeRef> =
                                call.arguments().map(|a| a.arguments().iter().filter_map(|n| IncludeRef::of(&n)).collect()).unwrap_or_default();
                            self.include(refs)?;
                            continue;
                        }
                        "setup" if call.block().is_some() => {
                            if let Some(body) = ingest_setup_declaration(&stmt, file).map_err(message)? {
                                self.setups.push(body);
                                self.block_setup |= in_block;
                            }
                            continue;
                        }
                        "teardown" if in_block => {
                            return Err(format!(
                                "a `teardown` in an `included do` block ({}) is not modeled",
                                at(file, stmt.location().start_offset())
                            ));
                        }
                        _ => {}
                    }
                }
            }
            if in_block {
                if let Some(def) = stmt.as_def_node() {
                    let method = ingest_library_method(&def, &self.class, file).map_err(message)?;
                    self.block_helpers.retain(|m| m.name != method.name);
                    self.block_helpers.push(method);
                    continue;
                }
                if hidden_test_declaration(&stmt).is_some() {
                    return Err(format!(
                        "a test declared {} by an `included do` statement ingest does not model",
                        at(file, stmt.location().start_offset())
                    ));
                }
            }
        }
        Ok(())
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

    fn include(&mut self, refs: Vec<IncludeRef>) -> Result<(), String> {
        let resolved: Vec<Option<usize>> = refs.iter().map(|r| self.reg.resolve(r, &self.nesting)).collect();
        // The emitted class includes each module after the last: `include
        // A, B` puts A first in the lookup, so it is written last.
        for (r, k) in refs.iter().zip(&resolved).rev() {
            let flat = match k {
                Some(k) => match table_of(self.tables, *k) {
                    Ok(_) => flat_passthrough(*k, self.tables),
                    Err(why) => {
                        return Err(format!("includes shared tests `{}`, not modeled: {why}", self.reg.mixins[*k].name))
                    }
                },
                None => vec![r.written()],
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
        self.walk(stmts, &file, true, None)
    }

    /// The class as Minitest sees it: its tests under their method names.
    fn finish(self, tm: &TestModule) -> Result<TestModule, String> {
        let ClassWalk { reg, tables, class, mut t, own, base_tags, tags, setups, block_setup, block_helpers, includes_out, .. } = self;
        let table = |m: usize| -> &Table { table_of(tables, m).expect("an included module's table is built") };
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
            for h in tm.helpers.iter().chain(block_helpers.iter()) {
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
        ) -> Option<(String, Expr)> {
            for (pos, &m) in mro.iter().enumerate() {
                match table_of(tables, m).ok()?.slot(name) {
                    None => continue,
                    Some(Slot::Own(i)) => {
                        let Item::Test { name, body, .. } = &reg.mixins[m].items[i] else { return None };
                        return Some((name.clone(), body.clone()));
                    }
                    Some(Slot::Wrapper(k)) => {
                        let (display, inner) = resolve(name, &mro[pos + 1..], reg, tables, wrap)?;
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
                    let Some((display, inner)) = resolve(original, &t.mro, reg, tables, &wrap) else {
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
                let Some((display, body)) = resolve(name, &t.mro[pos..], reg, tables, &wrap) else { continue };
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
        // lookup order, so the chain runs as it does natively.
        let mut helpers = tm.helpers.clone();
        let mut chains: Vec<(String, Vec<(Option<usize>, MethodDef)>)> = Vec::new();
        let add = |owner: Option<usize>, h: MethodDef, chains: &mut Vec<(String, Vec<(Option<usize>, MethodDef)>)>| {
            let name = h.name.to_string();
            match chains.iter_mut().find(|(n, _)| *n == name) {
                Some((_, defs)) => defs.push((owner, h)),
                None => chains.push((name, vec![(owner, h)])),
            }
        };
        for h in &tm.helpers {
            add(None, h.clone(), &mut chains);
        }
        for h in block_helpers {
            if !tm.helpers.iter().any(|own| own.name == h.name) {
                add(None, h, &mut chains);
            }
        }
        for &m in &t.mro {
            let mixin = &reg.mixins[m];
            for h in &mixin.helpers {
                add(Some(m), h.clone(), &mut chains);
            }
            let prefix = method_prefix(&mixin.name);
            let mt = table(m);
            if let Some(body) = &mt.setup_fn {
                add(Some(m), synth_method(&format!("{prefix}_setup"), body.clone(), &class), &mut chains);
            }
            if let Some(body) = &mt.teardown_fn {
                add(Some(m), synth_method(&format!("{prefix}_teardown"), body.clone(), &class), &mut chains);
            }
        }
        let mut supers: Vec<(usize, Vec<MethodDef>)> = Vec::new();
        for (name, defs) in chains {
            if seen.contains(&name) {
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
        let mut super_includes: Vec<String> = Vec::new();
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
            super_includes.push(short);
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
                constants.push((name.clone(), value.clone()));
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
        out.includes = includes_out.into_iter().chain(super_includes).map(|i| ClassId(Symbol::from(i.as_str()))).collect();
        out.inner_classes = inner_classes;
        if block_setup {
            out.setup = Some(Expr::new(Span::synthetic(), ExprNode::Seq { exprs: setups }));
        }
        Ok(out)
    }
}

/// The modules other than shared test ones that including `k` brings,
/// in the order the emitted class includes them: a Concern's deferred
/// dependencies first, as `Concern#append_features` includes them first.
fn flat_passthrough(k: usize, tables: &Tables) -> Vec<String> {
    let Ok(kt) = table_of(tables, k) else { return Vec::new() };
    let mut out: Vec<String> = Vec::new();
    for &d in &kt.deps {
        for inc in flat_passthrough(d, tables) {
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

fn scan_node(node: &Node<'_>, outer: &[String], file: &str, out: &mut Vec<Decl>, classes: &mut Vec<ClassRef>, known: &mut Set<String>) {
    if let Some(c) = node.as_class_node() {
        let Some(written) = class_name_path(&c) else { return };
        let name = match outer.first() {
            Some(o) => format!("{o}::{}", written.join("::")),
            None => written.join("::"),
        };
        let mut nesting = vec![name.clone()];
        nesting.extend(outer.iter().cloned());
        let stmts = c.body().map(flatten_statements).unwrap_or_default();
        let mut includes = Vec::new();
        let mut extends = Vec::new();
        for stmt in &stmts {
            if stmt.as_class_node().is_some() || stmt.as_module_node().is_some() {
                scan_node(stmt, &nesting, file, out, classes, known);
                continue;
            }
            if let Some(call) = stmt.as_call_node() {
                if call.receiver().is_none() && constant_id_str(&call.name()) == "include" {
                    if let Some(args) = call.arguments() {
                        includes.extend(args.arguments().iter().filter_map(|n| IncludeRef::of(&n)));
                    }
                }
                if call.receiver().is_none() && constant_id_str(&call.name()) == "extend" {
                    if let Some(args) = call.arguments() {
                        let at = call.location().start_offset();
                        extends.extend(args.arguments().iter().filter_map(|n| IncludeRef::of(&n)).map(|r| (r, at)));
                    }
                }
            }
        }
        if c.superclass().is_some() && (!includes.is_empty() || !extends.is_empty()) {
            classes.push(ClassRef { name, nesting, includes, extends, file: file.to_string() });
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
    };
    let mut class_defs: Vec<ruby_prism::DefNode<'_>> = Vec::new();
    // A `def test_…` after a bare `private`/`protected`, or written as
    // `private def test_…`, is a helper: Minitest runs public methods only
    // (`public_instance_methods` in Core's `methods_matching`).
    let mut hidden = false;
    for stmt in &stmts {
        if let Some(inner) = stmt.as_module_node() {
            if module_name_path(&inner).as_deref() == Some(&["ClassMethods".to_string()][..]) {
                class_defs.extend(inner.body().map(flatten_statements).unwrap_or_default().iter().filter_map(|n| n.as_def_node()));
            }
            scan_node(stmt, &nesting, file, out, classes, known);
            continue;
        }
        if stmt.as_class_node().is_some() {
            scan_node(stmt, &nesting, file, out, classes, known);
            continue;
        }
        decl.content = true;
        if let Some(level) = bare_visibility(stmt) {
            hidden = level != MethodVisibility::Public;
        }
        if !non_public_def(stmt, hidden) && (is_test_declaration_shape(stmt) || hidden_test_declaration(stmt).is_some()) {
            decl.declares = true;
        }
        if let Some(call) = stmt.as_call_node() {
            if call.receiver().is_none() {
                match constant_id_str(&call.name()) {
                    "include" => {
                        if let Some(args) = call.arguments() {
                            decl.includes.extend(args.arguments().iter().filter_map(|n| IncludeRef::of(&n)));
                        }
                    }
                    "class_methods" => {
                        let body = call.block().and_then(|b| b.as_block_node()).and_then(|b| b.body());
                        class_defs.extend(body.map(flatten_statements).unwrap_or_default().iter().filter_map(|n| n.as_def_node()));
                    }
                    "included" | "tag" | "setup" | "teardown" | "generate_tests" if call.block().is_some() => {
                        if constant_id_str(&call.name()) != "included" || included_block_declares(&call) {
                            decl.declares = true;
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    decl.macros = test_macros(&class_defs);
    out.push(decl);
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

/// Ingest one shared test module's body.
fn ingest_mixin(decl: &Decl, source: &[u8]) -> Mixin {
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
        if stmt.as_module_node().is_some() || stmt.as_class_node().is_some() {
            if mixin.provider && stmt.as_module_node().is_some_and(|m| module_name_path(&m).as_deref() == Some(&["ClassMethods".to_string()][..])) {
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
        let mut mixins = TestMixins::default();
        for (file, source) in files {
            mixins.scan(source.as_bytes(), file);
        }
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
    fn a_call_to_an_included_test_macro_is_a_named_gap() {
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
        let tms = crate::ingest::test::ingest_test_files(class.as_bytes(), "input_test.rb").unwrap();
        assert!(mixins.gives_test_macros(&tms[0]));
        let err = mixins.refuse_test_macro_calls(&tms[0], class.as_bytes(), "input_test.rb").unwrap_err().to_string();
        assert!(err.contains("tests declared by a class method an included module gives the class are not modeled") && err.contains("`test_field` of `AuthHelper`"), "{err}");
        crate::ingest::survey::activate();
        mixins.refuse_test_macro_calls(&tms[0], class.as_bytes(), "input_test.rb").unwrap();
        let gaps: Vec<String> = crate::ingest::survey::drain().iter().map(|e| e.to_string()).collect();
        assert_eq!(gaps.len(), 2, "{gaps:#?}");
        assert!(gaps[1].contains("`test_fields` of `AuthHelper`"), "{}", gaps[1]);
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
}

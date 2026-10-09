//! Class methods that declare tests where a test class body calls them
//! ("test macros"), expanded at the call.
//!
//! Core writes a family of tests once, as a class method whose body is
//! TestGenerator's `generate_test` (or `test`) calls, and calls it once
//! per subject:
//!
//! ```ruby
//! class << self
//!   def generate_check_access_tests(key, sufficient_permissions:, insufficient_permissions:)
//!     location = T.must(caller_locations.find { … })
//!     generate_test("check_access returns true when input includes #{key} and user has " \
//!       "permission to #{sufficient_permissions.to_sentence}", location) do
//!       @user.permissions = sufficient_permissions
//!       …
//!     end
//!   end
//! end
//! generate_check_access_tests(:price, sufficient_permissions: [:products], insufficient_permissions: [])
//! ```
//!
//! The method is found as Ruby's singleton lookup finds it: in the test
//! class body (`class << self`, `def self.m`), else in the modules the
//! body includes (a concern's `module ClassMethods` or `class_methods
//! do`, after the concerns it depends on) or extends, the latest first,
//! from any source registered before the test class is ingested (the
//! app's and lib's, and the test files sorted before this one). A
//! method of the name that declares nothing (a plain method) shadows a
//! macro it comes before; a body statement that may define singleton
//! methods another way (`define_singleton_method`, an `extend` of a
//! module not known), a module a test source registered later declares,
//! or a superclass's macro, makes the call a named gap.
//! Each call is expanded as Ruby runs it: the arguments are bound to the
//! parameters (`test_unroll::bind_call`), each test is named as the
//! macro names it (`#{key}`, `to_sentence`), and its body starts by
//! binding the parameters its block closes over (`key = :price`).
//!
//! TestGenerator's `generate_test(name, location = nil, tags: nil, &b)`
//! is `test(name, tags, &b)` plus an alias whose wrapper only `send`s
//! to it; `location` places the generated method in backtraces. A
//! macro is expanded only when its body is nothing but such calls and
//! local assignments only ever passed as their `location`, and its
//! blocks neither read those locals nor assign a parameter. A macro
//! from another file is expanded only when every constant its blocks
//! name resolves to the same declaration from the test class as from
//! the macro (`test_unroll::consts`).
//!
//! A call that is not expanded is named, here (whether the class body,
//! a module it includes or extends, or an ancestor gives the method),
//! or by `TestMixins` for a test-dir module's that this module neither
//! expanded nor named (`macro_call_expanded`).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use ruby_prism::Node;

use crate::dialect::Test;
use crate::span::FileId;
use crate::ClassId;

use super::super::sources;
use super::super::util::{constant_id_str, flatten_statements};
use super::super::{IngestError, IngestResult};
use super::test_unroll::{self, consts};
use super::{dsl_test_tagged, hidden_test_declaration, TagConstants, TestTags};

/// A class method a module gives the singleton class of a class that
/// includes it (a concern's `ClassMethods`) or extends it.
#[derive(Clone)]
struct Given {
    name: String,
    kind: GivenKind,
}

#[derive(Clone)]
enum GivenKind {
    /// Its body declares tests: (def start, def end, the lexical scopes
    /// of its body).
    Macro(usize, usize, Vec<String>),
    /// It declares none itself: the first method its body calls.
    Plain(Option<String>),
}

/// The methods a module body defines for its includer's (or extender's)
/// singleton class.
#[derive(Clone, Default)]
struct Methods {
    given: Vec<Given>,
    /// It includes or extends TestGenerator, so `generate_test` in a
    /// macro is its.
    generator: bool,
    /// A statement that may define more (`include X`, `define_method`,
    /// `alias_method`).
    more: Option<String>,
}

/// A concern (`extend ActiveSupport::Concern`) from a source.
struct Concern {
    module: String,
    path: String,
    fingerprint: u64,
    /// Its `module ClassMethods` or `class_methods do`.
    methods: Methods,
    /// The modules its body includes: ActiveSupport::Concern includes
    /// the concerns among them in the includer before extending it.
    dependencies: Vec<Vec<String>>,
    /// The lexical scopes inside it.
    cref: Vec<String>,
    /// A statement that changes the includer's singleton class other
    /// than by `ClassMethods` (in its `included do`, or its own hook).
    hook: Option<String>,
}

thread_local! {
    static CONCERNS: RefCell<Vec<Concern>> = const { RefCell::new(Vec::new()) };
    /// The registry `CONCERNS` was read from: its first path, and how
    /// many of its files have been read.
    static SCANNED: RefCell<(Option<String>, u32)> = const { RefCell::new((None, 0)) };
    /// Calls expanded or named here, as (file, its fingerprint, call
    /// start).
    static EXPANDED: RefCell<Vec<(String, u64, usize)>> = const { RefCell::new(Vec::new()) };
    /// Methods whose body declares tests, by name (the first file that
    /// defines one), from every registered source and test source on
    /// disk: (registry first path, files read, late root read, names).
    #[allow(clippy::type_complexity)]
    static MACRO_NAMES: RefCell<(Option<String>, u32, Option<Option<String>>, HashMap<String, String>)> =
        RefCell::new((None, 0, None, HashMap::new()));
    /// `foreign_use` answers: (registry first path, files) → (name, file) → answer.
    #[allow(clippy::type_complexity)]
    static FOREIGN: RefCell<((Option<String>, u32), HashMap<(String, String), Option<String>>)> =
        RefCell::new(((None, 0), HashMap::new()));
}

fn fingerprint(text: &str) -> u64 {
    // FNV-1a: tells a stale entry (another app ingested on this thread)
    // from the file registered now.
    text.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3))
}

/// Was the call at `offset` of `file` expanded, or named, here?
pub(super) fn expanded(file: &str, offset: usize) -> bool {
    let Some(now) = sources::with_text(file, fingerprint) else { return false };
    EXPANDED.with(|e| e.borrow().iter().any(|(f, print, at)| f == file && *print == now && *at == offset))
}

/// The registry: its first path and how many files.
fn registry() -> (Option<String>, u32) {
    // Ids are dense from 1: bisect for the last.
    let exists = |n: u32| n == 0 || sources::path_of(FileId(n)).is_some();
    let mut high = 1u32;
    while exists(high) && high < u32::MAX / 2 {
        high *= 2;
    }
    let mut low = high / 2;
    while high - low > 1 {
        let mid = low + (high - low) / 2;
        if exists(mid) {
            low = mid;
        } else {
            high = mid;
        }
    }
    (sources::path_of(FileId(1)), low)
}

/// Read the concerns of the sources registered since the last call
/// (every registered source, once per ingest). One only a test source
/// not registered yet declares is not known (`resolve_module`).
fn sync_concerns() {
    let first = sources::path_of(FileId(1));
    SCANNED.with(|scanned| {
        let mut scanned = scanned.borrow_mut();
        if scanned.0 != first || (scanned.1 > 0 && sources::path_of(FileId(scanned.1)).is_none()) {
            *scanned = (first, 0);
            CONCERNS.with(|c| c.borrow_mut().clear());
        }
        let mut id = scanned.1 + 1;
        while let Some(path) = sources::path_of(FileId(id)) {
            let found = sources::with_text(&path, |text| {
                if text.contains("ActiveSupport::Concern") {
                    let result = ruby_prism::parse(text.as_bytes());
                    record_concerns(&result.node(), &path, fingerprint(text))
                } else {
                    Vec::new()
                }
            });
            CONCERNS.with(|c| {
                let mut c = c.borrow_mut();
                c.retain(|old| old.path != path);
                c.extend(found.unwrap_or_default());
            });
            scanned.1 = id;
            id += 1;
        }
    });
}

/// The concerns `file` (`root`, whose text has `print`) declares.
fn record_concerns(root: &Node<'_>, file: &str, print: u64) -> Vec<Concern> {
    struct Walk<'a> {
        file: &'a str,
        print: u64,
        scopes: Vec<(String, bool)>,
        found: Vec<Concern>,
    }
    impl Walk<'_> {
        fn statements(&mut self, node: &Node<'_>) {
            for stmt in flatten_statements_of(node) {
                if let Some(module) = stmt.as_module_node() {
                    let Some((segs, rooted)) = consts::segments(&module.constant_path()) else { continue };
                    let (name, exact) = match self.scopes.last() {
                        _ if rooted => (segs.join("::"), true),
                        None => (segs.join("::"), true),
                        Some((outer, exact)) => (format!("{outer}::{}", segs.join("::")), *exact && segs.len() == 1),
                    };
                    let body = module.body().map(flatten_statements).unwrap_or_default();
                    if exact {
                        let mut cref: Vec<String> = self.scopes.iter().map(|(n, _)| n.clone()).collect();
                        cref.push(name.clone());
                        if let Some(concern) = concern_of(&body, &name, &cref, self.file, self.print) {
                            self.found.push(concern);
                        }
                    }
                    self.scopes.push((name, exact));
                    if let Some(body) = module.body() {
                        self.statements(&body);
                    }
                    self.scopes.pop();
                }
            }
        }
    }
    let mut walk = Walk { file, print, scopes: Vec::new(), found: Vec::new() };
    if let Some(program) = root.as_program_node() {
        walk.statements(&program.statements().as_node());
    }
    walk.found
}

fn flatten_statements_of<'pr>(node: &Node<'pr>) -> Vec<Node<'pr>> {
    match node.as_statements_node() {
        Some(s) => s.body().iter().collect(),
        None => Vec::new(),
    }
}

/// The concern whose body is `body` (an `extend ActiveSupport::Concern`
/// module): its `module ClassMethods` or `class_methods do`, the modules
/// it includes, and a hook that changes its includer's singleton class.
fn concern_of(body: &[Node<'_>], module: &str, cref: &[String], file: &str, print: u64) -> Option<Concern> {
    let is_concern = body.iter().any(|s| {
        s.as_call_node().is_some_and(|c| {
            c.receiver().is_none()
                && c.name().as_slice() == b"extend"
                && c.arguments().is_some_and(|a| {
                    a.arguments().iter().any(|arg| {
                        consts::segments(&arg).is_some_and(|(segs, _)| segs.join("::") == "ActiveSupport::Concern")
                    })
                })
        })
    });
    if !is_concern {
        return None;
    }
    let mut methods = Methods::default();
    let mut dependencies = Vec::new();
    let mut hook = None;
    for stmt in body {
        // A `module ClassMethods` body's constants are looked up from
        // it; a `class_methods` block's from the concern.
        if let Some(m) = stmt.as_module_node() {
            if consts::segments(&m.constant_path()).is_some_and(|(s, rooted)| !rooted && s == ["ClassMethods"]) {
                let mut scopes = cref.to_vec();
                scopes.push(format!("{module}::ClassMethods"));
                methods_of(&m.body().map(flatten_statements).unwrap_or_default(), &scopes, &mut methods);
            }
            continue;
        }
        if let Some(def) = stmt.as_def_node().filter(|d| d.receiver().is_some()) {
            if matches!(def.name().as_slice(), b"included" | b"append_features" | b"prepended" | b"prepend_features") {
                hook.get_or_insert_with(|| first_line(stmt));
            }
            continue;
        }
        let Some(call) = stmt.as_call_node().filter(|c| c.receiver().is_none()) else { continue };
        match call.name().as_slice() {
            b"class_methods" => {
                if let Some(body) = call.block().and_then(|b| b.as_block_node()).and_then(|b| b.body()) {
                    methods_of(&flatten_statements(body), cref, &mut methods);
                }
            }
            b"include" => {
                for arg in call.arguments().map(|a| a.arguments().iter().collect::<Vec<_>>()).unwrap_or_default() {
                    match consts::segments(&arg) {
                        Some((segs, false)) => dependencies.push(segs),
                        _ => {
                            hook.get_or_insert_with(|| first_line(stmt));
                        }
                    }
                }
            }
            b"included" | b"prepended" => {
                if let Some(op) = call.block().and_then(|b| b.as_block_node()).and_then(|b| b.body()).and_then(|b| singleton_op(&b)) {
                    hook.get_or_insert(op);
                }
            }
            _ => {}
        }
    }
    Some(Concern {
        module: module.to_string(),
        path: file.to_string(),
        fingerprint: print,
        methods,
        dependencies,
        cref: cref.to_vec(),
        hook,
    })
}

/// Record the methods of a module body's `items` (written where the
/// lexical scopes are `scopes`) in `out`.
fn methods_of(items: &[Node<'_>], scopes: &[String], out: &mut Methods) {
    for item in items {
        if let Some(def) = item.as_def_node() {
            if def.receiver().is_none() {
                let loc = def.location();
                let kind = if def.body().is_some_and(|b| hidden_test_declaration(&b).is_some()) {
                    GivenKind::Macro(loc.start_offset(), loc.end_offset(), scopes.to_vec())
                } else {
                    GivenKind::Plain(def.body().and_then(|b| first_call(&b)))
                };
                out.given.push(Given { name: constant_id_str(&def.name()).to_string(), kind });
            }
            continue;
        }
        if names_test_generator(item, b"include") || names_test_generator(item, b"extend") {
            out.generator = true;
            continue;
        }
        if !defines_nothing(item) && out.more.is_none() {
            out.more = Some(first_line(item));
        }
    }
}

/// Sorbet's modules a body extends to write signatures: they give it no
/// method a class body calls.
const SORBET: &[&str] = &["T::Sig", "T::Helpers", "T::Generic", "T::Sig::WithoutRuntime"];

/// A statement of a module (or `class << self`) body that defines no
/// method there: a constant, a nested module or class, a `def self.`,
/// `sig`, `extend T::Sig`, `private`.
fn defines_nothing(item: &Node<'_>) -> bool {
    if item.as_constant_write_node().is_some()
        || item.as_class_node().is_some()
        || item.as_module_node().is_some()
        || item.as_def_node().is_some_and(|d| d.receiver().is_some())
    {
        return true;
    }
    let Some(call) = item.as_call_node().filter(|c| c.receiver().is_none()) else { return false };
    let args: Vec<Node<'_>> = call.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default();
    match call.name().as_slice() {
        b"sig" => args.is_empty(),
        b"private" | b"public" | b"protected" | b"module_function" => {
            call.block().is_none() && args.iter().all(|a| a.as_symbol_node().is_some())
        }
        b"abstract!" | b"interface!" | b"final!" | b"sealed!" => args.is_empty(),
        b"extend" => {
            !args.is_empty()
                && args.iter().all(|a| consts::segments(a).is_some_and(|(segs, _)| SORBET.contains(&segs.join("::").as_str())))
        }
        _ => false,
    }
}

/// The first place `node` (statements run as a class body loads) may
/// change the class's singleton class other than by the `def self.`, `class
/// << self`, `include` and `extend` statements of the class body itself:
/// `extend`, `include` (of a concern), `define_singleton_method`,
/// `singleton_class`, `class_eval`, `instance_eval`, a `send` of one of
/// those or of a name computed when it runs, a nested `def self.` or
/// `class << self`. Blocks that run in a test (`test`, `setup`, …) and
/// method bodies are not looked in.
fn singleton_op(node: &Node<'_>) -> Option<String> {
    use ruby_prism::Visit;
    const OPS: &[&[u8]] = &[
        b"extend", b"include", b"prepend", b"define_singleton_method", b"singleton_class", b"instance_eval",
        b"instance_exec", b"class_eval", b"class_exec", b"module_eval", b"module_exec", b"singleton_method_added",
    ];
    const LATER: &[&[u8]] = &[b"test", b"setup", b"teardown", b"it", b"specify", b"before", b"after", b"around"];
    struct Scan {
        found: Option<String>,
    }
    impl<'pr> Visit<'pr> for Scan {
        fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
            if self.found.is_some() {
                return;
            }
            let name = node.name();
            let name = name.as_slice();
            if OPS.contains(&name) {
                self.found = Some(first_line(&node.as_node()));
                return;
            }
            if matches!(name, b"send" | b"public_send" | b"__send__") {
                let first = node.arguments().and_then(|a| a.arguments().iter().next());
                let literal = first.as_ref().and_then(|f| {
                    f.as_symbol_node().map(|s| s.unescaped().to_vec()).or_else(|| f.as_string_node().map(|s| s.unescaped().to_vec()))
                });
                if literal.is_none_or(|n| OPS.contains(&n.as_slice())) {
                    self.found = Some(first_line(&node.as_node()));
                    return;
                }
            }
            if node.receiver().is_none() && LATER.contains(&name) && node.block().is_some() {
                if let Some(args) = node.arguments() {
                    self.visit(&args.as_node());
                }
                return;
            }
            ruby_prism::visit_call_node(self, node);
        }
        fn visit_def_node(&mut self, node: &ruby_prism::DefNode<'pr>) {
            if self.found.is_none() && node.receiver().is_some() {
                self.found = Some(first_line(&node.as_node()));
            }
        }
        fn visit_singleton_class_node(&mut self, node: &ruby_prism::SingletonClassNode<'pr>) {
            if self.found.is_none() {
                self.found = Some(first_line(&node.as_node()));
            }
        }
        fn visit_class_node(&mut self, _: &ruby_prism::ClassNode<'pr>) {}
        fn visit_module_node(&mut self, _: &ruby_prism::ModuleNode<'pr>) {}
    }
    let mut scan = Scan { found: None };
    scan.visit(node);
    scan.found
}

/// A file that defines a method named `name` whose body declares tests
/// (a class method, a concern's, a module's): any registered source or
/// test source on disk, by its path relative to the app.
fn macro_defined_in(name: &str) -> Option<String> {
    if matches!(name, "test" | "generate_test") {
        return None;
    }
    let (first, count) = registry();
    let root = consts::late_root();
    MACRO_NAMES.with(|m| {
        let mut m = m.borrow_mut();
        if m.0 != first || m.1 > count {
            *m = (first.clone(), 0, None, HashMap::new());
        }
        for id in m.1 + 1..=count {
            let Some(path) = sources::path_of(FileId(id)) else { continue };
            if let Some(found) = sources::with_text(&path, macro_defs) {
                for name in found {
                    m.3.entry(name).or_insert_with(|| path.clone());
                }
            }
        }
        m.1 = count;
        if m.2.as_ref() != Some(&root) {
            let (files, ids) = consts::late_sources(None);
            for i in ids {
                let (path, text) = &files[i];
                for name in macro_defs(text) {
                    m.3.entry(name).or_insert_with(|| path.clone());
                }
            }
            m.2 = Some(root);
        }
        m.3.get(name).map(|p| sources::relative_path(p))
    })
}

/// The names of the methods `text` defines whose body declares tests,
/// other than the instance methods of a class.
fn macro_defs(text: &str) -> Vec<String> {
    use ruby_prism::Visit;
    if !text.contains("test")
        || !(text.contains("def self.")
            || text.contains("class << self")
            || text.contains("module ")
            || text.contains("class_methods")
            || text.contains("singleton_class"))
    {
        return Vec::new();
    }
    struct Scan {
        /// Innermost scope is a class body (not a module, `class << self`
        /// or block).
        in_class: Vec<bool>,
        out: Vec<String>,
    }
    impl<'pr> Visit<'pr> for Scan {
        fn visit_class_node(&mut self, node: &ruby_prism::ClassNode<'pr>) {
            self.in_class.push(true);
            ruby_prism::visit_class_node(self, node);
            self.in_class.pop();
        }
        fn visit_module_node(&mut self, node: &ruby_prism::ModuleNode<'pr>) {
            self.in_class.push(false);
            ruby_prism::visit_module_node(self, node);
            self.in_class.pop();
        }
        fn visit_singleton_class_node(&mut self, node: &ruby_prism::SingletonClassNode<'pr>) {
            self.in_class.push(false);
            ruby_prism::visit_singleton_class_node(self, node);
            self.in_class.pop();
        }
        fn visit_block_node(&mut self, node: &ruby_prism::BlockNode<'pr>) {
            self.in_class.push(false);
            ruby_prism::visit_block_node(self, node);
            self.in_class.pop();
        }
        fn visit_def_node(&mut self, node: &ruby_prism::DefNode<'pr>) {
            let instance = node.receiver().is_none() && self.in_class.last() == Some(&true);
            if !instance && node.body().is_some_and(|b| hidden_test_declaration(&b).is_some()) {
                self.out.push(constant_id_str(&node.name()).to_string());
            }
        }
    }
    let result = ruby_prism::parse(text.as_bytes());
    let mut scan = Scan { in_class: Vec::new(), out: Vec::new() };
    scan.visit(&result.node());
    scan.out
}

/// `<verb> TestGenerator` (`include`/`extend`).
fn names_test_generator(node: &Node<'_>, verb: &[u8]) -> bool {
    node.as_call_node().is_some_and(|c| {
        c.receiver().is_none()
            && c.name().as_slice() == verb
            && c.arguments().is_some_and(|a| {
                a.arguments().iter().any(|arg| consts::segments(&arg).is_some_and(|(segs, _)| segs.join("::") == "TestGenerator"))
            })
    })
}

/// Where a macro is defined.
enum Origin {
    /// In the test class body (`Macros::own`, by index).
    Own(usize),
    /// A class method of the test class body that declares no test
    /// itself, defined after (or instead of) a macro of its name: the one
    /// the call runs (`Macros::own`, by index).
    Shadow(usize),
    /// A method a module gives the class that declares nothing and
    /// calls nothing, where a macro of its name is defined elsewhere.
    Inert,
    /// A macro ingest does not model, and why.
    Unmodeled(String),
    /// In a module the class includes (a concern's `ClassMethods`) or
    /// extends: (file, def start, def end, its lexical scopes,
    /// TestGenerator is the module's).
    Concern(String, usize, usize, Vec<String>, bool),
}

/// A class method the test class body defines (`class << self`, `def
/// self.m`).
struct Own<'pr> {
    name: String,
    def: ruby_prism::DefNode<'pr>,
    kind: OwnKind,
    /// Defined in `class << self`: its blocks look constants up from the
    /// singleton class, not from the class.
    singleton: bool,
}

enum OwnKind {
    /// Its body declares tests.
    Macro,
    /// It declares none itself.
    Plain,
    /// A macro in a `class << self` that does more than define methods
    /// (the first other statement).
    Unmodeled(String),
}

/// What an `include` or `extend` of the class body gives its singleton
/// class.
enum Gives {
    /// Methods of a module (`from`, in `path`): a concern's
    /// `ClassMethods`, or an extended module's own.
    Methods { from: String, path: String, methods: Methods },
    /// Methods not known (why).
    Unknown(String),
}

/// The test macros a test class body can call.
pub(super) struct Macros<'pr> {
    /// Every class method the body defines, in order.
    own: Vec<Own<'pr>>,
    /// Statements of the body that may define class methods ingest does
    /// not list, as (start, the statement): `define_singleton_method`, a
    /// concern's `included` hook, an `attr_accessor` in `class << self`.
    opaque: Vec<(usize, String)>,
    /// What the modules the body includes and extends give its singleton
    /// class, as (statement start, gift), the one Ruby looks in last
    /// first.
    gifts: Vec<(usize, Gives)>,
    /// The class body extends TestGenerator.
    generator: bool,
    file: String,
    class: ClassId,
    class_start: usize,
    /// Starts of the call statements this class body makes of its own
    /// macros.
    calls: Vec<(String, usize)>,
    /// Where the class body's statements look constants up (the class
    /// body's own constants included).
    cx: Option<consts::ConstCx>,
}

/// The receiverless (or `self.`) `include`/`extend`/`prepend` statement
/// `stmt`: (its verb, its arguments).
fn mixin_call<'pr>(stmt: &Node<'pr>) -> Option<(&'static str, Vec<Node<'pr>>)> {
    let call = stmt.as_call_node()?;
    if !call.receiver().is_none_or(|r| r.as_self_node().is_some()) || call.block().is_some() {
        return None;
    }
    let verb = match call.name().as_slice() {
        b"include" => "include",
        b"extend" => "extend",
        b"prepend" => "prepend",
        _ => return None,
    };
    Some((verb, call.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default()))
}

/// What `include`ing (or `prepend`ing) the module written `segs` where
/// the lexical scopes are `cref` gives the includer's singleton class,
/// the one Ruby looks in last first, and an `included` hook that
/// changes it there.
fn include_gives(segs: &[String], cref: Option<&[String]>, depth: usize) -> (Vec<Gives>, Option<String>) {
    let written = segs.join("::");
    let full = match resolve_module(segs, cref) {
        Ok(Some(full)) => full,
        Ok(None) => return (Vec::new(), None),
        Err(why) => return (vec![Gives::Unknown(why)], None),
    };
    let found: Vec<(String, Methods, Vec<Vec<String>>, Vec<String>, Option<String>)> = CONCERNS.with(|c| {
        c.borrow()
            .iter()
            .filter(|concern| concern.module == full)
            .filter(|concern| sources::with_text(&concern.path, fingerprint) == Some(concern.fingerprint))
            .map(|c| (c.path.clone(), c.methods.clone(), c.dependencies.clone(), c.cref.clone(), c.hook.clone()))
            .collect()
    });
    match found.as_slice() {
        [(path, methods, dependencies, inner, hook)] => {
            let mut out = Vec::new();
            let mut hooks = None;
            for dep in dependencies {
                if depth >= 8 {
                    out.push(Gives::Unknown(format!("`{full}` includes concerns more than 8 deep")));
                    break;
                }
                let (gives, hook) = include_gives(dep, Some(inner), depth + 1);
                out.extend(gives);
                hooks = hooks.or(hook);
            }
            out.push(Gives::Methods { from: full.clone(), path: path.clone(), methods: methods.clone() });
            let hook = hook.as_ref().map(|h| format!("the concern `{full}` changes its includer's singleton class (`{h}`)"));
            (out, hook.or(hooks))
        }
        [_, _, ..] => (vec![Gives::Unknown(format!("`{written}` is declared as a concern in more than one source"))], None),
        // A plain module gives the singleton class nothing, unless a hook
        // of its own does.
        [] => match consts::module_declarations(&full) {
            Err(why) => (vec![Gives::Unknown(why)], None),
            Ok(decls) => match module_hook(&decls) {
                Some(hook) => (vec![Gives::Unknown(format!("`{full}` has a hook (`{hook}`)"))], None),
                None => (Vec::new(), None),
            },
        },
    }
}

/// What `extend`ing the module written `segs` where the lexical scopes
/// are `cref` gives the singleton class.
fn extend_gives(segs: &[String], cref: Option<&[String]>) -> Option<Gives> {
    let full = match resolve_module(segs, cref) {
        Ok(Some(full)) => full,
        Ok(None) => return None,
        Err(why) => return Some(Gives::Unknown(why)),
    };
    let decls = match consts::module_declarations(&full) {
        Ok(decls) => decls,
        Err(why) => return Some(Gives::Unknown(why)),
    };
    let mut methods = Methods::default();
    let mut path = String::new();
    for (file, start, end, scopes) in &decls {
        let read = sources::with_text(file, |text| {
            let result = ruby_prism::parse(text.as_bytes());
            let root = result.node();
            let module = node_at(&root, *start, *end)?.as_module_node()?;
            let items = module.body().map(flatten_statements).unwrap_or_default();
            if let Some(hook) = items.iter().find(|i| i.as_def_node().is_some_and(|d| d.receiver().is_some() && d.name().as_slice() == b"extended")) {
                methods.more.get_or_insert_with(|| first_line(hook));
            }
            methods_of(&items, scopes, &mut methods);
            Some(())
        });
        if read.flatten().is_none() {
            return Some(Gives::Unknown(format!("the declaration of `{full}` was not found")));
        }
        path = file.clone();
    }
    if decls.len() > 1 && methods.given.iter().any(|g| matches!(g.kind, GivenKind::Macro(..))) {
        // Macros of a module declared in more than one file: their file is
        // the one each is in, not modeled.
        return Some(Gives::Unknown(format!("`{full}` is declared in more than one source")));
    }
    Some(Gives::Methods { from: full, path, methods })
}

/// The full name of the module `segs` names where `cref` is: `None` for
/// one that gives a class body nothing (TestGenerator, Sorbet's, a
/// framework module); `Err` for one not known.
fn resolve_module(segs: &[String], cref: Option<&[String]>) -> Result<Option<String>, String> {
    let written = segs.join("::");
    if written == "TestGenerator" || SORBET.contains(&written.as_str()) || consts::known_module(&written) {
        return Ok(None);
    }
    let Some(cref) = cref else { return Err(format!("`{written}`: the lexical scopes of the class are not certain")) };
    match consts::namespace(cref, segs) {
        Some(full) if consts::known_module(&full) => Ok(None),
        Some(full) => Ok(Some(full)),
        None => match consts::late_declared(&written) {
            Some(path) => Err(format!("`{written}` is declared in {path}, a test source not registered before this one")),
            None => Err(format!("`{written}` is not a module of a registered source")),
        },
    }
}

/// A hook of the plain module declared at `decls` that runs when a
/// class includes it (`def self.included`, `included do`).
fn module_hook(decls: &[(String, usize, usize, Vec<String>)]) -> Option<String> {
    decls.iter().find_map(|(file, start, end, _)| {
        sources::with_text(file, |text| {
            let result = ruby_prism::parse(text.as_bytes());
            let root = result.node();
            let module = node_at(&root, *start, *end)?.as_module_node()?;
            module.body().map(flatten_statements).unwrap_or_default().iter().find_map(|item| {
                let hook = item.as_def_node().is_some_and(|d| {
                    d.receiver().is_some() && matches!(d.name().as_slice(), b"included" | b"append_features" | b"prepended" | b"prepend_features")
                }) || item.as_call_node().is_some_and(|c| c.receiver().is_none() && matches!(c.name().as_slice(), b"included" | b"prepended"));
                hook.then(|| first_line(item))
            })
        })
        .flatten()
    })
}

impl<'pr> Macros<'pr> {
    pub(super) fn of_class(class_node: &ruby_prism::ClassNode<'pr>, body: &[Node<'pr>], class: &ClassId, file: &str) -> Self {
        let mut own = Vec::new();
        let mut opaque: Vec<(usize, String)> = Vec::new();
        let mut gifts: Vec<(usize, Gives)> = Vec::new();
        let mut generator = false;
        let declares = |def: &ruby_prism::DefNode<'_>| def.body().is_some_and(|b| hidden_test_declaration(&b).is_some());
        let cref = consts::cref_of(file, class_node.location().start_offset());
        let mut synced = false;
        let mut sync = || {
            if !synced {
                sync_concerns();
                synced = true;
            }
        };
        for stmt in body {
            let at = stmt.location().start_offset();
            if let Some(singleton) = stmt.as_singleton_class_node() {
                if singleton.expression().as_self_node().is_none() {
                    continue;
                }
                let items = singleton.body().map(flatten_statements).unwrap_or_default();
                // `attr_accessor`, `private`, a constant: not modeled
                // beside macros (`covers_singleton`).
                let other = items.iter().find(|i| i.as_def_node().is_none_or(|d| d.receiver().is_some())).map(first_line);
                for item in &items {
                    if let Some(def) = item.as_def_node().filter(|d| d.receiver().is_none()) {
                        let kind = match (&other, declares(&def)) {
                            (_, false) => OwnKind::Plain,
                            (None, true) => OwnKind::Macro,
                            (Some(other), true) => OwnKind::Unmodeled(format!(
                                "the `class << self` that defines it does more than define methods (`{other}`)"
                            )),
                        };
                        own.push(Own { name: constant_id_str(&def.name()).to_string(), def, kind, singleton: true });
                    } else if let Some(("include", args)) = mixin_call(item) {
                        // `include` in the singleton class: `extend`.
                        sync();
                        for arg in args.iter().rev() {
                            match consts::segments(arg) {
                                Some((segs, false)) => gifts.extend(extend_gives(&segs, cref.as_deref()).map(|g| (item.location().start_offset(), g))),
                                _ => opaque.push((item.location().start_offset(), first_line(item))),
                            }
                        }
                    } else if !defines_nothing(item) {
                        opaque.push((item.location().start_offset(), first_line(item)));
                    }
                }
            } else if let Some(def) = stmt.as_def_node() {
                if def.receiver().is_some_and(|r| r.as_self_node().is_some()) {
                    let kind = if declares(&def) { OwnKind::Macro } else { OwnKind::Plain };
                    own.push(Own { name: constant_id_str(&def.name()).to_string(), def, kind, singleton: false });
                } else if def.receiver().is_some() {
                    opaque.push((at, first_line(stmt)));
                }
            } else if let Some((verb, args)) = mixin_call(stmt) {
                sync();
                // `include A, B` includes B, then A: A is looked in first.
                for arg in args.iter().rev() {
                    let Some((segs, false)) = consts::segments(arg) else {
                        opaque.push((at, first_line(stmt)));
                        continue;
                    };
                    if verb == "extend" {
                        generator |= segs == ["TestGenerator"];
                        gifts.extend(extend_gives(&segs, cref.as_deref()).map(|g| (at, g)));
                    } else {
                        let (gives, hook) = include_gives(&segs, cref.as_deref(), 0);
                        gifts.extend(gives.into_iter().map(|g| (at, g)));
                        if let Some(hook) = hook {
                            opaque.push((at, hook));
                        }
                    }
                }
            } else if stmt.as_class_node().is_none() && stmt.as_module_node().is_none() {
                if let Some(op) = singleton_op(stmt) {
                    opaque.push((at, op));
                }
            }
        }
        let calls = body
            .iter()
            .filter_map(|stmt| stmt.as_call_node())
            .filter(|c| c.receiver().is_none() && c.block().is_none())
            .map(|c| (constant_id_str(&c.name()).to_string(), c.location().start_offset()))
            .collect();
        Macros {
            own,
            opaque,
            gifts,
            generator,
            file: file.to_string(),
            class: class.clone(),
            class_start: class_node.location().start_offset(),
            calls,
            cx: consts::ConstCx::of_class(class_node, body, file, 0),
        }
    }

    /// Is a method named `name` one that declares tests: one of this
    /// class's own, one a module gives it, or one any source defines?
    fn matters(&self, name: &str) -> bool {
        self.own.iter().any(|o| o.name == name && !matches!(o.kind, OwnKind::Plain))
            || self.gifts.iter().any(|(_, g)| match g {
                Gives::Methods { methods, .. } => methods.given.iter().any(|m| m.name == name && matches!(m.kind, GivenKind::Macro(..))),
                Gives::Unknown(_) => false,
            })
            || macro_defined_in(name).is_some()
    }

    /// The macro a call of `name` at `call_at` of the class body runs, as
    /// Ruby looks it up in the singleton class: the class's own singleton
    /// method defined last before it (one that declares no test itself
    /// when it replaces a macro), else that of the module included or
    /// extended last before it. `None` when the call runs no method that
    /// declares tests anywhere.
    fn lookup(&self, name: &str, call_at: usize) -> Option<Origin> {
        let own_at = self.own.iter().rposition(|o| o.name == name && o.def.location().start_offset() < call_at);
        let opaque = self.opaque.iter().filter(|(at, _)| *at < call_at).last();
        if let Some((at, what)) = opaque {
            let after_own = own_at.is_none_or(|i| self.own[i].def.location().start_offset() < *at);
            if after_own && self.matters(name) {
                let line = sources::line_at(&self.file, *at).map(|l| l.to_string()).unwrap_or_else(|| "?".into());
                return Some(Origin::Unmodeled(format!("the class body may define it another way (`{what}` at line {line})")));
            }
        }
        if let Some(at) = own_at {
            return match &self.own[at].kind {
                OwnKind::Macro => Some(Origin::Own(at)),
                OwnKind::Unmodeled(why) => Some(Origin::Unmodeled(why.clone())),
                OwnKind::Plain => self.matters(name).then_some(Origin::Shadow(at)),
            };
        }
        let mut unknown: Option<String> = None;
        for (_, gift) in self.gifts.iter().rev().filter(|(at, _)| *at < call_at) {
            match gift {
                Gives::Unknown(why) => {
                    unknown.get_or_insert_with(|| why.clone());
                }
                Gives::Methods { from, path, methods } => {
                    let Some(found) = methods.given.iter().rev().find(|m| m.name == name) else {
                        if let Some(more) = &methods.more {
                            unknown.get_or_insert_with(|| format!("`{from}` may define it (`{more}`)"));
                        }
                        continue;
                    };
                    if let Some(why) = unknown {
                        return self.matters(name).then(|| Origin::Unmodeled(format!("{why}, before `{from}` that defines it")));
                    }
                    return match &found.kind {
                        GivenKind::Macro(start, end, cref) => {
                            Some(Origin::Concern(path.clone(), *start, *end, cref.clone(), methods.generator))
                        }
                        GivenKind::Plain(None) => self.matters(name).then_some(Origin::Inert),
                        GivenKind::Plain(Some(called)) => self.matters(name).then(|| {
                            Origin::Unmodeled(format!("the method it runs, `{from}`'s, declares no test itself but calls `{called}`"))
                        }),
                    };
                }
            }
        }
        match unknown {
            Some(why) if self.matters(name) => Some(Origin::Unmodeled(format!("the module that defines it is not known ({why})"))),
            _ => None,
        }
    }

    /// The first use of the class method `name` (defined in the class
    /// body) other than a call this class body makes as a statement or
    /// one of its own definitions: `send(:gen, …)`, `self.gen`, `method
    /// (:gen)`, a call from inside a method or block, a call before it is
    /// defined, a use in another source. `None` when every use is one
    /// `expand` expands or names.
    fn other_use(&self, name: &str) -> Option<String> {
        let defs: Vec<usize> = self.own.iter().filter(|o| o.name == name).map(|o| o.def.name_loc().start_offset()).collect();
        let own = sources::with_text(&self.file, |text| {
            let result = ruby_prism::parse(text.as_bytes());
            uses(&result.node(), name, true).into_iter().find_map(|used| match used {
                Use::Def(at) if defs.contains(&at) => None,
                // A call before any definition runs no macro (Ruby raises
                // NoMethodError as the class loads): not one `expand` expands.
                Use::Call(at) if self.calls.iter().any(|(n, c)| n == name && *c == at) && self.lookup(name, at).is_some() => None,
                Use::Def(at) | Use::Call(at) | Use::Named(at) => Some(use_text(text, at)),
            })
        });
        match own {
            None => Some("its file is not registered".into()),
            Some(Some(used)) => Some(used),
            Some(None) => foreign_use(name, &self.file),
        }
    }

    /// The `class << self` statement `stmt`, when it is nothing but
    /// method definitions, at least one a macro whose every use is a call
    /// this class body makes as a statement (each expanded or named by
    /// `expand`): its other methods, for the caller to define as the
    /// class's. `None` for any other statement.
    pub(super) fn covers_singleton(&self, stmt: &Node<'pr>) -> Option<Vec<ruby_prism::DefNode<'pr>>> {
        let singleton = stmt.as_singleton_class_node()?;
        singleton.expression().as_self_node()?;
        let start = stmt.location().start_offset();
        let end = stmt.location().end_offset();
        let inside = |o: &&Own<'pr>| (start..end).contains(&o.def.location().start_offset());
        let mine: Vec<&Own<'pr>> = self.own.iter().filter(inside).collect();
        if !mine.iter().any(|o| matches!(o.kind, OwnKind::Macro)) || mine.iter().any(|o| matches!(o.kind, OwnKind::Unmodeled(_))) {
            return None;
        }
        let items = singleton.body().map(flatten_statements).unwrap_or_default();
        if items.len() != mine.len() {
            return None;
        }
        if mine.iter().filter(|o| matches!(o.kind, OwnKind::Macro)).any(|o| self.other_use(&o.name).is_some()) {
            return None;
        }
        let plain: Vec<usize> =
            mine.iter().filter(|o| matches!(o.kind, OwnKind::Plain)).map(|o| o.def.location().start_offset()).collect();
        Some(items.iter().filter_map(|i| i.as_def_node()).filter(|d| plain.contains(&d.location().start_offset())).collect())
    }

    /// Why the tests of the `def self.m` macro `def` are not all
    /// modeled: a use other than a class-body call statement (each of
    /// those is expanded or named by `expand`). `None` for any other def.
    pub(super) fn def_other_use(&self, def: &ruby_prism::DefNode<'_>) -> Option<String> {
        let at = def.location().start_offset();
        let own = self.own.iter().find(|o| !o.singleton && o.def.location().start_offset() == at)?;
        if !matches!(own.kind, OwnKind::Macro) {
            return None;
        }
        self.other_use(&own.name)
    }

    /// The tests the class-body statement `stmt` declares when it calls a
    /// macro: `None` when it is no such call. A call that is not expanded
    /// is named (and `expanded` says so to `TestMixins`), as is a call of
    /// a method that declares tests in some source the class's own
    /// lookup does not reach (one it inherits, a module's it does not
    /// include) and one made other than as `gen(…)` (`self.gen`, `send
    /// (:gen)`, with a block) of a macro not the class's own (whose other
    /// uses `other_use` names).
    pub(super) fn expand(
        &self,
        stmt: &Node<'pr>,
        tags: &TestTags,
        tag_consts: &TagConstants<'pr>,
    ) -> IngestResult<Option<Vec<Test>>> {
        let Some(call) = stmt.as_call_node().filter(|c| c.receiver().is_none() && c.block().is_none()) else {
            return self.other_call(stmt);
        };
        let name = constant_id_str(&call.name()).to_string();
        let Some(origin) = self.lookup(&name, call.location().start_offset()) else {
            if self.own.iter().any(|o| o.name == name) {
                return Ok(None);
            }
            return match macro_defined_in(&name) {
                Some(path) => self.name_gap(&name, stmt, format!(
                    "the method it runs is not one the class body, or a module it includes or extends, defines (one it inherits, or another), and a method of that name declares tests in {path}"
                )),
                None => Ok(None),
            };
        };
        let class_cref = consts::cref_of(&self.file, self.class_start);
        let outcome = match &origin {
            Origin::Own(at) => {
                let own = &self.own[*at];
                if !self.generator && uses_generate_test(&own.def) {
                    Err("`generate_test` is not TestGenerator's (the class does not extend it)".to_string())
                } else {
                    self.expand_def(&own.def, &self.file, &call, tags, tag_consts, None, own.singleton)?
                }
            }
            // A method that declares nothing and calls nothing declares
            // no test; one that calls something (`super`, another macro)
            // may.
            Origin::Shadow(at) => {
                let def = &self.own[*at].def;
                match def.body().and_then(|b| first_call(&b)) {
                    None => Ok(Vec::new()),
                    Some(called) => Err(format!(
                        "the definition it runs (line {}) declares no test itself but calls `{called}`",
                        sources::line_at(&self.file, def.location().start_offset()).map(|l| l.to_string()).unwrap_or_else(|| "?".into())
                    )),
                }
            }
            Origin::Inert => Ok(Vec::new()),
            Origin::Unmodeled(why) => Err(why.clone()),
            Origin::Concern(path, start, end, macro_cref, generator) => {
                let generator = *generator || self.generator;
                let path = path.clone();
                // A copy: ingesting the bodies writes to the registry
                // `with_text` would hold.
                match sources::text_of(&path) {
                    None => Err("its file is not registered".into()),
                    Some(text) => {
                        let parsed = super::super::prism::parse_silent(text.as_bytes());
                        let root = parsed.node();
                        match def_at(&root, *start, *end) {
                            None => Err("its definition was not found".into()),
                            Some(def) if !generator && uses_generate_test(&def) => {
                                Err("`generate_test` is not TestGenerator's".into())
                            }
                            Some(def) => {
                                let scopes = (macro_cref.clone(), class_cref.clone().unwrap_or_default());
                                self.expand_def(&def, &path, &call, tags, tag_consts, Some(scopes), false)?
                            }
                        }
                    }
                }
            }
        };
        match outcome {
            Ok(tests) => {
                let print = sources::with_text(&self.file, fingerprint).unwrap_or_default();
                EXPANDED.with(|e| e.borrow_mut().push((self.file.clone(), print, call.location().start_offset())));
                Ok(Some(tests))
            }
            Err(why) => self.name_gap(&name, stmt, why),
        }
    }

    /// Name the tests the call of `name` that is the statement `stmt`
    /// declares as not modeled (`why`), and mark it handled.
    fn name_gap(&self, name: &str, stmt: &Node<'pr>, why: String) -> IngestResult<Option<Vec<Test>>> {
        let at = stmt.location().start_offset();
        let line = sources::line_at(&self.file, at).map(|l| l.to_string()).unwrap_or_else(|| "?".into());
        super::super::survey::continue_or_fail(IngestError::Unsupported {
            file: self.file.clone(),
            message: format!(
                "tests declared by a call of the class method `{name}` are not modeled; they will not run (`{}` line {line}: {why})",
                self.class.0
            ),
        })?;
        let print = sources::with_text(&self.file, fingerprint).unwrap_or_default();
        EXPANDED.with(|e| e.borrow_mut().push((self.file.clone(), print, at)));
        Ok(Some(Vec::new()))
    }

    /// The class-body statement `stmt` calling a macro other than as
    /// `gen(…)`: `self.gen(…)`, `send(:gen, …)`, `gen(…) { … }`, of one
    /// not the class's own. Named; `None` for any other statement.
    fn other_call(&self, stmt: &Node<'pr>) -> IngestResult<Option<Vec<Test>>> {
        let Some(call) = stmt.as_call_node() else { return Ok(None) };
        let own_receiver = call.receiver().is_none_or(|r| r.as_self_node().is_some());
        if !own_receiver {
            return Ok(None);
        }
        let name = if matches!(call.name().as_slice(), b"send" | b"public_send" | b"__send__") {
            let first = call.arguments().and_then(|a| a.arguments().iter().next());
            match first.and_then(|f| f.as_symbol_node().map(|s| s.unescaped().to_vec())) {
                Some(name) => String::from_utf8_lossy(&name).into_owned(),
                None => return Ok(None),
            }
        } else if call.receiver().is_some() || call.block().is_some() {
            constant_id_str(&call.name()).to_string()
        } else {
            return Ok(None);
        };
        if matches!(name.as_str(), "test" | "setup" | "teardown" | "included" | "it" | "describe" | "context")
            || self.own.iter().any(|o| o.name == name)
            || !self.matters(&name)
        {
            return Ok(None);
        }
        self.name_gap(&name, stmt, format!("it is called other than as `{name}(…)` (`{}`)", first_line(stmt)))
    }

    /// The tests `def` (written in `def_file`) declares when `call` runs
    /// it, or why they are not modeled. `scopes` (the macro's lexical
    /// scopes, the class's) when it is written outside the class;
    /// `singleton` when it is the class's, in `class << self`.
    #[allow(clippy::too_many_arguments)]
    fn expand_def<'d>(
        &self,
        def: &ruby_prism::DefNode<'d>,
        def_file: &str,
        call: &ruby_prism::CallNode<'pr>,
        tags: &TestTags,
        tag_consts: &TagConstants<'d>,
        scopes: Option<(Vec<String>, Vec<String>)>,
        singleton: bool,
    ) -> IngestResult<Result<Vec<Test>, String>>
    where
        'pr: 'd,
    {
        let body = def.body().map(flatten_statements).unwrap_or_default();
        // Locals the body assigns: each only ever a `location` argument.
        let mut locals: Vec<String> = Vec::new();
        let mut declarations: Vec<ruby_prism::CallNode<'d>> = Vec::new();
        for stmt in &body {
            if let Some(write) = stmt.as_local_variable_write_node() {
                if !String::from_utf8_lossy(write.value().location().as_slice()).contains("caller_locations") {
                    return Ok(Err(format!("its body assigns `{}` from more than `caller_locations`", constant_id_str(&write.name()))));
                }
                locals.push(constant_id_str(&write.name()).to_string());
                continue;
            }
            match stmt.as_call_node() {
                Some(c) if c.receiver().is_none() && matches!(c.name().as_slice(), b"generate_test" | b"test") && c.arguments().is_some() => {
                    declarations.push(c)
                }
                _ => return Ok(Err(format!("its body does more than declare tests (`{}`)", first_line(stmt)))),
            }
        }
        let cx = self.cx.as_ref().map(|cx| Rc::new(cx.before(call.location().start_offset())));
        let bound = match test_unroll::bind_call(def, def_file, call, &self.file, cx)? {
            Ok(bound) => bound,
            Err(why) => return Ok(Err(why)),
        };
        let params = bound.names();
        let mut tests = Vec::new();
        for declaration in &declarations {
            let args: Vec<Node<'d>> = declaration.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default();
            let generate = declaration.name().as_slice() == b"generate_test";
            // generate_test(name, location = nil, tags: nil): the tags
            // are TestTags#test's.
            let tag_args: Vec<Node<'d>> = if generate {
                let mut tag_args = Vec::new();
                for (at, arg) in args.iter().enumerate().skip(1) {
                    if let Some(hash) = arg.as_keyword_hash_node().filter(|_| at == args.len() - 1) {
                        for element in hash.elements().iter() {
                            match element.as_assoc_node() {
                                Some(assoc) if assoc.key().as_symbol_node().is_some_and(|s| s.unescaped() == b"tags") => {
                                    tag_args.push(assoc.value())
                                }
                                _ => return Ok(Err(format!("`generate_test` keyword `{}`", first_line(&element)))),
                            }
                        }
                    } else if at == 1
                        && (arg.as_nil_node().is_some()
                            || arg.as_local_variable_read_node().is_some_and(|r| locals.iter().any(|l| l == constant_id_str(&r.name()))))
                    {
                    } else {
                        return Ok(Err(format!("`generate_test` argument `{}`", first_line(arg))));
                    }
                }
                tag_args
            } else {
                declaration.arguments().map(|a| a.arguments().iter().skip(1).collect()).unwrap_or_default()
            };
            if let Some(why) = block_misuse(declaration, &locals, &params) {
                return Ok(Err(why));
            }
            // A test's own copy of an argument is not the one object the
            // macro's tests share: changing it in place is not modeled.
            if let Some(block) = declaration.block() {
                let unshared = bound.unshared();
                if let Some((name, at)) = test_unroll::changed_in_place(&block, &unshared) {
                    return Ok(Err(format!("a test block changes the argument `{name}` its other tests share (`{at}`)")));
                }
            }
            if singleton {
                let class_cref = consts::cref_of(&self.file, self.class_start).unwrap_or_default();
                if let Some(why) = singleton_constants(declaration, &class_cref) {
                    return Ok(Err(why));
                }
            }
            if let Some((macro_cref, class_cref)) = &scopes {
                if let Some(why) = foreign_constants(declaration, macro_cref, class_cref) {
                    return Ok(Err(why));
                }
            }
            let Some(name) = args.first().and_then(|n| bound.string(n)) else {
                return Ok(Err(format!("a test name is not known before it runs (`{}`)", args.first().map(first_line).unwrap_or_default())));
            };
            match dsl_test_tagged(declaration, &tag_args, name, bound.prelude(), def_file, tags, tag_consts)? {
                Ok(test) => tests.push(test),
                Err((what, detail)) => return Ok(Err(format!("a test whose {what}: {detail}"))),
            }
        }
        Ok(Ok(tests))
    }
}

fn uses_generate_test(def: &ruby_prism::DefNode<'_>) -> bool {
    def.body()
        .map(flatten_statements)
        .unwrap_or_default()
        .iter()
        .any(|s| s.as_call_node().is_some_and(|c| c.name().as_slice() == b"generate_test"))
}

/// The first use of the class method `name` in a source other than
/// `file` (registered, or a test source on disk) that is not a class
/// body's statement calling it (each of those is expanded or named
/// where that class is ingested): `Base.gen(…)`, `:gen`, a call from a
/// method or block.
fn foreign_use(name: &str, file: &str) -> Option<String> {
    let key = registry();
    let cached = FOREIGN.with(|f| {
        let f = f.borrow();
        (f.0 == key).then(|| f.1.get(&(name.to_string(), file.to_string())).cloned()).flatten()
    });
    if let Some(found) = cached {
        return found;
    }
    let check = |path: &str, text: &str| -> Option<String> {
        if !has_word(text, name) {
            return None;
        }
        let result = ruby_prism::parse(text.as_bytes());
        let root = result.node();
        let statements = class_body_calls(&root);
        uses(&root, name, false).into_iter().find_map(|used| match used {
            Use::Def(_) => None,
            Use::Call(at) if statements.contains(&at) => None,
            Use::Call(at) | Use::Named(at) => Some(format!("{} of {}", use_text(text, at), sources::relative_path(path))),
        })
    };
    let mut found = None;
    for id in 1..=key.1 {
        let Some(path) = sources::path_of(FileId(id)) else { continue };
        if path == file {
            continue;
        }
        found = sources::with_text(&path, |text| check(&path, text)).flatten();
        if found.is_some() {
            break;
        }
    }
    if found.is_none() {
        let (files, ids) = consts::late_sources(None);
        found = ids.into_iter().filter(|i| files[*i].0 != file).find_map(|i| check(&files[i].0, &files[i].1));
    }
    FOREIGN.with(|f| {
        let mut f = f.borrow_mut();
        if f.0 != key {
            *f = (key, HashMap::new());
        }
        f.1.insert((name.to_string(), file.to_string()), found.clone());
    });
    found
}

/// Does `text` have `name` as a whole word?
fn has_word(text: &str, name: &str) -> bool {
    let bytes = text.as_bytes();
    let ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80;
    text.match_indices(name).any(|(at, _)| {
        let end = at + name.len();
        !(at > 0 && ident(bytes[at - 1])) && !(end < bytes.len() && ident(bytes[end]))
    })
}

/// The starts of the statements of every class body in `root` that call
/// a method of the class (`gen(…)`, `self.gen(…)`).
fn class_body_calls(root: &Node<'_>) -> Vec<usize> {
    use ruby_prism::Visit;
    struct Scan {
        out: Vec<usize>,
    }
    impl<'pr> Visit<'pr> for Scan {
        fn visit_class_node(&mut self, node: &ruby_prism::ClassNode<'pr>) {
            for stmt in node.body().map(flatten_statements).unwrap_or_default() {
                if stmt.as_call_node().is_some_and(|c| c.receiver().is_none_or(|r| r.as_self_node().is_some())) {
                    self.out.push(stmt.location().start_offset());
                }
            }
            ruby_prism::visit_class_node(self, node);
        }
    }
    let mut scan = Scan { out: Vec::new() };
    scan.visit(root);
    scan.out
}

fn node_at<'pr>(root: &Node<'pr>, start: usize, end: usize) -> Option<Node<'pr>> {
    struct Find<'pr> {
        start: usize,
        end: usize,
        found: Option<Node<'pr>>,
    }
    impl<'pr> ruby_prism::Visit<'pr> for Find<'pr> {
        fn visit_branch_node_enter(&mut self, node: Node<'pr>) {
            let loc = node.location();
            // The innermost node of the span (a statement list of one
            // statement has its span).
            if loc.start_offset() == self.start && loc.end_offset() == self.end {
                self.found = Some(node);
            }
        }
    }
    use ruby_prism::Visit;
    let mut find = Find { start, end, found: None };
    find.visit(root);
    find.found
}

fn def_at<'pr>(root: &Node<'pr>, start: usize, end: usize) -> Option<ruby_prism::DefNode<'pr>> {
    node_at(root, start, end)?.as_def_node()
}

/// Where a file mentions the method `name`.
enum Use {
    /// `def name` (its name's start).
    Def(usize),
    /// A receiverless call (its start).
    Call(usize),
    /// A call with a receiver, the name as a Symbol or String (`send`,
    /// `method`, `alias_method`), or a `send` of a name built at run
    /// time (`send("gen_#{x}")`): a use not modeled (its start).
    Named(usize),
}

/// The line of `text` at `at`, trimmed, with its number.
fn use_text(text: &str, at: usize) -> String {
    let start = text[..at].rfind('\n').map_or(0, |n| n + 1);
    let end = text[at..].find('\n').map_or(text.len(), |n| at + n);
    let line = text[..at].matches('\n').count() + 1;
    let shown: String = text[start..end].trim().chars().take(100).collect();
    format!("`{shown}` at line {line}")
}

/// The first method `node` calls (`super` and `yield` included).
fn first_call(node: &Node<'_>) -> Option<String> {
    use ruby_prism::Visit;
    struct Scan {
        found: Option<String>,
    }
    impl<'pr> Visit<'pr> for Scan {
        fn visit_branch_node_enter(&mut self, node: Node<'pr>) {
            if self.found.is_some() {
                return;
            }
            if let Some(call) = node.as_call_node() {
                self.found = Some(constant_id_str(&call.name()).to_string());
            } else if node.as_super_node().is_some() || node.as_forwarding_super_node().is_some() {
                self.found = Some("super".into());
            } else if node.as_yield_node().is_some() {
                self.found = Some("yield".into());
            }
        }
        fn visit_leaf_node_enter(&mut self, node: Node<'pr>) {
            if self.found.is_none() && node.as_forwarding_super_node().is_some() {
                self.found = Some("super".into());
            }
        }
    }
    let mut scan = Scan { found: None };
    scan.visit(node);
    scan.found
}

/// The uses of the method `name` in `root`; with `dynamic`, a `send`
/// of a name computed when it runs is one.
fn uses(root: &Node<'_>, name: &str, dynamic: bool) -> Vec<Use> {
    use ruby_prism::Visit;
    struct Scan<'a> {
        name: &'a [u8],
        dynamic: bool,
        out: Vec<Use>,
    }
    impl<'pr> Visit<'pr> for Scan<'_> {
        fn visit_branch_node_enter(&mut self, node: Node<'pr>) {
            if let Some(call) = node.as_call_node().filter(|c| c.name().as_slice() == self.name) {
                self.out.push(match call.receiver() {
                    None => Use::Call(call.location().start_offset()),
                    Some(_) => Use::Named(call.location().start_offset()),
                });
            } else if let Some(def) = node.as_def_node().filter(|d| d.name().as_slice() == self.name) {
                self.out.push(Use::Def(def.name_loc().start_offset()));
            }
            if let Some(call) = node.as_call_node() {
                let dynamic = matches!(call.name().as_slice(), b"send" | b"public_send" | b"__send__" | b"method")
                    && call
                        .arguments()
                        .and_then(|a| a.arguments().iter().next())
                        .is_some_and(|first| first.as_symbol_node().is_none() && first.as_string_node().is_none());
                if dynamic && self.dynamic {
                    self.out.push(Use::Named(call.location().start_offset()));
                }
            }
        }
        fn visit_leaf_node_enter(&mut self, node: Node<'pr>) {
            if node.as_symbol_node().is_some_and(|s| s.unescaped() == self.name)
                || node.as_string_node().is_some_and(|s| s.unescaped() == self.name)
            {
                self.out.push(Use::Named(node.location().start_offset()));
            }
        }
    }
    let mut scan = Scan { name: name.as_bytes(), dynamic, out: Vec::new() };
    scan.visit(root);
    scan.out
}

fn first_line(node: &Node<'_>) -> String {
    let text = String::from_utf8_lossy(node.location().as_slice()).into_owned();
    let line = text.lines().next().unwrap_or_default().trim().to_string();
    if line.chars().count() > 100 {
        format!("{}…", line.chars().take(100).collect::<String>())
    } else {
        line
    }
}

/// Does the block of `declaration` read a macro local other than a
/// parameter (`location`), or assign a parameter its other tests share?
fn block_misuse(declaration: &ruby_prism::CallNode<'_>, locals: &[String], params: &[String]) -> Option<String> {
    use ruby_prism::Visit;
    struct Scan<'a> {
        locals: &'a [String],
        params: &'a [String],
        found: Option<String>,
    }
    impl<'pr> Visit<'pr> for Scan<'_> {
        fn visit_local_variable_read_node(&mut self, node: &ruby_prism::LocalVariableReadNode<'pr>) {
            let name = constant_id_str(&node.name());
            if node.depth() > 0 && self.locals.iter().any(|l| l == name) && self.found.is_none() {
                self.found = Some(format!("a test block reads the macro's local `{name}`"));
            }
        }
        fn visit_local_variable_write_node(&mut self, node: &ruby_prism::LocalVariableWriteNode<'pr>) {
            let name = constant_id_str(&node.name());
            if node.depth() > 0 && self.params.iter().chain(self.locals).any(|p| p == name) && self.found.is_none() {
                self.found = Some(format!("a test block assigns the macro's `{name}`"));
            }
            ruby_prism::visit_local_variable_write_node(self, node);
        }
        fn visit_local_variable_operator_write_node(&mut self, node: &ruby_prism::LocalVariableOperatorWriteNode<'pr>) {
            if node.depth() > 0 && self.found.is_none() {
                self.found = Some(format!("a test block assigns the macro's `{}`", constant_id_str(&node.name())));
            }
            ruby_prism::visit_local_variable_operator_write_node(self, node);
        }
        fn visit_local_variable_or_write_node(&mut self, node: &ruby_prism::LocalVariableOrWriteNode<'pr>) {
            if node.depth() > 0 && self.found.is_none() {
                self.found = Some(format!("a test block assigns the macro's `{}`", constant_id_str(&node.name())));
            }
            ruby_prism::visit_local_variable_or_write_node(self, node);
        }
        fn visit_local_variable_and_write_node(&mut self, node: &ruby_prism::LocalVariableAndWriteNode<'pr>) {
            if node.depth() > 0 && self.found.is_none() {
                self.found = Some(format!("a test block assigns the macro's `{}`", constant_id_str(&node.name())));
            }
            ruby_prism::visit_local_variable_and_write_node(self, node);
        }
    }
    let block = declaration.block()?;
    let mut scan = Scan { locals, params, found: None };
    scan.visit(&block);
    scan.found
}

/// The first names of the constants the block of `declaration` names
/// (`A` of `A::B`; not `::A`), each once.
fn block_constants(declaration: &ruby_prism::CallNode<'_>) -> Vec<String> {
    use ruby_prism::Visit;
    struct Scan {
        firsts: Vec<String>,
    }
    impl<'pr> Visit<'pr> for Scan {
        fn visit_constant_read_node(&mut self, node: &ruby_prism::ConstantReadNode<'pr>) {
            self.firsts.push(constant_id_str(&node.name()).to_string());
        }
        fn visit_constant_path_node(&mut self, node: &ruby_prism::ConstantPathNode<'pr>) {
            match node.parent() {
                // `::A`: the same from anywhere.
                None => {}
                Some(parent) => self.visit(&parent),
            }
        }
    }
    let Some(block) = declaration.block() else { return Vec::new() };
    let mut scan = Scan { firsts: Vec::new() };
    scan.visit(&block);
    scan.firsts.sort();
    scan.firsts.dedup();
    scan.firsts
}

/// A constant the block of `declaration` names whose first name resolves
/// differently from the macro's scopes than from the test class's (each
/// by its lexical scopes, the innermost one's ancestors, the top level).
fn foreign_constants(declaration: &ruby_prism::CallNode<'_>, macro_cref: &[String], class_cref: &[String]) -> Option<String> {
    for name in &block_constants(declaration) {
        let from_macro = consts::resolution(macro_cref, name);
        let from_class = consts::resolution(class_cref, name);
        if from_macro == consts::Resolution::Unknown || from_macro != from_class {
            return Some(format!(
                "its block names `{name}`, which the test class may resolve differently ({from_class:?}) from the macro ({from_macro:?})"
            ));
        }
    }
    None
}

/// A constant the block of `declaration` (in a method of the test
/// class's `class << self`) names that may resolve differently there
/// than in the test's own method: past the lexical scopes, Ruby looks in
/// the singleton class's ancestors, not the class's (`consts::
/// same_in_singleton`).
fn singleton_constants(declaration: &ruby_prism::CallNode<'_>, class_cref: &[String]) -> Option<String> {
    block_constants(declaration).into_iter().find_map(|name| {
        consts::same_in_singleton(class_cref, &name)
            .err()
            .map(|why| format!("its block, in `class << self`, names `{name}`, which {why}"))
    })
}

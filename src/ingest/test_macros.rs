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
//! The method is found in the test class body (`class << self`, `def
//! self.m`) or, for a call the class body does not define, in an
//! `ActiveSupport::Concern` the class includes (its `module ClassMethods`
//! or `class_methods do`), from any source registered before the test
//! class is ingested (the app's and lib's, and the test files sorted
//! before this one).
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
//! A call that is not expanded is named: one of the class's own macros
//! by this module, one an included module gives by `TestMixins`, which
//! skips the calls expanded here (`expanded`).

use std::cell::RefCell;
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

/// A concern's test macros, from a registered source.
struct Concern {
    module: String,
    path: String,
    fingerprint: u64,
    /// (name, def start, def end, lexical scopes of its body).
    macros: Vec<(String, usize, usize, Vec<String>)>,
    /// `ClassMethods` includes (or `class_methods` extends with)
    /// TestGenerator, so `generate_test` in a macro is its.
    generator: bool,
}

thread_local! {
    static CONCERNS: RefCell<Vec<Concern>> = const { RefCell::new(Vec::new()) };
    /// The registry `CONCERNS` was read from: its first path, and how
    /// many of its files have been read.
    static SCANNED: RefCell<(Option<String>, u32)> = const { RefCell::new((None, 0)) };
    /// Calls expanded, as (file, its fingerprint, call start).
    static EXPANDED: RefCell<Vec<(String, u64, usize)>> = const { RefCell::new(Vec::new()) };
}

fn fingerprint(text: &str) -> u64 {
    // FNV-1a: tells a stale entry (another app ingested on this thread)
    // from the file registered now.
    text.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3))
}

/// Was the call at `offset` of `file` expanded?
pub(super) fn expanded(file: &str, offset: usize) -> bool {
    let Some(now) = sources::with_text(file, fingerprint) else { return false };
    EXPANDED.with(|e| e.borrow().iter().any(|(f, print, at)| f == file && *print == now && *at == offset))
}

/// Read the concerns of the sources registered since the last call
/// (every registered source, once per ingest).
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
            sources::with_text(&path, |text| {
                if text.contains("ActiveSupport::Concern")
                    && (text.contains("ClassMethods") || text.contains("class_methods"))
                    && text.contains("test")
                {
                    let result = ruby_prism::parse(text.as_bytes());
                    record_concerns(&result.node(), &path);
                }
            });
            scanned.1 = id;
            id += 1;
        }
    });
}

/// Record the test macros the concerns `file` declares (`root`).
fn record_concerns(root: &Node<'_>, file: &str) {
    struct Walk<'a> {
        file: &'a str,
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
                        if let Some(concern) = concern_of(&body, &name, &cref, self.file) {
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
    let mut walk = Walk { file, scopes: Vec::new(), found: Vec::new() };
    if let Some(program) = root.as_program_node() {
        walk.statements(&program.statements().as_node());
    }
    if walk.found.is_empty() {
        return;
    }
    CONCERNS.with(|c| {
        let mut c = c.borrow_mut();
        c.retain(|old| old.path != file);
        c.extend(walk.found);
    });
}

fn flatten_statements_of<'pr>(node: &Node<'pr>) -> Vec<Node<'pr>> {
    match node.as_statements_node() {
        Some(s) => s.body().iter().collect(),
        None => Vec::new(),
    }
}

/// The macros of the concern whose body is `body`: an `extend
/// ActiveSupport::Concern` module with `module ClassMethods` or
/// `class_methods do`.
fn concern_of(body: &[Node<'_>], module: &str, cref: &[String], file: &str) -> Option<Concern> {
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
    let mut macros = Vec::new();
    let mut generator = false;
    for stmt in body {
        // A `module ClassMethods` body's constants are looked up from
        // it; a `class_methods` block's from the concern.
        let (class_methods, scopes): (Vec<Node<'_>>, Vec<String>) = if let Some(m) = stmt.as_module_node() {
            if consts::segments(&m.constant_path()).is_some_and(|(s, rooted)| !rooted && s == ["ClassMethods"]) {
                let mut scopes = cref.to_vec();
                scopes.push(format!("{module}::ClassMethods"));
                (m.body().map(flatten_statements).unwrap_or_default(), scopes)
            } else {
                continue;
            }
        } else if let Some(call) = stmt.as_call_node().filter(|c| c.receiver().is_none() && c.name().as_slice() == b"class_methods") {
            match call.block().and_then(|b| b.as_block_node()).and_then(|b| b.body()) {
                Some(body) => (flatten_statements(body), cref.to_vec()),
                None => continue,
            }
        } else {
            continue;
        };
        for item in &class_methods {
            if let Some(def) = item.as_def_node().filter(|d| d.receiver().is_none()) {
                if def.body().is_some_and(|b| hidden_test_declaration(&b).is_some()) {
                    let loc = def.location();
                    macros.push((constant_id_str(&def.name()).to_string(), loc.start_offset(), loc.end_offset(), scopes.clone()));
                }
            }
            generator |= names_test_generator(item, b"include") || names_test_generator(item, b"extend");
        }
    }
    if macros.is_empty() {
        return None;
    }
    let fingerprint = sources::with_text(file, fingerprint)?;
    Some(Concern { module: module.to_string(), path: file.to_string(), fingerprint, macros, generator })
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
    /// A macro of the test class body ingest does not model, and why.
    Unmodeled(String),
    /// In a concern the class includes: (file, def start, def end, its
    /// lexical scopes, TestGenerator is the concern's).
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

/// The test macros a test class body can call.
pub(super) struct Macros<'pr> {
    /// Every class method the body defines, in order.
    own: Vec<Own<'pr>>,
    /// The concerns the class includes, in order: (file, macros,
    /// TestGenerator is the concern's, start of the `include`).
    #[allow(clippy::type_complexity)]
    concerns: Vec<(String, Vec<(String, usize, usize, Vec<String>)>, bool, usize)>,
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

impl<'pr> Macros<'pr> {
    pub(super) fn of_class(class_node: &ruby_prism::ClassNode<'pr>, body: &[Node<'pr>], class: &ClassId, file: &str) -> Self {
        let mut own = Vec::new();
        let declares = |def: &ruby_prism::DefNode<'_>| def.body().is_some_and(|b| hidden_test_declaration(&b).is_some());
        for stmt in body {
            if let Some(singleton) = stmt.as_singleton_class_node() {
                if singleton.expression().as_self_node().is_none() {
                    continue;
                }
                let items = singleton.body().map(flatten_statements).unwrap_or_default();
                // `attr_accessor`, `private`, a constant: not modeled
                // beside macros (`covers_singleton`).
                let other = items.iter().find(|i| i.as_def_node().is_none_or(|d| d.receiver().is_some())).map(first_line);
                for def in items.iter().filter_map(|i| i.as_def_node()).filter(|d| d.receiver().is_none()) {
                    let kind = match (&other, declares(&def)) {
                        (_, false) => OwnKind::Plain,
                        (None, true) => OwnKind::Macro,
                        (Some(other), true) => OwnKind::Unmodeled(format!(
                            "the `class << self` that defines it does more than define methods (`{other}`)"
                        )),
                    };
                    own.push(Own { name: constant_id_str(&def.name()).to_string(), def, kind, singleton: true });
                }
            } else if let Some(def) = stmt.as_def_node().filter(|d| d.receiver().is_some_and(|r| r.as_self_node().is_some())) {
                let kind = if declares(&def) { OwnKind::Macro } else { OwnKind::Plain };
                own.push(Own { name: constant_id_str(&def.name()).to_string(), def, kind, singleton: false });
            }
        }
        let calls = body
            .iter()
            .filter_map(|stmt| stmt.as_call_node())
            .filter(|c| c.receiver().is_none() && c.block().is_none())
            .map(|c| (constant_id_str(&c.name()).to_string(), c.location().start_offset()))
            .collect();
        let generator = body.iter().any(|s| names_test_generator(s, b"extend"));
        let mut concerns = Vec::new();
        let includes: Vec<(Vec<String>, usize)> = body
            .iter()
            .filter_map(|stmt| stmt.as_call_node())
            .filter(|call| call.receiver().is_none() && call.name().as_slice() == b"include")
            .flat_map(|call| {
                let at = call.location().start_offset();
                call.arguments().map(|a| a.arguments().iter().map(|arg| (arg, at)).collect::<Vec<_>>()).unwrap_or_default()
            })
            .filter_map(|(arg, at)| consts::segments(&arg).map(|(s, _)| (s, at)))
            .collect();
        if !includes.is_empty() {
            sync_concerns();
            if let Some(cref) = consts::cref_of(file, class_node.location().start_offset()) {
                for (include, at) in includes {
                    let Some(module) = consts::namespace(&cref, &include) else { continue };
                    let found = CONCERNS.with(|c| {
                        c.borrow()
                            .iter()
                            .filter(|concern| concern.module == module)
                            .filter(|concern| sources::with_text(&concern.path, fingerprint) == Some(concern.fingerprint))
                            .map(|concern| (concern.path.clone(), concern.macros.clone(), concern.generator))
                            .collect::<Vec<_>>()
                    });
                    // A module two files declare as a concern is not one
                    // to choose from.
                    if let [(path, macros, generator)] = found.as_slice() {
                        concerns.push((path.clone(), macros.clone(), *generator, at));
                    }
                }
            }
        }
        Macros {
            own,
            concerns,
            generator,
            file: file.to_string(),
            class: class.clone(),
            class_start: class_node.location().start_offset(),
            calls,
            cx: consts::ConstCx::of_class(class_node, body, file, 0),
        }
    }

    /// The macro a call of `name` at `call_at` of the class body runs:
    /// the class's own singleton method defined last before it (one that
    /// declares no test itself when it replaces a macro), else that of
    /// the concern included last before it.
    fn lookup(&self, name: &str, call_at: usize) -> Option<Origin> {
        if let Some(at) = self.own.iter().rposition(|o| o.name == name && o.def.location().start_offset() < call_at) {
            return match &self.own[at].kind {
                OwnKind::Macro => Some(Origin::Own(at)),
                OwnKind::Unmodeled(why) => Some(Origin::Unmodeled(why.clone())),
                OwnKind::Plain => {
                    let replaces = self.own.iter().any(|o| o.name == name && !matches!(o.kind, OwnKind::Plain))
                        || self.concerns.iter().any(|c| c.3 < call_at && c.1.iter().any(|(n, ..)| n == name));
                    replaces.then_some(Origin::Shadow(at))
                }
            };
        }
        for (path, macros, generator, _) in self.concerns.iter().rev().filter(|c| c.3 < call_at) {
            if let Some((_, start, end, cref)) = macros.iter().find(|(n, ..)| n == name) {
                return Some(Origin::Concern(path.clone(), *start, *end, cref.clone(), *generator));
            }
        }
        None
    }

    /// The first use of the class method `name` (defined in the class
    /// body) other than a call this class body makes as a statement or
    /// one of its own definitions: `send(:gen, …)`, `self.gen`, `method
    /// (:gen)`, a call from inside a method or block, a call before it is
    /// defined. `None` when every use is one `expand` expands or names.
    fn other_use(&self, name: &str) -> Option<String> {
        let defs: Vec<usize> = self.own.iter().filter(|o| o.name == name).map(|o| o.def.name_loc().start_offset()).collect();
        sources::with_text(&self.file, |text| {
            let result = ruby_prism::parse(text.as_bytes());
            uses(&result.node(), name).into_iter().find_map(|used| match used {
                Use::Def(at) if defs.contains(&at) => None,
                // A call before any definition runs no macro (Ruby raises
                // NoMethodError as the class loads): not one `expand` expands.
                Use::Call(at) if self.calls.iter().any(|(n, c)| n == name && *c == at) && self.lookup(name, at).is_some() => None,
                Use::Def(at) | Use::Call(at) | Use::Named(at) => Some(use_text(text, at)),
            })
        })
        .unwrap_or_else(|| Some("its file is not registered".into()))
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
    /// macro: `None` when it is no such call. A call of the class's own
    /// macro that is not expanded is named; one of a concern's is left
    /// to `TestMixins` to name.
    pub(super) fn expand(
        &self,
        stmt: &Node<'pr>,
        tags: &TestTags,
        tag_consts: &TagConstants<'pr>,
    ) -> IngestResult<Option<Vec<Test>>> {
        let Some(call) = stmt.as_call_node().filter(|c| c.receiver().is_none() && c.block().is_none()) else {
            return Ok(None);
        };
        let name = constant_id_str(&call.name()).to_string();
        let Some(origin) = self.lookup(&name, call.location().start_offset()) else { return Ok(None) };
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
            Err(why) => {
                if !matches!(origin, Origin::Concern(..)) {
                    let line = sources::line_at(&self.file, call.location().start_offset())
                        .map(|l| l.to_string())
                        .unwrap_or_else(|| "?".into());
                    super::super::survey::continue_or_fail(IngestError::Unsupported {
                        file: self.file.clone(),
                        message: format!(
                            "tests declared by a call of the class method `{name}` are not modeled; they will not run (`{}` line {line}: {why})",
                            self.class.0
                        ),
                    })?;
                    return Ok(Some(Vec::new()));
                }
                Ok(None)
            }
        }
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

fn def_at<'pr>(root: &Node<'pr>, start: usize, end: usize) -> Option<ruby_prism::DefNode<'pr>> {
    struct Find<'pr> {
        start: usize,
        end: usize,
        found: Option<ruby_prism::DefNode<'pr>>,
    }
    impl<'pr> ruby_prism::Visit<'pr> for Find<'pr> {
        fn visit_branch_node_enter(&mut self, node: Node<'pr>) {
            let loc = node.location();
            if self.found.is_none() && loc.start_offset() == self.start && loc.end_offset() == self.end {
                self.found = node.as_def_node();
            }
        }
    }
    use ruby_prism::Visit;
    let mut find = Find { start, end, found: None };
    find.visit(root);
    find.found
}

/// Byte offsets of `name` as a whole word in `text`.
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

fn uses(root: &Node<'_>, name: &str) -> Vec<Use> {
    use ruby_prism::Visit;
    struct Scan<'a> {
        name: &'a [u8],
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
                if dynamic {
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
    let mut scan = Scan { name: name.as_bytes(), out: Vec::new() };
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

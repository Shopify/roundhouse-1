//! Rails test-file ingestion — `test/models/*_test.rb` and
//! `test/controllers/*_test.rb`. Expects one or more test classes
//! (typically inheriting from `ActiveSupport::TestCase` or
//! `ActionDispatch::IntegrationTest`), at the top level or inside a
//! `module`, whose bodies are sequences of `test "name" do ... end`
//! declarations.

use ruby_prism::Node;

use crate::dialect::{MethodDef, Test, TestModule};
use crate::expr::{Expr, ExprNode};
use crate::span::Span;
use crate::{ClassId, Symbol};

use super::expr::ingest_expr;
use super::library_class::{
    ingest_library_method, library_class_from_node, library_class_from_node_with_scope,
};
use super::util::{
    class_name_path, constant_id_str, constant_path_is_rooted, constant_path_of, flatten_statements,
    module_name_path,
};
use super::{IngestError, IngestResult};

/// Returns `Ok(None)` if the file doesn't contain a class (e.g. a pure
/// helper or empty file). Unrecognized top-level constructs inside the
/// class are silently skipped — same posture as `ingest_controller`'s
/// `Unknown` fallback but without the surrounding body-item model.
///
/// When the file declares multiple top-level classes — typically a
/// helper model paired with a `*Test` class (see
/// `runtime/ruby/test/action_view/view_helpers_test.rb`'s `Article` +
/// `ViewHelpersTest` pair) — pick the `*Test` class as the test
/// module and ingest the rest as helpers on the returned module's
/// `inner_classes`. This keeps the emit path generic: every per-target
/// emitter already hoists `inner_classes` above the test body, so a
/// top-level helper class lands alongside the test file without
/// per-target plumbing changes.
pub fn ingest_test_file(source: &[u8], file: &str) -> IngestResult<Option<TestModule>> {
    Ok(ingest_test_files(source, file)?.into_iter().next())
}

/// Every test class the file declares, each as its own `TestModule`.
///
/// A Rails test file is usually one class, but not always:
/// campfire's `test/channels/room_messages_channel_test.rb` declares
/// `RoomMessagesChannelTest` and, beside it, `RoomMessagesViaStock-
/// TurboChannelTest` — the second exists to prove the STOCK
/// `Turbo::StreamsChannel` turns the same stream names away, which is
/// a different subject and so a different class. `ingest_test_file`
/// kept only the first and routed the second through
/// `inner_classes`, where a `*Test` class was then skipped on
/// purpose (see the note at the end of `ingest_test_class`): two
/// tests silently absent from every tally. Each class here becomes
/// a module of its own, and the emit names files by class, so they
/// land as two test files the way Minitest would have run them as
/// two suites. Non-test top-level classes are shared by every module
/// as helpers, the way they were for the one.
pub fn ingest_test_files(source: &[u8], file: &str) -> IngestResult<Vec<TestModule>> {
    super::sources::register(file, &String::from_utf8_lossy(source));
    let result = super::prism::parse(source, file);
    let root = result.node();
    let mut top_classes: Vec<ScopedClass<'_>> = Vec::new();
    collect_top_level_classes(&root, &[], &mut top_classes);
    if top_classes.is_empty() {
        return Ok(Vec::new());
    }
    let (mut test_nodes, helper_nodes): (Vec<_>, Vec<_>) =
        top_classes.into_iter().partition(|(_, c)| is_test_class_node(c));
    // No candidate matches: the historical single-class shape, where
    // the first top-level class is the test whatever it is called. A
    // class inside a module is not a candidate here: a support file
    // in a test directory often declares one, and it has no tests.
    if test_nodes.is_empty() {
        let mut helper_nodes = helper_nodes;
        let Some(first) = helper_nodes.iter().position(|(scope, _)| scope.is_empty()) else {
            return Ok(Vec::new());
        };
        test_nodes.push(helper_nodes.remove(first));
        return ingest_test_class(&test_nodes[0], &helper_nodes, file).map(|tm| vec![tm]);
    }
    test_nodes
        .iter()
        .map(|class| ingest_test_class(class, &helper_nodes, file))
        .collect()
}

fn ingest_test_class(
    (scope, class): &ScopedClass<'_>,
    top_level_helper_nodes: &[ScopedClass<'_>],
    file: &str,
) -> IngestResult<TestModule> {
    let mut name_path = scope.clone();
    name_path.extend(class_name_path(class).ok_or_else(|| IngestError::Unsupported {
        file: file.into(),
        message: "test class name must be a simple constant or path".into(),
    })?);
    let name = ClassId(Symbol::from(name_path.join("::")));

    let parent = class.superclass().and_then(|n| {
        constant_path_of(&n).map(|p| ClassId(Symbol::from(p.join("::"))))
    });
    let parent_span = class
        .superclass()
        .filter(|n| !n.as_constant_path_node().is_some_and(|p| constant_path_is_rooted(&p)))
        .map(|n| Span {
            file: super::sources::file_id(file),
            start: n.location().start_offset() as u32,
            end: n.location().end_offset() as u32,
        });

    // Rails convention: `ArticleTest` tests `Article`. Strip the `Test`
    // suffix off the last name-path segment. Unusual naming → None.
    let mut target = name_path
        .last()
        .and_then(|last| last.strip_suffix("Test"))
        .map(|stem| ClassId(Symbol::from(stem)));

    let mut tests: Vec<Test> = Vec::new();
    let mut setup: Option<Expr> = None;
    let mut inner_classes: Vec<crate::dialect::LibraryClass> = Vec::new();
    let mut helpers: Vec<MethodDef> = Vec::new();
    let mut constants: Vec<(Symbol, Expr)> = Vec::new();
    let mut includes: Vec<ClassId> = Vec::new();
    let mut teardowns: Vec<Expr> = Vec::new();
    if let Some(class_body) = class.body() {
        for stmt in flatten_statements(class_body) {
            if let Some(test) = ingest_test_declaration(&stmt, file)? {
                tests.push(test);
                continue;
            }
            if let Some(body) = ingest_teardown_block(&stmt, file)? {
                teardowns.push(body);
                continue;
            }
            if let Some(body) = ingest_setup_declaration(&stmt, file)? {
                // Multiple setup hooks compose in source order — append
                // by wrapping in a Seq. Rare in practice; first wins
                // when stored as Option, so accumulate.
                setup = Some(match setup.take() {
                    None => body,
                    Some(prev) => {
                        let prev_stmts = match &*prev.node {
                            ExprNode::Seq { exprs } => exprs.clone(),
                            _ => vec![prev],
                        };
                        let body_stmts = match &*body.node {
                            ExprNode::Seq { exprs } => exprs.clone(),
                            _ => vec![body],
                        };
                        let mut all = prev_stmts;
                        all.extend(body_stmts);
                        Expr::new(Span::synthetic(), ExprNode::Seq { exprs: all })
                    }
                });
                continue;
            }
            // Inline class declaration inside the test class —
            // `class Validatable; include ActiveRecord::Validations;
            // ...; end`. Captured per-test-module so the emit can
            // hoist them to file scope above the lowered test class.
            if let Some(inner) = stmt.as_class_node() {
                let lc = library_class_from_node(&inner, file)?;
                inner_classes.push(lc);
                continue;
            }
            // Non-test, non-setup `def` — instance helper method
            // (e.g. `setup_adapter_with_stub_row(id)`). Capture as
            // an ordinary method on the test class so test bodies
            // can `self.<helper>(...)` it. Definitions of `def
            // setup` and `def test_*` already short-circuited above.
            if let Some(def) = stmt.as_def_node() {
                let m = ingest_library_method(&def, &name, file)?;
                helpers.push(m);
                continue;
            }
            // `include ModName` at class-body scope. Captured for
            // the spinel emit to replay verbatim so bare-name refs
            // (`Router`, `FormBuilder`) resolve under CRuby. TS emit
            // ignores these — its framework-namespace import-stripper
            // handles the same refs by a different mechanism.
            if let Some(call) = stmt.as_call_node() {
                // `tests RoomMessagesChannel` — ActionCable's channel
                // and connection test cases (and ActionController::
                // TestCase) name their subject with this macro when
                // the class name does not: `RoomMessagesViaStock-
                // TurboChannelTest` tests `Turbo::StreamsChannel`. It
                // overrides the `FooTest` → `Foo` convention above.
                if call.receiver().is_none() && constant_id_str(&call.name()) == "tests" {
                    if let Some(args) = call.arguments() {
                        if let Some(arg) = args.arguments().iter().next() {
                            if let Some(path) = constant_path_of(&arg) {
                                target = Some(ClassId(Symbol::from(path.join("::"))));
                            }
                        }
                    }
                    continue;
                }
                if call.receiver().is_none()
                    && constant_id_str(&call.name()) == "include"
                {
                    if let Some(args) = call.arguments() {
                        for arg in args.arguments().iter() {
                            if let Some(path) = constant_path_of(&arg) {
                                includes.push(ClassId(Symbol::from(path.join("::"))));
                            }
                        }
                    }
                    continue;
                }
            }
            // Class-level constant assignment — `TABLE = [...]`,
            // `Article = Struct.new(...)`. Captured here so the test
            // emit can hoist them to file scope as `const NAME =
            // <value>`, which matches Ruby's lexical constant
            // resolution for bare references inside test methods.
            if let Some(cw) = stmt.as_constant_write_node() {
                let const_name = std::str::from_utf8(cw.name().as_slice())
                    .ok()
                    .map(Symbol::from);
                if let Some(const_name) = const_name {
                    let value = ingest_expr(&cw.value(), file)?;
                    constants.push((const_name, value));
                }
                continue;
            }
        }
    }

    // ActiveSupport runs `teardown do … end` blocks after every test,
    // passed or failed, after the class's own `def teardown` and in
    // reverse of the order they were declared. The harness calls the
    // test's `teardown` after each test the same way, so the blocks
    // become that method.
    let teardown_blocks = !teardowns.is_empty();
    let teardown_def = helpers.iter().any(|m| m.name.as_str() == "teardown");
    if teardown_blocks {
        let mut exprs: Vec<Expr> = Vec::new();
        let own = helpers.iter().position(|m| m.name.as_str() == "teardown");
        let name_span = match own.map(|i| helpers.remove(i)) {
            Some(m) => {
                exprs.push(m.body);
                m.name_span
            }
            None => Span::synthetic(),
        };
        exprs.extend(teardowns.into_iter().rev());
        helpers.push(MethodDef {
            name_span,
            name: Symbol::from("teardown"),
            receiver: crate::dialect::MethodReceiver::Instance,
            visibility: crate::dialect::MethodVisibility::Public,
            params: Vec::new(),
            unsupported_formals: None,
            has_anonymous_block: false,
            body: Expr::new(Span::synthetic(), ExprNode::Seq { exprs }),
            signature: None,
            effects: crate::effect::EffectSet::default(),
            enclosing_class: Some(name.0.clone()),
            kind: crate::dialect::AccessorKind::Method,
            is_async: false,
            mutates_self: false,
            block_param: None,
        });
    }

    // Top-level helper classes (e.g. `class Article < ActiveRecord::
    // Base` declared next to `ViewHelpersTest` in the same file) ingest
    // as LibraryClass and flow through the same `inner_classes` channel
    // as body-internal helpers. The per-target emit paths already hoist
    // `inner_classes` to file scope above the test body — same routing,
    // no further per-target changes needed.
    //
    // Additional `*Test`-shaped classes are each a module of their
    // own — `ingest_test_files` splits them before this runs — so
    // every node here is a genuine helper. (They used to be dropped:
    // an inner class does not get the `< Minitest::Test → < TestBase`
    // parent rewrite the test class does, so emitting one inline
    // would have Minitest find it under CRuby and run it against the
    // wrong assertion surface.)
    for (helper_scope, helper_node) in top_level_helper_nodes {
        let lc = library_class_from_node_with_scope(helper_node, helper_scope, file)?;
        inner_classes.push(lc);
    }

    Ok(TestModule {
        name,
        parent,
        parent_span,
        target,
        tests,
        setup,
        inner_classes,
        helpers,
        teardown_blocks,
        teardown_def,
        constants,
        includes,
    })
}

/// Classes the test helper reopens (no superclass: `class
/// ActiveSupport::TestCase`, `module ActiveSupport; class TestCase`,
/// `class ActionDispatch::IntegrationTest`) that declare a teardown,
/// block or `def`. Only a reopen's `setup` is carried into the tests
/// (`ingest_test_case_setup`); a teardown there would run after none.
pub fn helper_reopen_teardowns(source: &[u8], file: &str) -> Vec<String> {
    let result = super::prism::parse(source, file);
    let root = result.node();
    let mut classes: Vec<ScopedClass<'_>> = Vec::new();
    collect_top_level_classes(&root, &[], &mut classes);
    let mut out = Vec::new();
    for (scope, class) in classes {
        if class.superclass().is_some() {
            continue;
        }
        let mut path = scope;
        path.extend(class_name_path(&class).unwrap_or_default());
        // Only the framework's test cases (every one is a `Minitest::Test`):
        // a helper's own class may define `teardown` for its own reasons.
        let rails = path.first().is_some_and(|ns| ns.starts_with("Active") || ns.starts_with("Action"))
            && path.last().is_some_and(|n| n.ends_with("TestCase") || n.ends_with("IntegrationTest"));
        let minitest = path.first().is_some_and(|ns| ns == "Minitest");
        if !(rails || minitest) {
            continue;
        }
        let Some(body) = class.body() else { continue };
        let declares = flatten_statements(body).iter().any(|stmt| {
            let call = stmt.as_call_node().is_some_and(|c| c.receiver().is_none() && constant_id_str(&c.name()) == "teardown");
            let def = stmt.as_def_node().is_some_and(|d| d.name().as_slice() == b"teardown");
            call || def
        });
        if declares {
            out.push(path.join("::"));
        }
    }
    out
}

/// Every `establish_connection` call in the test helper, as written:
/// at the top level, under a condition, in a class body or `begin`, or
/// in a method the helper calls. Any of them can name the database the
/// helper's tests run on, so none is left out.
pub fn helper_connections(source: &[u8], file: &str) -> IngestResult<Vec<crate::app::HelperConnection>> {
    struct Calls<'f> {
        file: &'f str,
        top_level: Vec<(usize, usize)>,
        found: Vec<IngestResult<crate::app::HelperConnection>>,
    }
    impl<'pr> ruby_prism::Visit<'pr> for Calls<'_> {
        fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
            if constant_id_str(&node.name()) == "establish_connection" {
                let at = node.location();
                let top_level = self.top_level.contains(&(at.start_offset(), at.end_offset()));
                self.found.push(
                    ingest_expr(&node.as_node(), self.file)
                        .map(|call| crate::app::HelperConnection { call, top_level }),
                );
            }
            ruby_prism::visit_call_node(self, node);
        }
    }
    let result = super::prism::parse(source, file);
    let root = result.node();
    let top_level = root
        .as_program_node()
        .map(|program| {
            program
                .statements()
                .body()
                .iter()
                .filter(|stmt| stmt.as_call_node().is_some())
                .map(|stmt| (stmt.location().start_offset(), stmt.location().end_offset()))
                .collect()
        })
        .unwrap_or_default();
    let mut calls = Calls { file, top_level, found: Vec::new() };
    ruby_prism::Visit::visit(&mut calls, &root);
    calls.found.into_iter().collect()
}

/// Methods whose call runs against the database or changes the
/// connection: the helper's own schema load (`ActiveRecord::Schema
/// .define`, `load_schema`), a statement on its connection, a
/// connection of another kind. Any of these the harness does not carry.
const DATABASE_CALLS: &[&str] = &[
    "connection",
    "lease_connection",
    "with_connection",
    "connection_pool",
    "connects_to",
    "remove_connection",
    "load_schema",
    "load_schema!",
    "maintain_test_schema!",
    "purge",
    "migrate",
];

/// Calls that set the connection a class runs on.
const CONNECTING_CALLS: &[&str] = &["establish_connection", "connects_to", "remove_connection"];

/// Receivers whose every call touches the database.
const DATABASE_RECEIVERS: &[&str] =
    &["ActiveRecord::Schema", "ActiveRecord::Migration", "ActiveRecord::Migrator", "ActiveRecord::Tasks"];

/// Calls that stop the file where they stand, bare or on `Kernel` or
/// `Process`.
const EXITS: &[&str] = &["exit", "exit!", "abort", "raise", "fail", "throw"];

/// Calls that load another file, bare or on `Kernel`.
const LOADS: &[&str] = &["require", "require_relative", "load", "autoload"];

/// Where a scanned file stands: the test helper itself, or any other
/// file of the app.
#[derive(Clone, Copy, PartialEq)]
enum Scanned {
    Helper,
    Other,
}

struct Scan<'a> {
    kind: Scanned,
    local: &'a dyn Fn(&str) -> bool,
    found: Vec<String>,
    exits: bool,
    /// Inside a method or block: code that runs when called, not as the
    /// file loads.
    deferred: usize,
}

impl Scan<'_> {
    fn runs_at_load(&self) -> bool {
        self.deferred == 0
    }
}

impl<'pr> ruby_prism::Visit<'pr> for Scan<'_> {
    fn visit_def_node(&mut self, node: &ruby_prism::DefNode<'pr>) {
        self.deferred += 1;
        ruby_prism::visit_def_node(self, node);
        self.deferred -= 1;
    }
    fn visit_block_node(&mut self, node: &ruby_prism::BlockNode<'pr>) {
        // A helper's block (`Dir[...].each { |f| require f }`) runs as it
        // loads; another file's is a test body or a callback.
        let defer = self.kind == Scanned::Other;
        self.deferred += defer as usize;
        ruby_prism::visit_block_node(self, node);
        self.deferred -= defer as usize;
    }
    fn visit_return_node(&mut self, node: &ruby_prism::ReturnNode<'pr>) {
        if self.deferred == 0 {
            self.exits = true;
        }
        ruby_prism::visit_return_node(self, node);
    }
    fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
        let name = constant_id_str(&node.name()).to_string();
        let text = |n: &Node<'_>| String::from_utf8_lossy(n.location().as_slice()).to_string();
        let receiver = node.receiver().map(|r| text(&r).trim_start_matches("::").to_string());
        let bare_or_on = |modules: &[&str]| receiver.as_deref().is_none_or(|r| modules.contains(&r));
        let args: Vec<Node<'_>> = node.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default();
        // Its first line, which names it.
        let call = text(&node.as_node()).lines().next().unwrap_or_default().trim_end().to_string();
        // `send(:establish_connection, …)` and its kin.
        let sent = matches!(name.as_str(), "send" | "public_send" | "__send__")
            .then(|| args.first())
            .flatten()
            .and_then(|a| {
                a.as_symbol_node()
                    .map(|s| String::from_utf8_lossy(s.unescaped()).to_string())
                    .or_else(|| a.as_string_node().map(|s| String::from_utf8_lossy(s.unescaped()).to_string()))
            });
        let on_schema = receiver
            .as_deref()
            .is_some_and(|r| DATABASE_RECEIVERS.iter().any(|root| r == *root || r.starts_with(&format!("{root}::")) || r.starts_with(&format!("{root}["))));
        match self.kind {
            Scanned::Helper => {
                if DATABASE_CALLS.contains(&name.as_str()) || on_schema {
                    self.found.push(format!("`{call}` runs against the database"));
                } else if sent.as_deref().is_some_and(|m| CONNECTING_CALLS.contains(&m)) {
                    self.found.push(format!("`{call}` connects by a dynamic call"));
                } else if LOADS.contains(&name.as_str()) && bare_or_on(&["Kernel"]) {
                    // Only a gem's `require "name"` is known not to load a
                    // file of the app's.
                    let gem = name == "require"
                        && args.len() == 1
                        && args[0].as_string_node().is_some_and(|s| {
                            let feature = String::from_utf8_lossy(s.unescaped()).to_string();
                            !feature.starts_with(['.', '/', '~']) && !(self.local)(&feature)
                        });
                    if !gem {
                        self.found.push(format!("`{call}` loads a file the harness does not carry"));
                    }
                } else if self.deferred == 0 && EXITS.contains(&name.as_str()) && bare_or_on(&["Kernel", "Process"]) {
                    self.exits = true;
                }
            }
            Scanned::Other => {
                if CONNECTING_CALLS.contains(&name.as_str()) || sent.as_deref().is_some_and(|m| CONNECTING_CALLS.contains(&m)) {
                    self.found.push(format!("`{call}` sets another connection"));
                } else if on_schema {
                    self.found.push(format!("`{call}` runs against the database"));
                } else if self.runs_at_load() && DATABASE_CALLS.contains(&name.as_str()) {
                    self.found.push(format!("`{call}` runs against the database as the file loads"));
                }
            }
        }
        ruby_prism::visit_call_node(self, node);
    }
}

/// What in the test helper could set or change the database its tests
/// run on, besides its `establish_connection` calls, that the emitted
/// harness does not carry: a call that runs against the database or
/// reconnects (anywhere in the file), any load of another file but a
/// gem's `require "name"` (`local` says whether the app has the file),
/// and a top-level statement before its first connection that can stop
/// the file first (`return`, `exit`, `raise` and the like), after which
/// the connection is no longer certain.
pub fn helper_connection_blockers(source: &[u8], file: &str, local: &dyn Fn(&str) -> bool) -> Vec<String> {
    let result = super::prism::parse_silent(source);
    let root = result.node();
    let mut scan = Scan { kind: Scanned::Helper, local, found: Vec::new(), exits: false, deferred: 0 };
    ruby_prism::Visit::visit(&mut scan, &root);
    let mut found: Vec<String> = scan.found.into_iter().map(|why| format!("{file}: {why}")).collect();
    // The exits that matter come before the first connection, at the
    // top level.
    if let Some(program) = root.as_program_node() {
        for stmt in program.statements().body().iter() {
            if count_connections(&stmt) > 0 {
                break;
            }
            let mut exits = Scan { kind: Scanned::Helper, local, found: Vec::new(), exits: false, deferred: 0 };
            ruby_prism::Visit::visit(&mut exits, &stmt);
            if exits.exits {
                found.push(format!(
                    "{file}: `{}` can stop the helper before its connection",
                    String::from_utf8_lossy(stmt.location().as_slice())
                ));
                break;
            }
        }
    }
    found
}

/// What in another Ruby file of the app could set or change the
/// database the helper's connection names: a connecting call
/// (`establish_connection`, `connects_to`, `remove_connection`, also
/// through `send`), a schema or migration call, or a statement on a
/// connection that runs as the file loads (not in a method, a test body
/// or another block).
pub fn connection_blockers(source: &[u8], file: &str) -> Vec<String> {
    let result = super::prism::parse_silent(source);
    let local = |_: &str| false;
    let mut scan = Scan { kind: Scanned::Other, local: &local, found: Vec::new(), exits: false, deferred: 0 };
    ruby_prism::Visit::visit(&mut scan, &result.node());
    scan.found.into_iter().map(|why| format!("{file}: {why}")).collect()
}

fn count_connections(node: &Node<'_>) -> usize {
    struct Count(usize);
    impl<'pr> ruby_prism::Visit<'pr> for Count {
        fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
            if constant_id_str(&node.name()) == "establish_connection" {
                self.0 += 1;
            }
            ruby_prism::visit_call_node(self, node);
        }
    }
    let mut count = Count(0);
    ruby_prism::Visit::visit(&mut count, node);
    count.0
}

/// The app-wide `setup do … end` an app's `test/test_helper.rb` puts on
/// `ActiveSupport::TestCase` (reopened there, or nested as `module
/// ActiveSupport; class TestCase`). Rails runs it before every test
/// case's own setup, so the caller splices it ahead of each test
/// module's `setup` — the same move `splice_test_helpers` makes for the
/// helper modules that file includes.
///
/// campfire's is three statements — clear the cable test adapter,
/// replace the web-push pool, forbid network — and the middle one is
/// why this exists: `Room::PushTest` waits on the pool's task counter,
/// which is cumulative, so every test needs the fresh pool the app's
/// own setup gives it. Without the splice the suite's first push test
/// leaves a count the next one's wait is satisfied by before its tasks
/// have run.
///
/// Only the `setup` hook is read. `fixtures :all`, `parallelize`, and
/// the `include` list are the harness's concern (the includes are read
/// by `ingest_test_helper_modules`), and a `teardown` there has no
/// home yet — the emitted `TestBase#teardown` is ours.
pub fn ingest_test_case_setup(source: &[u8], file: &str) -> IngestResult<Option<Expr>> {
    let result = super::prism::parse(source, file);
    let root = result.node();
    let mut classes: Vec<ScopedClass<'_>> = Vec::new();
    collect_top_level_classes(&root, &[], &mut classes);
    for (_, class) in classes {
        let Some(path) = class_name_path(&class) else { continue };
        if path.last().map(String::as_str) != Some("TestCase") {
            continue;
        }
        let Some(body) = class.body() else { continue };
        for stmt in flatten_statements(body) {
            if let Some(setup) = ingest_setup_declaration(&stmt, file)? {
                return Ok(Some(setup));
            }
        }
    }
    Ok(None)
}

/// Collect every class the file declares outside another class, each
/// with the path of the modules that enclose it. `module Models; class
/// PostTest` gives `(["Models"], PostTest)`, the same class as `class
/// Models::PostTest`. The walk does not go into class bodies: a class
/// inside a test class is a helper of that test class.
fn collect_top_level_classes<'pr>(
    node: &Node<'pr>,
    scope: &[String],
    out: &mut Vec<ScopedClass<'pr>>,
) {
    if let Some(c) = node.as_class_node() {
        out.push((scope.to_vec(), c));
        return;
    }
    if let Some(p) = node.as_program_node() {
        collect_top_level_classes(&p.statements().as_node(), scope, out);
        return;
    }
    if let Some(s) = node.as_statements_node() {
        for stmt in s.body().iter() {
            collect_top_level_classes(&stmt, scope, out);
        }
        return;
    }
    if let Some(m) = node.as_module_node() {
        let mut inner = scope.to_vec();
        inner.extend(module_name_path(&m).unwrap_or_default());
        if let Some(body) = m.body() {
            collect_top_level_classes(&body, &inner, out);
        }
    }
}

type ScopedClass<'pr> = (Vec<String>, ruby_prism::ClassNode<'pr>);

/// Heuristic: a class is "the test class" when its name ends with
/// `Test` (e.g. `ViewHelpersTest`, `InflectorTest`) OR when its parent
/// path's leaf segment is a known test base (`Test`, `TestCase`,
/// `IntegrationTest`). The two conditions together cover both the
/// framework's own `< Minitest::Test` shape and Rails app tests'
/// `< ActiveSupport::TestCase` / `< ActionDispatch::IntegrationTest`.
fn is_test_class_node(c: &ruby_prism::ClassNode<'_>) -> bool {
    if let Some(name_path) = class_name_path(c) {
        if let Some(last) = name_path.last() {
            if last.ends_with("Test") {
                return true;
            }
        }
    }
    if let Some(parent_node) = c.superclass() {
        if let Some(parent_path) = constant_path_of(&parent_node) {
            if let Some(last) = parent_path.last() {
                if matches!(last.as_str(), "Test" | "TestCase" | "IntegrationTest") {
                    return true;
                }
            }
        }
    }
    false
}

/// Recognize `setup do ... end` (Call with method=setup, block body)
/// and `def setup; ...; end` (DefNode named setup). Returns the body
/// expression in either case. Other shapes return Ok(None).
fn ingest_setup_declaration(
    stmt: &Node<'_>,
    file: &str,
) -> IngestResult<Option<Expr>> {
    // `setup do ... end` form.
    if let Some(call) = stmt.as_call_node() {
        if call.receiver().is_some() {
            return Ok(None);
        }
        if constant_id_str(&call.name()) != "setup" {
            return Ok(None);
        }
        if let Some(block_ref) = call.block() {
            if let Some(block) = block_ref.as_block_node() {
                let body = match block.body() {
                    Some(body_node) => ingest_expr(&body_node, file)?,
                    None => Expr::new(Span::synthetic(), ExprNode::Seq { exprs: vec![] }),
                };
                return Ok(Some(body));
            }
        }
        return Ok(None);
    }
    // `def setup; ...; end` form.
    if let Some(def) = stmt.as_def_node() {
        let name_bytes = def.name().as_slice();
        if std::str::from_utf8(name_bytes).ok() != Some("setup") {
            return Ok(None);
        }
        super::forwarding::reject_entrypoint(&def, file, "test setup hook")?;
        let body = match def.body() {
            Some(body_node) => ingest_expr(&body_node, file)?,
            None => Expr::new(Span::synthetic(), ExprNode::Seq { exprs: vec![] }),
        };
        return Ok(Some(body));
    }
    Ok(None)
}

/// `teardown do … end`: the block's body. Any other `teardown` call
/// (`teardown :close_files`, a block argument) is refused by name: it
/// would otherwise fall to the unrecognized-statement drop below and
/// its cleanup would silently never run.
fn ingest_teardown_block(stmt: &Node<'_>, file: &str) -> IngestResult<Option<Expr>> {
    let Some(call) = stmt.as_call_node() else { return Ok(None) };
    if call.receiver().is_some() || constant_id_str(&call.name()) != "teardown" {
        return Ok(None);
    }
    let block = call.block().and_then(|b| b.as_block_node());
    match block {
        Some(block) if call.arguments().is_none() => Ok(Some(match block.body() {
            Some(body_node) => ingest_expr(&body_node, file)?,
            None => Expr::new(Span::synthetic(), ExprNode::Seq { exprs: vec![] }),
        })),
        _ => Err(IngestError::Unsupported {
            file: file.into(),
            message: "test teardown not modeled: only `teardown do … end` runs after each test".into(),
        }),
    }
}

/// Recognize a single test declaration. Two shapes are supported:
///
///   `test "name" do ... end`  — Rails AS::TestCase DSL. Test name is the
///                                string argument verbatim.
///   `def test_*; ...; end`    — vanilla Minitest. Test name is the
///                                method name with the `test_` prefix
///                                stripped and underscores replaced with
///                                spaces (mirrors how Minitest reports
///                                them; keeps the two shapes
///                                indistinguishable downstream).
///
/// Returns `None` for any other statement shape (intentional — we
/// silently drop unrecognized body items rather than error; they'll
/// surface when the test tries to reference something that isn't
/// emitted).
fn ingest_test_declaration(
    stmt: &Node<'_>,
    file: &str,
) -> IngestResult<Option<Test>> {
    // `def test_*; ...; end` form (vanilla Minitest).
    if let Some(def) = stmt.as_def_node() {
        let name_bytes = def.name().as_slice();
        let Some(method_name) = std::str::from_utf8(name_bytes).ok() else {
            return Ok(None);
        };
        let Some(stem) = method_name.strip_prefix("test_") else {
            return Ok(None);
        };
        super::forwarding::reject_entrypoint(&def, file, "test entrypoint")?;
        let name = stem.replace('_', " ");
        let body = match def.body() {
            Some(body_node) => ingest_expr(&body_node, file)?,
            None => Expr::new(Span::synthetic(), ExprNode::Seq { exprs: vec![] }),
        };
        return Ok(Some(Test { name, body }));
    }

    let Some(call) = stmt.as_call_node() else {
        return Ok(None);
    };
    // Must be a bare-name `test(...)` call.
    if call.receiver().is_some() {
        return Ok(None);
    }
    let method = constant_id_str(&call.name()).to_string();
    if method != "test" {
        return Ok(None);
    }

    // First positional argument: the test's name as a string literal.
    let Some(args) = call.arguments() else {
        return Ok(None);
    };
    let args_iter = args.arguments();
    let Some(first_arg) = args_iter.iter().next() else {
        return Ok(None);
    };
    let Some(string_node) = first_arg.as_string_node() else {
        return Ok(None);
    };
    let name = String::from_utf8_lossy(string_node.unescaped()).into_owned();

    // The block body — Prism exposes it via `call.block()` when present.
    let body = match call.block() {
        Some(block_ref) => match block_ref.as_block_node() {
            Some(block) => match block.body() {
                Some(body_node) => ingest_expr(&body_node, file)?,
                None => Expr::new(Span::synthetic(), ExprNode::Seq { exprs: vec![] }),
            },
            None => Expr::new(Span::synthetic(), ExprNode::Seq { exprs: vec![] }),
        },
        None => Expr::new(Span::synthetic(), ExprNode::Seq { exprs: vec![] }),
    };

    Ok(Some(Test { name, body }))
}

#[cfg(test)]
mod tests {
    use super::*;

    // campfire's `test/channels/room_messages_channel_test.rb`: two
    // `< ActionCable::Channel::TestCase` classes in one file, the
    // second naming its subject with `tests Turbo::StreamsChannel`.
    // Before `ingest_test_files`, the second class was demoted to a
    // helper and then skipped for being a test — two tests in no
    // tally.
    const TWO_CLASSES: &str = r#"
require "test_helper"

class RoomMessagesChannelTest < ActionCable::Channel::TestCase
  tests RoomMessagesChannel

  setup do
    @room = rooms(:designers)
  end

  test "a member may subscribe" do
    assert true
  end

  test "an outsider may not" do
    assert true
  end
end

class RoomMessagesViaStockTurboChannelTest < ActionCable::Channel::TestCase
  tests Turbo::StreamsChannel

  test "the stock channel refuses a room message stream" do
    assert true
  end
end
"#;

    #[test]
    fn every_test_class_in_a_file_is_its_own_module() {
        let tms = ingest_test_files(TWO_CLASSES.as_bytes(), "room_messages_channel_test.rb")
            .expect("ingest");
        let names: Vec<&str> = tms.iter().map(|tm| tm.name.0.as_str()).collect();
        assert_eq!(
            names,
            ["RoomMessagesChannelTest", "RoomMessagesViaStockTurboChannelTest"]
        );
        assert_eq!(tms[0].tests.len(), 2);
        assert_eq!(tms[1].tests.len(), 1);
        // Neither is the other's helper.
        assert!(tms.iter().all(|tm| tm.inner_classes.is_empty()));
        // The first module still carries its own setup.
        assert!(tms[0].setup.is_some());
        assert!(tms[1].setup.is_none());
    }

    #[test]
    fn tests_macro_names_the_subject() {
        let tms = ingest_test_files(TWO_CLASSES.as_bytes(), "room_messages_channel_test.rb")
            .expect("ingest");
        let targets: Vec<&str> = tms
            .iter()
            .map(|tm| tm.target.as_ref().map(|t| t.0.as_str()).unwrap_or(""))
            .collect();
        assert_eq!(targets, ["RoomMessagesChannel", "Turbo::StreamsChannel"]);
    }

    #[test]
    fn single_class_wrapper_still_answers_the_first() {
        let tm = ingest_test_file(TWO_CLASSES.as_bytes(), "room_messages_channel_test.rb")
            .expect("ingest")
            .expect("a test class");
        assert_eq!(tm.name.0.as_str(), "RoomMessagesChannelTest");
    }

    // A test class that a `module` wraps is the same class as
    // `class Models::PostTest`. Ingest used to read only the file's
    // top-level classes, so it lost this one and reported nothing.
    #[test]
    fn a_test_class_inside_a_module_keeps_its_namespace() {
        let src = r#"
module Models
  class Helper
  end

  class PostTest < ActiveSupport::TestCase
    class Inner
    end

    test "truth" do
      assert true
    end
  end
end
"#;
        let tms = ingest_test_files(src.as_bytes(), "post_test.rb").expect("ingest");
        let names: Vec<&str> = tms.iter().map(|tm| tm.name.0.as_str()).collect();
        assert_eq!(names, ["Models::PostTest"]);
        assert_eq!(tms[0].target.as_ref().map(|t| t.0.as_str()), Some("Post"));
        assert_eq!(tms[0].tests.len(), 1);
        let helpers: Vec<&str> =
            tms[0].inner_classes.iter().map(|lc| lc.name.0.as_str()).collect();
        assert_eq!(helpers, ["Inner", "Models::Helper"]);
    }

    // Ingest reads every `.rb` file in the test directories, so a
    // support class in a module reaches it too. It is not a test.
    #[test]
    fn a_support_class_inside_a_module_is_not_a_test() {
        let src = "module Support\n  class Thing\n    def x\n      1\n    end\n  end\nend\n";
        let tms = ingest_test_files(src.as_bytes(), "thing.rb").expect("ingest");
        assert!(tms.is_empty());
    }

    // When no class looks like a test, the first top-level class is
    // the test. A class inside a module does not take its place.
    #[test]
    fn the_fallback_test_class_is_a_top_level_class() {
        let src = r#"
module Fakes
  class Client
  end
end

class ProbeChecks < Minitest::Unit
  def test_x
    assert true
  end
end
"#;
        let tms = ingest_test_files(src.as_bytes(), "probe_checks.rb").expect("ingest");
        let names: Vec<&str> = tms.iter().map(|tm| tm.name.0.as_str()).collect();
        assert_eq!(names, ["ProbeChecks"]);
        let helpers: Vec<&str> =
            tms[0].inner_classes.iter().map(|lc| lc.name.0.as_str()).collect();
        assert_eq!(helpers, ["Fakes::Client"]);
    }
}

/// A test class's superclass is a name Ruby resolves in the modules
/// around the class: `class SkiplistTest < TestCase` inside `module
/// SqliteTestCompat` subclasses the `SqliteTestCompat::TestCase` the
/// helper defines. Where Rubydex resolves the written name to a test
/// class the app defines, that class is the parent, so the emitted
/// file loads it and the analyzer inherits its helpers. Any other
/// parent keeps its written path.
pub(super) fn resolve_test_parents(app: &mut crate::App) {
    let resolver = app.const_resolver.for_sources(&app.sources);
    let defined: std::collections::HashSet<ClassId> = app.test_modules.iter().map(|tm| tm.name.clone()).collect();
    for tm in &mut app.test_modules {
        let (Some(span), Some(parent)) = (tm.parent_span, &tm.parent) else { continue };
        let path: Vec<Symbol> = parent.0.as_str().split("::").map(Symbol::from).collect();
        if let Some(id) = resolver.namespace(span, &path).filter(|id| defined.contains(*id)) {
            tm.parent = Some(id.clone());
        }
    }
}

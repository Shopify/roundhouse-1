//! Rails test-file ingestion — `test/models/*_test.rb` and
//! `test/controllers/*_test.rb`. Expects one or more test classes
//! (typically inheriting from `ActiveSupport::TestCase` or
//! `ActionDispatch::IntegrationTest`), at the top level or inside a
//! `module`, whose bodies are sequences of `test "name" do ... end`
//! declarations.

use ruby_prism::Node;

use crate::dialect::{
    AccessorKind, MethodDef, MethodReceiver, MethodVisibility, Test, TestFileRequire, TestModule, TestRequireTarget,
};
use crate::effect::EffectSet;
use crate::expr::{Expr, ExprNode, Literal};
use crate::span::Span;
use crate::ty::Ty;
use crate::{ClassId, Symbol};

use super::expr::ingest_expr;
use super::library_class::{declared_classes_under, ingest_library_method};
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
    ingest_test_classes(source, file, true)
}

/// The test classes a support file (`test/test_helper.rb`) defines: only
/// classes that read as test classes, with no single-class fallback, so a
/// helper's plain `class RecordingLogger < Logger` is never mistaken for one.
pub fn ingest_test_bases(source: &[u8], file: &str) -> IngestResult<Vec<TestModule>> {
    ingest_test_classes(source, file, false)
}

fn ingest_test_classes(
    source: &[u8],
    file: &str,
    single_class_fallback: bool,
) -> IngestResult<Vec<TestModule>> {
    super::sources::register(file, &String::from_utf8_lossy(source));
    let result = super::prism::parse(source, file);
    let root = result.node();
    let mut top_classes: Vec<ScopedClass<'_>> = Vec::new();
    let mut helper_modules: Vec<ScopedModule<'_>> = Vec::new();
    collect_top_level_declarations(&root, &[], &mut top_classes, Some(&mut helper_modules));
    if top_classes.is_empty() {
        refuse_unreached_test_classes(&root, &[], file)?;
        return Ok(Vec::new());
    }
    let (test_nodes, helper_nodes): (Vec<_>, Vec<_>) =
        top_classes.into_iter().partition(|(_, c)| is_test_class_node(c));
    // No candidate matches: the historical single-class shape, where
    // the first top-level class is the test whatever it is called. A
    // class inside a module is not a candidate here: a support file
    // in a test directory often declares one, and it has no tests.
    if test_nodes.is_empty() {
        if !single_class_fallback {
            return Ok(Vec::new());
        }
        let mut helper_nodes = helper_nodes;
        let Some(first) = helper_nodes.iter().position(|(scope, c)| scope.is_empty() && may_be_test_class(c)) else {
            refuse_unreached_test_classes(&root, &[], file)?;
            return Ok(Vec::new());
        };
        let first = helper_nodes.remove(first);
        let mut test_nodes = Vec::new();
        push_with_nested_test_classes(first, &mut test_nodes);
        refuse_unreached_test_classes(&root, &test_nodes, file)?;
        return Ok(with_file_requires(ingest_each_test_class(&test_nodes, &helper_nodes, &helper_modules, file)?, &root, file));
    }
    // A test class declared in a test class's body (Core's
    // `class PermissionGeneratorTest … class ValidateInitialTest <
    // PermissionGeneratorTest`) is a suite Minitest runs on its own,
    // `PermissionGeneratorTest::ValidateInitialTest`. It used to ride
    // along as an inner helper class, emitted verbatim and never run.
    let mut all_test_nodes = Vec::new();
    for class in test_nodes {
        push_with_nested_test_classes(class, &mut all_test_nodes);
    }
    refuse_unreached_test_classes(&root, &all_test_nodes, file)?;
    Ok(with_file_requires(ingest_each_test_class(&all_test_nodes, &helper_nodes, &helper_modules, file)?, &root, file))
}

/// Each of the file's classes with the file's top-level loads
/// (`TestModule::file_requires`): every emitted class file runs them, as
/// the authored file ran them before any of its classes.
fn with_file_requires(mut modules: Vec<TestModule>, root: &Node<'_>, file: &str) -> Vec<TestModule> {
    let requires = file_requires(root, file);
    for tm in &mut modules {
        tm.file_requires = requires.clone();
    }
    modules
}

/// The `require`, `require_relative` and `load` calls among the file's
/// top-level statements, bare or on `Kernel`, in source order. A literal
/// feature or relative path is kept as such, as is `require
/// File.expand_path("path", __dir__)` (the path from the file's
/// directory); any other load is kept as written, unmodeled.
fn file_requires(root: &Node<'_>, file: &str) -> Vec<TestFileRequire> {
    let Some(program) = root.as_program_node() else { return Vec::new() };
    let text = |n: &Node<'_>| String::from_utf8_lossy(n.location().as_slice()).to_string();
    let literal = |n: &Node<'_>| n.as_string_node().map(|s| String::from_utf8_lossy(s.unescaped()).to_string());
    let mut out = Vec::new();
    for stmt in program.statements().body().iter() {
        let Some(call) = stmt.as_call_node() else { continue };
        let name = constant_id_str(&call.name());
        if !LOADS.contains(&name) || name == "autoload" {
            continue;
        }
        if call.receiver().is_some_and(|r| text(&r).trim_start_matches("::") != "Kernel") || call.block().is_some() {
            continue;
        }
        let args: Vec<Node<'_>> = call.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default();
        let expanded = || {
            let inner = args.first()?.as_call_node()?;
            if constant_id_str(&inner.name()) != "expand_path"
                || inner.receiver().is_none_or(|r| text(&r).trim_start_matches("::") != "File")
            {
                return None;
            }
            let inner_args: Vec<Node<'_>> = inner.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default();
            match inner_args.as_slice() {
                [path, dir] if text(dir) == "__dir__" => literal(path),
                _ => None,
            }
        };
        let target = match (name, args.as_slice()) {
            ("require", [arg]) if literal(arg).is_some() => TestRequireTarget::Feature(literal(arg).unwrap_or_default()),
            ("require_relative", [arg]) if literal(arg).is_some() => TestRequireTarget::Relative(literal(arg).unwrap_or_default()),
            ("require", [_]) if expanded().is_some() => TestRequireTarget::Relative(expanded().unwrap_or_default()),
            _ => TestRequireTarget::Unsupported(text(&stmt).lines().next().unwrap_or_default().trim_end().to_string()),
        };
        let line = super::sources::line_at(file, stmt.location().start_offset()).unwrap_or(0);
        out.push(TestFileRequire { target, line });
    }
    out
}

/// Ingest each test class on its own. A class that cannot be ingested is
/// refused BY NAME (the error names the class); under `--survey` the
/// refusal is recorded and the file's other classes still ingest, so one
/// unsupported construct does not take its neighbours' tests with it.
fn ingest_each_test_class(
    test_nodes: &[ScopedClass<'_>],
    helper_nodes: &[ScopedClass<'_>],
    helper_modules: &[ScopedModule<'_>],
    file: &str,
) -> IngestResult<Vec<TestModule>> {
    let mut out = Vec::new();
    // A test class the file declares by two class bodies (`class XTest <
    // ActiveSupport::TestCase … end; class XTest … end`) is one class
    // whose body is split: each body as a test file of its own would run
    // without the other's (the second without its superclass).
    let names: Vec<String> = test_nodes.iter().map(scoped_class_name).collect();
    let mut reported = crate::det::HashSet::default();
    for (class, name) in test_nodes.iter().zip(&names) {
        let bodies: Vec<String> = test_nodes
            .iter()
            .zip(&names)
            .filter(|(_, other)| *other == name)
            .map(|((_, c), _)| super::sources::line_at(file, c.location().start_offset()).map_or_else(|| "?".into(), |l| l.to_string()))
            .collect();
        if bodies.len() > 1 {
            if reported.insert(name.clone()) {
                super::survey::unwrap_or_record::<()>(Err(IngestError::Unsupported {
                    file: file.into(),
                    message: format!(
                        "test class `{name}` refused: it is declared by more than one class body in this file, which is not modeled; \
                         its tests would not run (`{name}`, lines {})",
                        bodies.join(", ")
                    ),
                }))?;
            }
            continue;
        }
        let result = ingest_test_class(class, helper_nodes, helper_modules, file).map_err(|err| match err {
            IngestError::Unsupported { file, message } => IngestError::Unsupported {
                message: format!("test class `{}` refused: {message}", scoped_class_name(class)),
                file,
            },
            other => other,
        });
        if let Some(tm) = super::survey::unwrap_or_record(result)? {
            out.push(tm);
        }
    }
    Ok(out)
}

/// `Outer::Inner` for a class at `scope`, as written.
fn scoped_class_name((scope, class): &ScopedClass<'_>) -> String {
    let mut path = scope.clone();
    path.extend(class_name_path(class).unwrap_or_default());
    path.join("::")
}

/// `class` and, after it, every test class its body declares (at any
/// depth of test-class nesting), each scoped by the classes around it.
fn push_with_nested_test_classes<'pr>(class: ScopedClass<'pr>, out: &mut Vec<ScopedClass<'pr>>) {
    let (scope, node) = class;
    let mut inner_scope = scope.clone();
    inner_scope.extend(class_name_path(&node).unwrap_or_default());
    let nested: Vec<ruby_prism::ClassNode<'pr>> = node
        .body()
        .map(flatten_statements)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|stmt| stmt.as_class_node())
        .filter(is_nested_test_class_node)
        .collect();
    out.push((scope, node));
    for inner in nested {
        push_with_nested_test_classes((inner_scope.clone(), inner), out);
    }
}

/// A class in a test class's body that is a test class of its own: it
/// has a superclass and is test-shaped. `class Validatable` and
/// `class FakeController < ActionController::Base` stay helpers.
fn is_nested_test_class_node(c: &ruby_prism::ClassNode<'_>) -> bool {
    c.superclass().is_some() && is_test_class_node(c)
}

/// Refuse, by name, every class in the file that declares tests but is
/// not one of the test classes collected (`reached`): one inside an `if`,
/// a block or a method body, one nested in a class that has content of
/// its own, one that is not test-shaped. Minitest would run it (or fail
/// to load it); nothing here would, and nothing would say so. The
/// message says which construct keeps it out, with the class's name and
/// line in parentheses, so the survey buckets refusals by reason.
fn refuse_unreached_test_classes(root: &Node<'_>, reached: &[ScopedClass<'_>], file: &str) -> IngestResult<()> {
    use ruby_prism::Visit;
    /// What encloses a class, outermost first: a namespace the class
    /// walk goes through (`transparent`), a class it stops at, or another
    /// construct it never enters.
    #[derive(Clone)]
    enum Frame {
        Module,
        Class { name: String, test: bool, transparent: bool, namespace_only: bool },
        Construct(&'static str),
    }
    struct Classes {
        found: Vec<(usize, Option<u32>, String, (&'static str, Option<String>))>,
        scope: Vec<String>,
        frames: Vec<Frame>,
    }
    impl Classes {
        fn within(&mut self, frame: Frame, visit: impl FnOnce(&mut Self)) {
            self.frames.push(frame);
            visit(self);
            self.frames.pop();
        }
        /// Why a class that declares tests, at the current frames, is not
        /// one the walk reaches, and the class that keeps it out, if one
        /// does.
        fn reason(&self, class: &ruby_prism::ClassNode<'_>) -> (&'static str, Option<String>) {
            let blocker = self.frames.iter().enumerate().find(|(_, f)| match f {
                Frame::Module => false,
                Frame::Class { transparent, .. } => !transparent,
                Frame::Construct(_) => true,
            });
            match blocker {
                Some((_, Frame::Construct(what))) => (*what, None),
                _ if !is_test_class_node(class) => {
                    ("it is not test-shaped: it has no superclass and its name does not end in `Test`", None)
                }
                Some((at, Frame::Class { name, test: true, .. })) => {
                    let why = if at + 1 == self.frames.len() {
                        "it is nested in a test class without a superclass of its own, so it is not a test case"
                    } else {
                        "it is nested in a module or class inside a test class, which is not modeled"
                    };
                    (why, Some(name.clone()))
                }
                Some((_, Frame::Class { name, namespace_only: false, .. })) => (
                    "it is nested in a class with statements of its own besides class and module declarations, \
                     and reopening such a class around a test class is not modeled",
                    Some(name.clone()),
                ),
                Some((_, Frame::Class { name, .. })) => {
                    ("it is nested in a class that is not modeled as a test or a namespace", Some(name.clone()))
                }
                Some((_, Frame::Module)) | None => ("it is not reached by the test-class walk", None),
            }
        }
    }
    impl<'pr> Visit<'pr> for Classes {
        fn visit_class_node(&mut self, node: &ruby_prism::ClassNode<'pr>) {
            let path = class_name_path(node).unwrap_or_else(|| vec!["?".into()]);
            let mut qualified = self.scope.clone();
            qualified.extend(path.iter().cloned());
            if declares_tests(node) {
                let line = super::sources::line_at_parse(&node.location());
                self.found.push((node.location().start_offset(), line, qualified.join("::"), self.reason(node)));
            }
            let frame = Frame::Class {
                name: qualified.join("::"),
                test: is_test_class_node(node),
                transparent: is_test_namespace(node),
                namespace_only: is_namespace_only(node),
            };
            let depth = self.scope.len();
            self.scope.extend(path);
            self.within(frame, |v| ruby_prism::visit_class_node(v, node));
            self.scope.truncate(depth);
        }
        fn visit_module_node(&mut self, node: &ruby_prism::ModuleNode<'pr>) {
            let depth = self.scope.len();
            self.scope.extend(module_name_path(node).unwrap_or_else(|| vec!["?".into()]));
            self.within(Frame::Module, |v| ruby_prism::visit_module_node(v, node));
            self.scope.truncate(depth);
        }
        fn visit_def_node(&mut self, node: &ruby_prism::DefNode<'pr>) {
            self.within(Frame::Construct("it is declared inside a method body, which is not modeled"), |v| ruby_prism::visit_def_node(v, node));
        }
        fn visit_block_node(&mut self, node: &ruby_prism::BlockNode<'pr>) {
            self.within(Frame::Construct("it is declared inside a block, which is not modeled"), |v| ruby_prism::visit_block_node(v, node));
        }
        fn visit_lambda_node(&mut self, node: &ruby_prism::LambdaNode<'pr>) {
            self.within(Frame::Construct("it is declared inside a lambda, which is not modeled"), |v| ruby_prism::visit_lambda_node(v, node));
        }
        fn visit_if_node(&mut self, node: &ruby_prism::IfNode<'pr>) {
            self.within(Frame::Construct("it is declared inside a conditional, which is not modeled"), |v| ruby_prism::visit_if_node(v, node));
        }
        fn visit_unless_node(&mut self, node: &ruby_prism::UnlessNode<'pr>) {
            self.within(Frame::Construct("it is declared inside a conditional, which is not modeled"), |v| ruby_prism::visit_unless_node(v, node));
        }
        fn visit_case_node(&mut self, node: &ruby_prism::CaseNode<'pr>) {
            self.within(Frame::Construct("it is declared inside a conditional, which is not modeled"), |v| ruby_prism::visit_case_node(v, node));
        }
        fn visit_case_match_node(&mut self, node: &ruby_prism::CaseMatchNode<'pr>) {
            self.within(Frame::Construct("it is declared inside a conditional, which is not modeled"), |v| ruby_prism::visit_case_match_node(v, node));
        }
        fn visit_while_node(&mut self, node: &ruby_prism::WhileNode<'pr>) {
            self.within(Frame::Construct("it is declared inside a loop, which is not modeled"), |v| ruby_prism::visit_while_node(v, node));
        }
        fn visit_until_node(&mut self, node: &ruby_prism::UntilNode<'pr>) {
            self.within(Frame::Construct("it is declared inside a loop, which is not modeled"), |v| ruby_prism::visit_until_node(v, node));
        }
        fn visit_for_node(&mut self, node: &ruby_prism::ForNode<'pr>) {
            self.within(Frame::Construct("it is declared inside a loop, which is not modeled"), |v| ruby_prism::visit_for_node(v, node));
        }
        fn visit_begin_node(&mut self, node: &ruby_prism::BeginNode<'pr>) {
            self.within(Frame::Construct("it is declared inside a `begin` block, which is not modeled"), |v| ruby_prism::visit_begin_node(v, node));
        }
        fn visit_singleton_class_node(&mut self, node: &ruby_prism::SingletonClassNode<'pr>) {
            self.within(Frame::Construct("it is declared inside a `class << …` body, which is not modeled"), |v| ruby_prism::visit_singleton_class_node(v, node));
        }
    }
    let mut classes = Classes { found: Vec::new(), scope: Vec::new(), frames: Vec::new() };
    classes.visit(root);
    let reached: crate::det::HashSet<usize> =
        reached.iter().map(|(_, c)| c.location().start_offset()).collect();
    for (offset, line, name, (reason, outer)) in classes.found {
        if reached.contains(&offset) {
            continue;
        }
        let outer = outer.map(|o| format!(" in `{o}`")).unwrap_or_default();
        let line = line
            .or_else(|| super::sources::line_at(file, offset))
            .map(|l| format!(", line {l}"))
            .unwrap_or_default();
        super::survey::unwrap_or_record::<()>(Err(IngestError::Unsupported {
            file: file.into(),
            message: format!("test class `{name}` refused: {reason}; its tests would not run (`{name}`{outer}{line})"),
        }))?;
    }
    Ok(())
}

/// The fallback's candidate (`ingest_test_files`): a top-level class
/// that is not test-shaped is still taken as the file's test when it
/// declares tests, or inherits from a class named like one
/// (`class Variant < ProductBaseTest`), which may give it its tests. A
/// class that does neither is a support class: Core's
/// test/support/test_access_blocker.rb declares only
/// `class TestAccessBlocker < Module`, which tests include as a module,
/// and taking it for a test kept it out of the library tree.
fn may_be_test_class(c: &ruby_prism::ClassNode<'_>) -> bool {
    if declares_tests(c) {
        return true;
    }
    c.superclass()
        .and_then(|parent| constant_path_of(&parent))
        .and_then(|path| path.last().cloned())
        .is_some_and(|last| last.ends_with("Test") || last.ends_with("TestCase") || last.ends_with("Spec"))
}

/// The class body declares at least one test: `test "…"` with arguments
/// or a `def test_…`, as a statement of its own or inside one that runs
/// as the body loads (a `tag(…) do … end` block, a loop, a conditional;
/// `hidden_test_declaration`). A class whose tests are all in a `tag`
/// block declares tests as much as one that writes them bare.
fn declares_tests(c: &ruby_prism::ClassNode<'_>) -> bool {
    c.body()
        .map(flatten_statements)
        .unwrap_or_default()
        .iter()
        .any(|stmt| hidden_test_declaration(stmt).is_some())
}

/// `test "…" …` (a receiverless `test` call with arguments) or a
/// receiverless `def test_…`, whether or not ingest models its form.
pub(super) fn is_test_declaration_shape(stmt: &Node<'_>) -> bool {
    if let Some(def) = stmt.as_def_node() {
        return def.receiver().is_none() && def.name().as_slice().starts_with(b"test_");
    }
    stmt.as_call_node().is_some_and(|call| {
        call.receiver().is_none() && call.name().as_slice() == b"test" && call.arguments().is_some()
    })
}

/// Core's `TestGenerator#generate_test(name, location = nil, tags: nil,
/// &block)` (components/platform/essentials/lib/platform/testing/
/// test_generator.rb): it declares the test with `test(name, tags,
/// &block)`, then redefines it to call the original as `real_<name>`.
pub(super) fn is_generate_test(call: &ruby_prism::CallNode<'_>) -> bool {
    call.receiver().is_none() && call.name().as_slice() == b"generate_test" && call.arguments().is_some()
}

/// `define_method("test_…")` (`:test_…`, `"test_#{x}"`, a name made as
/// it runs) on the class itself: a method Minitest runs when its name
/// starts with `test_`.
pub(super) fn defines_test_method(call: &ruby_prism::CallNode<'_>) -> bool {
    if call.name().as_slice() != b"define_method" || call.receiver().is_some_and(|r| r.as_self_node().is_none()) {
        return false;
    }
    let Some(first) = call.arguments().and_then(|a| a.arguments().iter().next()) else { return false };
    let named = |text: &[u8]| text.starts_with(b"test_") || b"test_".starts_with(text);
    if let Some(sym) = first.as_symbol_node() {
        return sym.unescaped().starts_with(b"test_");
    }
    if let Some(string) = first.as_string_node() {
        return string.unescaped().starts_with(b"test_");
    }
    let parts = first
        .as_interpolated_string_node()
        .map(|n| n.parts())
        .or_else(|| first.as_interpolated_symbol_node().map(|n| n.parts()));
    match parts {
        Some(parts) => parts.iter().next().is_some_and(|p| p.as_string_node().is_none_or(|s| named(s.unescaped()))),
        None => true,
    }
}

/// The first test declaration (`test "…"`, `def test_…`, `generate_test`,
/// `define_method("test_…")`) inside `stmt`,
/// a class-body statement ingest does not model (`%w[a b].each do |x|
/// test "#{x}" do … end end`, or a `test` whose name is not a string
/// literal), as its offset. Nested classes and method bodies are not
/// searched: a class is its own suite, and a method's body runs when
/// called, not at class load. In a `class << self` body a `def test_…`
/// is a class method, which Minitest does not run, so its name is no
/// test; a `test` there, or in the body of a method there (a macro that
/// declares tests where it is called), is (`test_unroll::declare_hidden`
/// names either).
pub(super) fn hidden_test_declaration(stmt: &Node<'_>) -> Option<usize> {
    use ruby_prism::Visit;
    struct Find {
        found: Option<usize>,
        singleton: usize,
    }
    impl<'pr> Visit<'pr> for Find {
        fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
            if self.found.is_none() && (is_test_declaration_shape(&node.as_node()) || is_generate_test(node) || defines_test_method(node)) {
                self.found = Some(node.location().start_offset());
                return;
            }
            ruby_prism::visit_call_node(self, node);
        }
        fn visit_def_node(&mut self, node: &ruby_prism::DefNode<'pr>) {
            if self.singleton > 0 {
                ruby_prism::visit_def_node(self, node);
                return;
            }
            if self.found.is_none() && is_test_declaration_shape(&node.as_node()) {
                self.found = Some(node.location().start_offset());
            }
        }
        fn visit_class_node(&mut self, _: &ruby_prism::ClassNode<'pr>) {}
        fn visit_module_node(&mut self, _: &ruby_prism::ModuleNode<'pr>) {}
        fn visit_singleton_class_node(&mut self, node: &ruby_prism::SingletonClassNode<'pr>) {
            self.singleton += 1;
            ruby_prism::visit_singleton_class_node(self, node);
            self.singleton -= 1;
        }
    }
    let mut find = Find { found: None, singleton: 0 };
    find.visit(stmt);
    find.found
}

/// A helper class or module of a test file (one beside the test class,
/// or in its body) with every class and module it nests, each under its
/// qualified name (`PriceRule::DiscountCode`), outer before inner. The
/// nested ones, and modules in a test class body, used to be dropped
/// without a word. A nested class that declares tests never gets here:
/// the file is refused by name (`refuse_unreached_test_classes`).
fn carried_classes(
    declaration: &ruby_prism::Node<'_>,
    scope: &[String],
    file: &str,
) -> IngestResult<Vec<crate::dialect::LibraryClass>> {
    declared_classes_under(declaration, file, scope)
}

/// `def self.use_transactional_tests; <value>; end` on the test class.
fn use_transactional_tests_reader(class: &ClassId, value: bool) -> MethodDef {
    MethodDef {
        name_span: Span::synthetic(),
        name: Symbol::from("use_transactional_tests"),
        receiver: crate::dialect::MethodReceiver::Class,
        visibility: crate::dialect::MethodVisibility::Public,
        params: Vec::new(),
        unsupported_formals: None,
        has_anonymous_block: false,
        body: Expr::new(Span::synthetic(), ExprNode::Lit { value: crate::expr::Literal::Bool { value } }),
        signature: Some(crate::lower::typing::fn_sig(vec![], crate::ty::Ty::Bool)),
        effects: crate::effect::EffectSet::default(),
        enclosing_class: Some(class.0.clone()),
        kind: crate::dialect::AccessorKind::Method,
        is_async: false,
        mutates_self: false,
        block_param: None,
    }
}

pub(super) fn ingest_test_class(
    (scope, class): &ScopedClass<'_>,
    top_level_helper_nodes: &[ScopedClass<'_>],
    top_level_helper_modules: &[ScopedModule<'_>],
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
    // TestTags state: this class's own (a subclass starts empty), and
    // the tags each enclosing `tag … do` block restores.
    let mut test_tags = TestTags::default();
    let mut saved_tags: Vec<Vec<(String, String)>> = Vec::new();
    // Every test method the body defines as it loads, under the name it
    // defines (before the persistent rename), and whether ActiveSupport's
    // `test` declared it: a subclass's `test` of one of these raises
    // (`refuse_inherited_test_redefinitions`).
    let mut defined_tests: Vec<(String, bool)> = Vec::new();
    // The class body's other statements, run as written in source order
    // (`carried_body_statement`), and where each stands: below a
    // definition, below a `setup do` or `teardown do` block.
    let mut body_statements: Vec<crate::dialect::TestBodyStatement> = Vec::new();
    let mut tests_macro: Option<crate::dialect::TestBodyStatement> = None;
    let mut after_definitions = false;
    let mut after_setup_blocks = false;
    let mut after_teardown_blocks = false;
    let mut setup_closes_over_class_locals = false;
    // A statement declares tests ingest cannot list: they are the loaded
    // class's (`TestModule::tests_at_load`).
    let mut tests_at_load = false;
    let mut covered_singletons: Vec<Node<'_>> = Vec::new();
    if let Some(class_body) = class.body() {
        let stmts = flatten_statements(class_body);
        let consts = TagConstants::of(&stmts);
        let macros = test_macros::Macros::of_class(class, &stmts, &name, file);
        let mut items = Vec::new();
        class_body_items(stmts, file, &consts, &mut items)?;
        for item in items {
            let stmt = match item {
                BodyItem::Stmt(stmt) => stmt,
                BodyItem::TagsBegin(tags) => {
                    saved_tags.push(test_tags.tags.clone());
                    merge_tags(&mut test_tags.tags, &tags);
                    continue;
                }
                BodyItem::TagsEnd => {
                    test_tags.tags = saved_tags.pop().unwrap_or_default();
                    continue;
                }
            };
            // A blockless `tag(...)`: the tags stay for the rest of the
            // class body, and the hook renames untagged tests at the end.
            if let Some((tags, None)) = tag_declaration(&stmt, file, &consts)? {
                merge_tags(&mut test_tags.tags, &tags);
                test_tags.persistent = true;
                continue;
            }
            if let Some(test) = ingest_test_declaration(&stmt, file, &test_tags, &consts)? {
                let is_def = stmt.as_def_node().is_some();
                declare_test(&mut tests, &mut defined_tests, test, is_def, &name, file, &stmt.location())?;
                after_definitions = true;
                continue;
            }
            if let Some(body) = ingest_teardown_block(&stmt, file)? {
                teardowns.push(body);
                after_teardown_blocks = true;
                continue;
            }
            if let Some(body) = ingest_teardown_block(&stmt, file)? {
                teardowns.push(body);
                continue;
            }
            if let Some(body) = ingest_setup_declaration(&stmt, file)? {
                // A method's super/return belongs to setup, not to whichever
                // test happens to run next. Keep it distinct from callbacks.
                if let Some(def) = stmt.as_def_node() {
                    let method = ingest_library_method(&def, &name, file)?;
                    helpers.retain(|m| m.name.as_str() != "setup");
                    helpers.push(method);
                    after_definitions = true;
                    continue;
                }
                after_setup_blocks = true;
                if let Some(block) = stmt.as_call_node().and_then(|c| c.block()).and_then(|b| b.as_block_node()) {
                    setup_closes_over_class_locals |= test_unroll::block_uses_outer_local(&block);
                }
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
            // ...; end` — or module. Captured per-test-module; the
            // Ruby emit writes it nested in the test class, ahead of
            // the test class body.
            //
            // Each is the namespaced constant it is in Ruby: inside
            // `ShopAnyEnabledTest`, `class TestClass` is
            // `ShopAnyEnabledTest::TestClass`, and its `name` says so
            // (Core builds cache keys from it). In a test-support class
            // (`MockGCSInterface`) a second support file's
            // `MockS3Interface::Bucket` is a different class, not a
            // reopen of a shared `Bucket`. What the class or module
            // nests comes with it (`carried_classes`).
            if let Some(inner) = stmt.as_class_node() {
                // A nested test class is a module of its own
                // (`push_with_nested_test_classes`).
                after_definitions = true;
                if is_nested_test_class_node(&inner) {
                    continue;
                }
                inner_classes.extend(carried_classes(&inner.as_node(), &name_path, file)?);
                continue;
            }
            if stmt.as_module_node().is_some() {
                after_definitions = true;
                inner_classes.extend(carried_classes(&stmt, &name_path, file)?);
                continue;
            }
            // Other non-test `def` — instance helper method
            // (e.g. `setup_adapter_with_stub_row(id)`). Capture as
            // an ordinary method on the test class so test bodies
            // can `self.<helper>(...)` it. Definitions of `def
            // setup` and `def test_*` already short-circuited above.
            if let Some(def) = stmt.as_def_node() {
                // A `def self.m` test macro used other than by a class-body
                // call statement (`send(:m, …)`) declares tests as that
                // statement, carried, runs.
                if macros.def_other_use(&def).is_some() {
                    tests_at_load = true;
                }
                let m = ingest_library_method(&def, &name, file)?;
                helpers.push(m);
                after_definitions = true;
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
                                tests_macro = carried_body_statement(&stmt, &name, file)?.map(|expr| crate::dialect::TestBodyStatement {
                                    expr,
                                    line: super::sources::line_at(file, stmt.location().start_offset()).unwrap_or(0),
                                    after_definitions,
                                    after_setup_blocks,
                                    after_teardown_blocks,
                                    source: None,
                                });
                            }
                        }
                    }
                    continue;
                }
                // `self.use_transactional_tests = false` (ActiveRecord::
                // TestFixtures' class_attribute): the class reader the
                // runtime's transactional fixtures consult, as a class
                // method answering the literal. Inherited by subclasses,
                // as the class_attribute is. Only a literal is modeled.
                if call.receiver().is_some_and(|r| r.as_self_node().is_some())
                    && constant_id_str(&call.name()) == "use_transactional_tests="
                {
                    let value = call.arguments().and_then(|args| {
                        let list: Vec<_> = args.arguments().iter().collect();
                        match list.as_slice() {
                            [arg] if arg.as_true_node().is_some() => Some(true),
                            [arg] if arg.as_false_node().is_some() => Some(false),
                            _ => None,
                        }
                    });
                    let Some(value) = value else {
                        return Err(IngestError::Unsupported {
                            file: file.into(),
                            message: "`self.use_transactional_tests =` with a value other than a true/false literal is not modeled".into(),
                        });
                    };
                    helpers.retain(|m| !(m.name.as_str() == "use_transactional_tests"
                        && matches!(m.receiver, crate::dialect::MethodReceiver::Class)));
                    helpers.push(use_transactional_tests_reader(&name, value));
                    continue;
                }
                if call.receiver().is_none()
                    && constant_id_str(&call.name()) == "include"
                {
                    // Tests its modules' `included` hooks declare.
                    macros.mixin_gaps(&stmt)?;
                    // A hook of a module it mixes in declares tests as it runs.
                    if macros.take_gap().is_some() {
                        tests_at_load = true;
                    }
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
            // `self.use_transactional_tests = false` — Rails' per-class
            // opt-out of the fixture transaction. Kept as a class method
            // so the autorun shim can ask the class. Anything else in
            // the class body is still skipped.
            if let Some(method) = transactional_tests_method(&stmt) {
                helpers.push(method);
                continue;
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
                // A sorbet type alias is the type object the check reads
                // (`sorbet_check::alias_value`), or nothing.
                if super::library_class::is_sorbet_type_alias(&cw.value()) {
                    if let (Some(const_name), Some(value)) = (const_name, super::sorbet_check::alias_value(&cw.value(), file)?) {
                        constants.push((const_name, value));
                    }
                    continue;
                }
                // `CASES = [...].each do |c| test … end`: the loop's tests,
                // and the constant its receiver (`each` returns it).
                if let Some(offset) = hidden_test_declaration(&cw.value()) {
                    match test_unroll::declare_hidden(&cw.value(), offset, class, &name, file, &test_tags, &consts)? {
                        test_unroll::Hidden::Tests(declared) => {
                            if let (Some(const_name), Some(receiver)) = (const_name, test_unroll::loop_receiver(&cw.value())) {
                                constants.push((const_name, ingest_expr(&receiver, file)?));
                            }
                            for test in declared {
                                declare_test(&mut tests, &mut defined_tests, test, false, &name, file, &stmt.location())?;
                            }
                        }
                        // The constant is written, and its tests declared,
                        // as the class loads.
                        test_unroll::Hidden::AtLoad(gap) => {
                            let at = BodyAt { after_definitions, after_setup_blocks, after_teardown_blocks };
                            carry_at_load(&stmt, gap, &name, file, &test_tags, at, &mut body_statements, &mut tests_at_load)?;
                        }
                    }
                    continue;
                }
                if let Some(const_name) = const_name {
                    let value = ingest_expr(&cw.value(), file)?;
                    constants.push((const_name, value));
                }
                continue;
            }
            // Any other class-body statement is not modeled. One that
            // declares tests (`%w[a b].each { |x| test "#{x}" do … end }`,
            // a `test` named by a non-literal) would lose them silently.
            // A loop over a literal is unrolled; anything else is refused
            // as that statement alone, so the class's other tests still
            // run (strict ingest fails on it).
            //
            // A call of a test macro is expanded (`test_macros`); the
            // `class << self` defining macros the body only calls that way
            // declares nothing itself.
            let expanded = macros.expand(&stmt, &test_tags, &consts)?;
            let gap = macros.take_gap();
            // A call not expanded declares its tests as it runs: carried. A
            // hook of a module the statement mixes in does too (the mixin
            // replays); a statement that also declares tests itself is left
            // to `declare_hidden` below, which carries it.
            if let Some(test_macros::Gap::Mixin) = gap {
                tests_at_load = true;
            }
            if let Some(declared) = expanded {
                for test in declared {
                    declare_test(&mut tests, &mut defined_tests, test, false, &name, file, &stmt.location())?;
                }
                match gap {
                    Some(test_macros::Gap::Call(gap)) => {
                        let at = BodyAt { after_definitions, after_setup_blocks, after_teardown_blocks };
                        carry_at_load(&stmt, gap, &name, file, &test_tags, at, &mut body_statements, &mut tests_at_load)?;
                    }
                    Some(test_macros::Gap::Named(gap)) => super::survey::continue_or_fail(gap)?,
                    _ => {}
                }
                after_definitions = true;
                continue;
            }
            // Its other methods are the class's (`def self.m`).
            if let Some(defs) = macros.covers_singleton(&stmt) {
                for def in defs {
                    let mut m = ingest_library_method(&def, &name, file)?;
                    m.receiver = crate::dialect::MethodReceiver::Class;
                    helpers.push(m);
                }
                after_definitions = true;
                // Its macros are not emitted (every call is expanded), unless
                // a call declares its tests as the class loads.
                covered_singletons.push(stmt);
                continue;
            }
            if let Some(offset) = hidden_test_declaration(&stmt) {
                match test_unroll::declare_hidden(&stmt, offset, class, &name, file, &test_tags, &consts)? {
                    test_unroll::Hidden::Tests(declared) => {
                        for test in declared {
                            declare_test(&mut tests, &mut defined_tests, test, false, &name, file, &stmt.location())?;
                        }
                    }
                    test_unroll::Hidden::AtLoad(gap) => {
                        let at = BodyAt { after_definitions, after_setup_blocks, after_teardown_blocks };
                        carry_at_load(&stmt, gap, &name, file, &test_tags, at, &mut body_statements, &mut tests_at_load)?;
                    }
                }
                after_definitions = true;
                continue;
            }
            // `class << self` of nothing but methods: the class's.
            if let Some(defs) = singleton_methods(&stmt) {
                for def in defs {
                    let mut m = ingest_library_method(&def, &name, file)?;
                    m.receiver = crate::dialect::MethodReceiver::Class;
                    helpers.push(m);
                }
                after_definitions = true;
                continue;
            }
            if let Some(expr) = carried_body_statement(&stmt, &name, file)? {
                body_statements.push(crate::dialect::TestBodyStatement {
                    expr,
                    line: super::sources::line_at(file, stmt.location().start_offset()).unwrap_or(0),
                    after_definitions,
                    after_setup_blocks,
                    after_teardown_blocks,
                    source: None,
                });
            }
        }
    }

    // A macro the class body expands is not defined in the emitted class,
    // but a call that declares its tests as the class loads runs it: the
    // `class << self` it is in is carried too, above the calls.
    if tests_at_load {
        for stmt in &covered_singletons {
            if let Some(expr) = carried_body_statement(stmt, &name, file)? {
                body_statements.push(crate::dialect::TestBodyStatement {
                    expr,
                    line: super::sources::line_at(file, stmt.location().start_offset()).unwrap_or(0),
                    after_definitions: false,
                    after_setup_blocks: false,
                    after_teardown_blocks: false,
                    source: None,
                });
            }
        }
        body_statements.sort_by_key(|carried| carried.line);
    }

    // TestTags' persistent hook (`_process_persistent_tags`, run before
    // Minitest lists the class's methods): every test method the class
    // defines whose name carries no tag yet gets the class's tags as they
    // stand at the end of the body, by `alias_method` and `undef_method`.
    // An alias onto a name the body already defined (`test "x"`, then
    // `tag a: 1`, then `test "x"` again, defined as `test_x_tag:a:1`)
    // replaces that method: one test of the name is left, with the
    // renamed one's (the earlier) body.
    if test_tags.persistent && !test_tags.tags.is_empty() {
        let renamed: Vec<bool> = tests.iter().map(|t| !t.method_name.contains("tag:")).collect();
        for (test, renamed) in tests.iter_mut().zip(&renamed) {
            if *renamed {
                test.method_name = name_with_tags(&test.method_name, &test_tags.tags);
            }
        }
        let aliased: crate::det::HashSet<String> = tests
            .iter()
            .zip(&renamed)
            .filter(|(_, renamed)| **renamed)
            .map(|(t, _)| t.method_name.clone())
            .collect();
        let mut at = 0;
        tests.retain(|t| {
            let keep = renamed[at] || !aliased.contains(&t.method_name);
            at += 1;
            keep
        });
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
        let own_def = own.is_some();
        let name_span = match own.map(|i| helpers.remove(i)) {
            Some(m) => {
                exprs.push(m.body);
                m.name_span
            }
            None => Span::synthetic(),
        };
        exprs.extend(teardowns.into_iter().rev());
        let blocks = Expr::new(Span::synthetic(), ExprNode::Seq { exprs });
        // With no `def teardown` of its own the method must still reach
        // TestBase's teardown, which runs `mocha_verify` / the stub-slot
        // verifies: `begin; blocks; ensure; super(); end`, so `super`
        // runs even when a block raises. A `def teardown` is the class's
        // to chain (`analyze::test_teardown` requires it to end in `super`).
        let body = if own_def {
            blocks
        } else {
            let syn = Span::synthetic();
            Expr::new(
                syn,
                ExprNode::BeginRescue {
                    body: blocks,
                    rescues: Vec::new(),
                    else_branch: None,
                    ensure: Some(Expr::new(syn, ExprNode::Super { args: Some(Vec::new()), block: None })),
                    implicit: true,
                },
            )
        };
        helpers.push(MethodDef {
            name_span,
            name: Symbol::from("teardown"),
            receiver: crate::dialect::MethodReceiver::Instance,
            visibility: crate::dialect::MethodVisibility::Public,
            params: Vec::new(),
            unsupported_formals: None,
            has_anonymous_block: false,
            body,
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
        inner_classes.extend(carried_classes(&helper_node.as_node(), helper_scope, file)?);
    }
    // So do the file's helper modules (`helper_module`): Core's
    // `module Admin; module OrderAssessmentLockTestHelper; def …` beside
    // `class OrderRisksControllerTest`, which includes it.
    for (helper_scope, helper_module) in top_level_helper_modules {
        inner_classes.extend(carried_classes(&helper_module.as_node(), helper_scope, file)?);
    }

    Ok(TestModule {
        name,
        parent,
        parent_span,
        target,
        tests,
        setup,
        setup_closes_over_class_locals,
        inner_classes,
        helpers,
        teardown_blocks,
        teardown_def,
        constants,
        includes,
        source_file: None,
        defined_tests,
        body_statements,
        file_requires: Vec::new(),
        tests_at_load,
        tests_macro,
    })
}

/// The methods of a `class << self` that holds nothing else.
fn singleton_methods<'pr>(stmt: &Node<'pr>) -> Option<Vec<ruby_prism::DefNode<'pr>>> {
    let singleton = stmt.as_singleton_class_node()?;
    singleton.expression().as_self_node()?;
    let items = singleton.body().map(flatten_statements).unwrap_or_default();
    items.iter().map(|item| item.as_def_node()).collect()
}

/// Where a carried class-body statement stands among the body's
/// definitions and callback blocks (`TestBodyStatement`).
#[derive(Clone, Copy)]
struct BodyAt {
    after_definitions: bool,
    after_setup_blocks: bool,
    after_teardown_blocks: bool,
}

/// `stmt` declares tests that cannot be listed before it runs (`gap` is
/// the refusal that would name them): a loop over a collection computed
/// as the class loads, a call of a class method that declares tests, a
/// conditional `test`. Ruby runs the statement as the body loads, and
/// ActiveSupport's `test` (or a macro's) defines each method then, so
/// the emitted class carries it as written (`carried_body_statement`)
/// and the driver lists the methods the loaded class has
/// (`TestModule::tests_at_load`). A statement under TestTags' `tag` is
/// named instead: the tags are applied by ingest here, and the carried
/// statement would declare the tests untagged, under other names.
fn carry_at_load(
    stmt: &Node<'_>,
    gap: IngestError,
    class: &ClassId,
    file: &str,
    tags: &TestTags,
    at: BodyAt,
    body_statements: &mut Vec<crate::dialect::TestBodyStatement>,
    tests_at_load: &mut bool,
) -> IngestResult<()> {
    if !tags.tags.is_empty() || tags.persistent {
        return super::survey::continue_or_fail(gap);
    }
    if let Some(expr) = carried_body_statement(stmt, class, file)? {
        body_statements.push(crate::dialect::TestBodyStatement {
            expr,
            line: super::sources::line_at(file, stmt.location().start_offset()).unwrap_or(0),
            after_definitions: at.after_definitions,
            after_setup_blocks: at.after_setup_blocks,
            after_teardown_blocks: at.after_teardown_blocks,
            source: None,
        });
        *tests_at_load = true;
    }
    Ok(())
}

/// A test class-body statement no case above models, which Ruby runs in
/// the class as the body loads: a class macro (`use_gcs`, whose
/// `setup(:setup_gcs, prepend: true)` registers the callback the
/// runtime's `RoundhouseTestCallbacks` runs), `let(:x) { … }`, a class
/// attribute write, `extend`. It runs as written in the emitted class
/// (`TestModule::body_statements`). One the expression ingest does not
/// model is named, not dropped.
///
/// A bare `private`/`protected`/`public`/`module_function` is not one:
/// it sets the visibility of the `def`s below it in the source, which
/// the emitted class defines elsewhere.
pub(super) fn carried_body_statement(stmt: &Node<'_>, class: &ClassId, file: &str) -> IngestResult<Option<Expr>> {
    if let Some(call) = stmt.as_call_node() {
        if call.receiver().is_none()
            && call.arguments().is_none()
            && call.block().is_none()
            && matches!(constant_id_str(&call.name()), "private" | "protected" | "public" | "module_function")
        {
            return Ok(None);
        }
    }
    match ingest_expr(stmt, file) {
        Ok(expr) => Ok(Some(expr)),
        Err(err) => {
            let why = match &err {
                IngestError::Unsupported { message, .. } => message.clone(),
                other => other.to_string(),
            };
            super::survey::continue_or_fail(IngestError::Unsupported {
                file: file.into(),
                message: format!(
                    "test class-body statement `{}` of `{}`{} is not carried; it will not run: {why}",
                    source_excerpt(stmt),
                    class.0,
                    line_suffix(file, &stmt.location())
                ),
            })?;
            Ok(None)
        }
    }
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

/// `self.use_transactional_tests = false` (or `true`) as
/// `def self.use_transactional_tests; <bool>; end`. The shim calls that
/// class method; `TestBase` answers true when the class does not override it.
fn transactional_tests_method(stmt: &Node) -> Option<MethodDef> {
    let call = stmt.as_call_node()?;
    if constant_id_str(&call.name()) != "use_transactional_tests=" {
        return None;
    }
    let recv = call.receiver()?;
    if recv.as_self_node().is_none() {
        return None;
    }
    let args = call.arguments()?;
    let arg = args.arguments().iter().next()?;
    let value = if arg.as_true_node().is_some() {
        true
    } else if arg.as_false_node().is_some() {
        false
    } else {
        return None;
    };
    let body = Expr::new(
        Span::synthetic(),
        ExprNode::Lit {
            value: Literal::Bool { value },
        },
    );
    Some(MethodDef {
        name: Symbol::from("use_transactional_tests"),
        receiver: MethodReceiver::Class,
        visibility: MethodVisibility::Public,
        params: Vec::new(),
        unsupported_formals: None,
        has_anonymous_block: false,
        block_param: None,
        name_span: Span::synthetic(),
        body,
        signature: Some(Ty::Fn {
            params: Vec::new().into(),
            block: None,
            ret: std::sync::Arc::new(Ty::Bool),
            effects: EffectSet::pure(),
        }),
        effects: EffectSet::pure(),
        enclosing_class: None,
        kind: AccessorKind::Method,
        is_async: false,
        mutates_self: false,
    })
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
        let mut setup: Option<Expr> = None;
        let mut after_fixtures: Vec<Expr> = Vec::new();
        for stmt in flatten_statements(body) {
            if setup.is_none() {
                if let Some(found) = ingest_setup_declaration(&stmt, file)? {
                    setup = Some(found);
                    continue;
                }
            }
            if let Some(stmts) = ingest_load_fixtures_override(&stmt, file)? {
                after_fixtures = stmts;
            }
        }
        if after_fixtures.is_empty() {
            return Ok(setup);
        }
        // The fixture hook's statements run first: Rails loads fixtures
        // in `before_setup`, ahead of every `setup` block, and the
        // harness reloads them per test before this runs.
        let mut exprs = after_fixtures;
        match setup {
            Some(s) => match *s.node {
                ExprNode::Seq { exprs: own } => exprs.extend(own),
                node => exprs.push(Expr::new(s.span, node)),
            },
            None => {}
        }
        return Ok(Some(Expr::new(Span::synthetic(), ExprNode::Seq { exprs })));
    }
    Ok(None)
}

/// `def load_fixtures(config) = (fixtures = super; …; fixtures)` on the
/// app's `ActiveSupport::TestCase` — work to do once fixtures are in
/// the database. campfire repairs `rooms.messages_count` there, because
/// `fixtures :all` inserts messages before the rooms they count:
///
/// ```ruby
/// def load_fixtures(config)
///   fixtures = super
///   Room::MessagesCount.ensure!
///   Room::MessagesCount.backfill!
///   fixtures
/// end
/// ```
///
/// Only that shape — `super` first (bare or assigned), its value last
/// when assigned — is read, as the statements between. The harness
/// loads fixtures itself, so those statements are what the override
/// adds. Any other shape is an error rather than a hook silently left
/// out.
fn ingest_load_fixtures_override(stmt: &Node<'_>, file: &str) -> IngestResult<Option<Vec<Expr>>> {
    let Some(def) = stmt.as_def_node() else { return Ok(None) };
    if std::str::from_utf8(def.name().as_slice()).ok() != Some("load_fixtures") {
        return Ok(None);
    }
    let unsupported = || IngestError::Unsupported {
        file: file.into(),
        message: "a `load_fixtures` override other than `x = super; …; x` is not supported".into(),
    };
    let Some(body) = def.body() else { return Ok(Some(Vec::new())) };
    let body = ingest_expr(&body, file)?;
    let mut stmts = match *body.node {
        ExprNode::Seq { exprs } => exprs,
        node => vec![Expr::new(body.span, node)],
    };
    if stmts.is_empty() {
        return Err(unsupported());
    }
    let first = stmts.remove(0);
    let is_super = |e: &Expr| matches!(&*e.node, ExprNode::Super { .. });
    match &*first.node {
        _ if is_super(&first) => {}
        ExprNode::Assign { target: crate::expr::LValue::Var { name, .. }, value } if is_super(value) => {
            let returns_it = stmts
                .last()
                .is_some_and(|e| matches!(&*e.node, ExprNode::Var { name: n, .. } if n == name));
            if !returns_it {
                return Err(unsupported());
            }
            stmts.pop();
        }
        _ => return Err(unsupported()),
    }
    Ok(Some(stmts))
}

/// Collect every class the file declares outside another class, each
/// with the path of the modules that enclose it. `module Models; class
/// PostTest` gives `(["Models"], PostTest)`, the same class as `class
/// Models::PostTest`. The walk does not go into class bodies (a class
/// inside a test class is a helper of that test class), except a test
/// namespace's (`is_test_namespace`): `module Delivery; class
/// Deliverable; class CustomerTest < ActiveSupport::TestCase` reopens
/// `Deliverable` only to nest the test, which is
/// `Delivery::Deliverable::CustomerTest`, so the walk goes on inside it
/// as inside a module and leaves the wrapper itself out.
fn collect_top_level_classes<'pr>(
    node: &Node<'pr>,
    scope: &[String],
    out: &mut Vec<ScopedClass<'pr>>,
) {
    collect_top_level_declarations(node, scope, out, None);
}

/// `collect_top_level_classes`, and with `modules`, the file's helper
/// modules (`helper_module`) there too, each with its scope: the walk
/// stops at one, whose classes are its own.
fn collect_top_level_declarations<'pr>(
    node: &Node<'pr>,
    scope: &[String],
    out: &mut Vec<ScopedClass<'pr>>,
    mut modules: Option<&mut Vec<ScopedModule<'pr>>>,
) {
    if let Some(c) = node.as_class_node() {
        if is_test_namespace(&c) {
            let mut inner = scope.to_vec();
            inner.extend(class_name_path(&c).unwrap_or_default());
            if let Some(body) = c.body() {
                collect_top_level_declarations(&body, &inner, out, modules);
            }
            return;
        }
        out.push((scope.to_vec(), c));
        return;
    }
    if let Some(p) = node.as_program_node() {
        collect_top_level_declarations(&p.statements().as_node(), scope, out, modules);
        return;
    }
    if let Some(s) = node.as_statements_node() {
        for stmt in s.body().iter() {
            collect_top_level_declarations(&stmt, scope, out, modules.as_deref_mut());
        }
        return;
    }
    if let Some(m) = node.as_module_node() {
        if let Some(found) = modules.as_deref_mut().filter(|_| helper_module(&m)) {
            found.push((scope.to_vec(), m));
            return;
        }
        let mut inner = scope.to_vec();
        inner.extend(module_name_path(&m).unwrap_or_default());
        if let Some(body) = m.body() {
            collect_top_level_declarations(&body, &inner, out, modules);
        }
    }
}

pub(super) type ScopedClass<'pr> = (Vec<String>, ruby_prism::ClassNode<'pr>);
pub(super) type ScopedModule<'pr> = (Vec<String>, ruby_prism::ModuleNode<'pr>);

/// A module a test file declares beside its test classes with content
/// of its own (a `def`, a constant), holding no test class: a helper
/// the file's tests include, which Ruby defines as the file loads. A
/// module that only nests declarations is a namespace the walk goes
/// through.
fn helper_module(m: &ruby_prism::ModuleNode<'_>) -> bool {
    let stmts = m.body().map(flatten_statements).unwrap_or_default();
    stmts.iter().any(|s| s.as_class_node().is_none() && s.as_module_node().is_none()) && !holds_test_class(m.body())
}

/// A class that only nests (`class Deliverable` whose body is nothing
/// but class and module declarations), is not test-shaped itself, and
/// holds a test class: a namespace the walk goes through
/// (`collect_top_level_classes`). Reopening it adds nothing to it, so
/// leaving it out loses nothing. A wrapper with content of its own
/// (a constant, a method) is not one: its tests are refused by name
/// (`refuse_unreached_test_classes`), since that content is not modeled.
fn is_test_namespace(c: &ruby_prism::ClassNode<'_>) -> bool {
    !is_test_class_node(c) && is_namespace_only(c) && holds_test_class(c.body())
}

/// The class body holds at least one class or module declaration and
/// nothing else.
fn is_namespace_only(c: &ruby_prism::ClassNode<'_>) -> bool {
    let stmts = c.body().map(flatten_statements).unwrap_or_default();
    !stmts.is_empty() && stmts.iter().all(|s| s.as_class_node().is_some() || s.as_module_node().is_some())
}

/// `body` declares a test-shaped class, directly or inside the classes
/// and modules it nests (not inside a test class: its nested classes
/// are its own).
fn holds_test_class(body: Option<Node<'_>>) -> bool {
    body.map(flatten_statements).unwrap_or_default().into_iter().any(|stmt| {
        if let Some(c) = stmt.as_class_node() {
            return is_test_class_node(&c) || holds_test_class(c.body());
        }
        stmt.as_module_node().is_some_and(|m| holds_test_class(m.body()))
    })
}

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
        // Whatever the names, a subclass that declares tests is a test
        // class (`class Checks < GeneratorTestBase` with `test "…"`):
        // as a helper its tests would be emitted and never run.
        if declares_tests(c) {
            return true;
        }
    }
    false
}

/// The tests `stmt`, a class-body statement other than a literal `test`
/// or `def test_…`, declares as the class loads, as `ingest_test_class`
/// declares them: an unrolled literal loop's (also as a constant's value,
/// `CASES = [...].each do`), in order; none for any other statement. The
/// gaps of a statement that is not modeled are not recorded again
/// (`ingest_test_class` named it). For `test_mixin`, which walks an
/// includer's body again to place its own tests among the shared ones.
pub(super) fn statement_tests<'pr>(
    stmt: &Node<'pr>,
    class_node: &ruby_prism::ClassNode<'pr>,
    class: &ClassId,
    file: &str,
    tags: &TestTags,
    consts: &TagConstants<'pr>,
) -> IngestResult<Vec<Test>> {
    let declare = |target: &Node<'pr>| match hidden_test_declaration(target) {
        Some(offset) => super::survey::without_recording(|| {
            test_unroll::declare_hidden(target, offset, class_node, class, file, tags, consts)
        })
        .map(|hidden| match hidden {
            test_unroll::Hidden::Tests(tests) => tests,
            // Declared as the class loads (`carry_at_load`), not listed.
            test_unroll::Hidden::AtLoad(_) => Vec::new(),
        }),
        None => Ok(Vec::new()),
    };
    match stmt.as_constant_write_node() {
        Some(cw) if super::library_class::is_sorbet_type_alias(&cw.value()) => Ok(Vec::new()),
        Some(cw) => declare(&cw.value()),
        None => declare(stmt),
    }
}

/// Add a test the class body declares as it loads, by a `def`
/// (`is_def`) or by ActiveSupport's `test` (a literal statement, or one
/// iteration of an unrolled loop), at `at`. A later `def` replaces an
/// earlier one in place, as Ruby does; a second `test` of a name raises
/// as ActiveSupport does. `defined_tests` records every name defined,
/// for `refuse_inherited_test_redefinitions`.
///
/// A ToggleFlagsHelper `flags:` test defines no method of its name
/// natively, only `_FLAGS:` variants run under flag configurations,
/// which are not modeled: it is left out, by name, rather than emitted
/// under a name native never defines (where it could shadow a sibling).
fn declare_test(
    tests: &mut Vec<Test>,
    defined_tests: &mut Vec<(String, bool)>,
    test: Test,
    is_def: bool,
    class: &ClassId,
    file: &str,
    at: &ruby_prism::Location<'_>,
) -> IngestResult<()> {
    if test.toggle_flags {
        let line = line_suffix(file, at);
        return super::survey::continue_or_fail(IngestError::Unsupported {
            file: file.into(),
            message: format!(
                "a ToggleFlagsHelper `flags:` test is not modeled: natively it defines no test of its \
                 own name, only `_FLAGS:<flag>:ON/OFF` variants, each run under its flag configuration; \
                 none is emitted (`{}`, test {:?}, not `{}`{line})",
                class.0, test.name, test.method_name
            ),
        });
    }
    defined_tests.push((test.method_name.clone(), !is_def));
    match tests.iter().position(|t| t.method_name == test.method_name) {
        // Ruby redefines a `def` of the same name in place.
        Some(existing) if is_def => tests[existing] = test,
        // ActiveSupport::Testing::Declarative#test raises while the
        // class body loads.
        Some(_) => {
            return Err(IngestError::Unsupported {
                file: file.into(),
                message: format!(
                    "a test name is declared twice, which ActiveSupport raises on as the class body \
                     loads; its tests would not run (`{} is already defined in {}`{})",
                    test.method_name,
                    class.0,
                    line_suffix(file, at)
                ),
            })
        }
        None => tests.push(test),
    }
    Ok(())
}

/// Recognize `setup do ... end` (Call with method=setup, block body)
/// and `def setup; ...; end` (DefNode named setup). Returns the body
/// expression in either case. Other shapes return Ok(None).
pub(super) fn ingest_setup_declaration(
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
/// (`teardown :close_files`, a block argument, `self.teardown do`) is
/// refused by name: it would otherwise fall to the unrecognized-statement
/// drop below and its cleanup would silently never run.
pub(super) fn ingest_teardown_block(stmt: &Node<'_>, file: &str) -> IngestResult<Option<Expr>> {
    let Some(call) = stmt.as_call_node() else { return Ok(None) };
    if constant_id_str(&call.name()) != "teardown" {
        return Ok(None);
    }
    // Receiverless `teardown do` is the ActiveSupport form. `self.teardown
    // do` is the same callback in some suites; refuse it rather than drop.
    if let Some(recv) = call.receiver() {
        let is_self = recv.as_self_node().is_some();
        if is_self {
            return Err(IngestError::Unsupported {
                file: file.into(),
                message: "test teardown not modeled: only receiverless `teardown do … end` runs after each test".into(),
            });
        }
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

/// Core's `TestTags` (components/platform/essentials/lib/platform/
/// testing/test_tags.rb), prepended on `ActiveSupport::TestCase`'s
/// singleton, as one class body's `test_tags`: each pair is the Hash
/// entry (key identity, `[key, value].join(":")`), in insertion order.
/// `persistent` is set once a blockless `tag(...)` installed its
/// `runnable_methods` hook.
#[derive(Clone, Debug, Default)]
pub(super) struct TestTags {
    pub(super) tags: Vec<(String, String)>,
    pub(super) persistent: bool,
}

/// `Hash#merge!`: a key already present keeps its place and takes the
/// new value.
pub(super) fn merge_tags(into: &mut Vec<(String, String)>, more: &[(String, String)]) {
    for (key, pair) in more {
        match into.iter_mut().find(|(k, _)| k == key) {
            Some(entry) => entry.1 = pair.clone(),
            None => into.push((key.clone(), pair.clone())),
        }
    }
}

/// `TestTags.generate_name_with_tags`: `name`, then `_tag:<k>:<v>` for
/// each tag joined by `_`.
pub(super) fn name_with_tags(name: &str, tags: &[(String, String)]) -> String {
    if tags.is_empty() {
        return name.to_string();
    }
    let suffix: Vec<String> = tags.iter().map(|(_, pair)| format!("tag:{pair}")).collect();
    format!("{name}_{}", suffix.join("_"))
}

/// `"test_#{name.gsub(/\s+/, '_')}"`: ActiveSupport::Testing::
/// Declarative#test's method name. Ruby's `\s` is ASCII whitespace.
fn declarative_method_name(name: &str) -> String {
    let mut out = String::from("test_");
    let mut in_space = false;
    for c in name.chars() {
        if matches!(c, ' ' | '\t' | '\n' | '\u{0b}' | '\u{0c}' | '\r') {
            if !in_space {
                out.push('_');
            }
            in_space = true;
        } else {
            out.push(c);
            in_space = false;
        }
    }
    out
}

/// One element of `[key, value].join(":")`: what `Array#join` writes for
/// a literal (`to_s`, a nested Array joined with the same separator).
/// `None` for anything whose text is not known before the code runs.
fn tag_join_piece(node: &Node<'_>) -> Option<String> {
    if let Some(sym) = node.as_symbol_node() {
        return Some(String::from_utf8_lossy(sym.unescaped()).into_owned());
    }
    if let Some(string) = node.as_string_node() {
        return Some(String::from_utf8_lossy(string.unescaped()).into_owned());
    }
    if node.as_true_node().is_some() {
        return Some("true".into());
    }
    if node.as_false_node().is_some() {
        return Some("false".into());
    }
    if node.as_nil_node().is_some() {
        return Some(String::new());
    }
    if let Some(int) = node.as_integer_node() {
        return Some(super::util::integer_decimal(&int.value()));
    }
    if let Some(array) = node.as_array_node() {
        let pieces: Option<Vec<String>> = array
            .elements()
            .iter()
            .map(|element| if element.as_splat_node().is_some() { None } else { tag_join_piece(&element) })
            .collect();
        return pieces.map(|p| p.join(":"));
    }
    None
}

/// A Hash key's identity for `merge`: `:slow` and `"slow"` are two keys.
fn tag_key_identity(node: &Node<'_>) -> Option<String> {
    let kind = if node.as_symbol_node().is_some() {
        "sym"
    } else if node.as_string_node().is_some() {
        "str"
    } else if node.as_integer_node().is_some() {
        "int"
    } else {
        return None;
    };
    Some(format!("{kind}:{}", tag_join_piece(node)?))
}

/// The pairs of a literal tags Hash (`slow: true`, `{ pg_only: true }`),
/// and whether it has ToggleFlagsHelper's `flags:` key, which is not a
/// tag of the name (its own naming is not modeled; the pair is left out).
/// `Ok(None)` when `node` is not a Hash literal.
fn literal_tags(node: &Node<'_>, file: &str) -> IngestResult<Option<(Vec<(String, String)>, bool)>> {
    let elements: Vec<Node<'_>> = if let Some(hash) = node.as_keyword_hash_node() {
        hash.elements().iter().collect()
    } else if let Some(hash) = node.as_hash_node() {
        hash.elements().iter().collect()
    } else {
        return Ok(None);
    };
    let unknown = |what: &str| IngestError::Unsupported {
        file: file.into(),
        message: format!(
            "a test tag {what} is not a literal: Core's TestTags puts each tag in the Minitest method name, \
             which is not known before the code runs"
        ),
    };
    let mut pairs: Vec<(String, String)> = Vec::new();
    let mut flags = false;
    for element in &elements {
        let Some(assoc) = element.as_assoc_node() else { return Err(unknown("splat")) };
        let key = assoc.key();
        if key.as_symbol_node().is_some_and(|s| s.unescaped() == b"flags") {
            flags = true;
            continue;
        }
        let identity = tag_key_identity(&key).ok_or_else(|| unknown("key"))?;
        let pair = format!(
            "{}:{}",
            tag_join_piece(&key).ok_or_else(|| unknown("key"))?,
            tag_join_piece(&assoc.value()).ok_or_else(|| unknown("value"))?
        );
        merge_tags(&mut pairs, &[(identity, pair)]);
    }
    Ok(Some((pairs, flags)))
}

/// The constants a test class body assigns, in order, each with where
/// its assignment ends and its value when that is a literal Hash
/// (`SLOW = { slow: true }.freeze`): a `tag(SLOW)` or `test "x", SLOW`
/// after it names that Hash, since Ruby's lexical lookup finds the
/// class's own constant first.
pub(super) struct TagConstants<'pr>(Vec<(Vec<u8>, usize, Option<Node<'pr>>)>);

impl<'pr> TagConstants<'pr> {
    pub(super) fn of(stmts: &[Node<'pr>]) -> Self {
        let mut consts = Vec::new();
        for stmt in stmts {
            let Some(cw) = stmt.as_constant_write_node() else { continue };
            let mut value = cw.value();
            if let Some(call) = value.as_call_node() {
                if call.name().as_slice() == b"freeze" && call.arguments().is_none() && call.block().is_none() {
                    if let Some(recv) = call.receiver() {
                        value = recv;
                    }
                }
            }
            let hash = (value.as_hash_node().is_some()).then_some(value);
            consts.push((cw.name().as_slice().to_vec(), cw.location().end_offset(), hash));
        }
        TagConstants(consts)
    }

    /// The literal Hash a bare constant `node` names: the class body's
    /// last assignment of it before `node`, when that is a literal Hash.
    fn resolve(&self, node: &Node<'pr>) -> Option<&Node<'pr>> {
        let read = node.as_constant_read_node()?;
        let at = node.location().start_offset();
        self.0
            .iter()
            .rev()
            .find(|(name, end, _)| *end <= at && name.as_slice() == read.name().as_slice())
            .and_then(|(_, _, hash)| hash.as_ref())
    }
}

/// `, line N` for `location` in `file` (the parse it is from), or
/// nothing.
fn line_suffix(file: &str, location: &ruby_prism::Location<'_>) -> String {
    super::sources::line_at_parse(location)
        .or_else(|| super::sources::line_at(file, location.start_offset()))
        .map(|l| format!(", line {l}"))
        .unwrap_or_default()
}

/// Source text of `node` for a message, cut at 60 characters.
fn source_excerpt(node: &Node<'_>) -> String {
    let text = String::from_utf8_lossy(node.location().as_slice()).replace('\n', " ");
    match text.char_indices().nth(60) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text,
    }
}

/// A class-body `tag(slow: true)` (TestTags#tag): its tags, and the
/// statements of its block when it has one. `Ok(None)` for any other
/// statement. The argument is a literal Hash, or a constant the class
/// body assigns one to above it (`consts`); any other `tag` is refused
/// by name, since its tags would be in the names of the tests after it.
#[allow(clippy::type_complexity)]
pub(super) fn tag_declaration<'pr>(
    stmt: &Node<'pr>,
    file: &str,
    consts: &TagConstants<'pr>,
) -> IngestResult<Option<(Vec<(String, String)>, Option<Vec<Node<'pr>>>)>> {
    let Some(call) = stmt.as_call_node() else { return Ok(None) };
    if call.receiver().is_some() || call.name().as_slice() != b"tag" {
        return Ok(None);
    }
    let line = line_suffix(file, &call.location());
    let args: Vec<Node<'pr>> = call.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default();
    let [arg] = args.as_slice() else {
        return Err(IngestError::Unsupported {
            file: file.into(),
            message: format!(
                "a test `tag` with {} arguments is not modeled: TestTags#tag takes one tags Hash (`{}`{line})",
                args.len(),
                source_excerpt(stmt)
            ),
        });
    };
    let literal = consts.resolve(arg).unwrap_or(arg);
    let Some((tags, flags)) = literal_tags(literal, file)? else {
        return Err(IngestError::Unsupported {
            file: file.into(),
            message: format!(
                "a test `tag` argument is neither a literal Hash nor a constant the class body assigns a literal \
                 Hash to above it: TestTags puts each tag in the Minitest method names of the tests after it, \
                 which are not known before the code runs (`tag {}`{line})",
                source_excerpt(arg)
            ),
        });
    };
    if flags {
        return Err(IngestError::Unsupported {
            file: file.into(),
            message: format!("a class-level `tag` with ToggleFlagsHelper's `flags:` key is not modeled (`tag {}`{line})", source_excerpt(arg)),
        });
    }
    let block = match call.block() {
        None => None,
        Some(block) => match block.as_block_node() {
            Some(block) => Some(block.body().map(flatten_statements).unwrap_or_default()),
            None => {
                return Err(IngestError::Unsupported {
                    file: file.into(),
                    message: format!(
                        "a test `tag` passed a block argument is not modeled: its tests are declared where the block runs (`{}`{line})",
                        source_excerpt(stmt)
                    ),
                })
            }
        },
    };
    Ok(Some((tags, block)))
}

/// A class-body statement, or where a `tag(...) do … end` block begins
/// and ends: TestTags#tag merges its tags for the statements in the
/// block and restores the previous ones after it.
pub(super) enum BodyItem<'pr> {
    Stmt(Node<'pr>),
    TagsBegin(Vec<(String, String)>),
    TagsEnd,
}

/// The class body's statements in order, each `tag … do` block's
/// statements spliced in between its begin and end.
pub(super) fn class_body_items<'pr>(
    stmts: Vec<Node<'pr>>,
    file: &str,
    consts: &TagConstants<'pr>,
    out: &mut Vec<BodyItem<'pr>>,
) -> IngestResult<()> {
    for stmt in stmts {
        match tag_declaration(&stmt, file, consts)? {
            Some((tags, Some(block))) => {
                out.push(BodyItem::TagsBegin(tags));
                class_body_items(block, file, consts, out)?;
                out.push(BodyItem::TagsEnd);
            }
            _ => out.push(BodyItem::Stmt(stmt)),
        }
    }
    Ok(())
}

/// Recognize a single test declaration. Two shapes are supported:
///
///   `test "name" do ... end`  — Rails AS::TestCase DSL. Test name is the
///                                string argument's text, adjacent
///                                literals (`"a " \ "b"`) joined. An
///                                optional literal tags Hash (`test "x",
///                                slow: true do`) is Core's TestTags: the
///                                method name carries the tags.
///   `def test_*; ...; end`    — vanilla Minitest. Test name is the
///                                method name with the `test_` prefix
///                                stripped and underscores replaced with
///                                spaces (mirrors how Minitest reports
///                                them; keeps the two shapes
///                                indistinguishable downstream).
///
/// Either way `method_name` is the method Minitest runs: the `def`'s
/// name, or ActiveSupport's `test_` name over the tagged name.
///
/// Returns `None` for any other statement, including a `test` whose
/// name is computed or whose tags or block are not modeled: the class
/// body loop names that statement as a gap (`test_unroll::
/// declare_hidden`), or unrolls the loop that declares it.
pub(super) fn ingest_test_declaration<'pr>(
    stmt: &Node<'pr>,
    file: &str,
    tags: &TestTags,
    consts: &TagConstants<'pr>,
) -> IngestResult<Option<Test>> {
    // `def test_*; ...; end` form (vanilla Minitest). `def self.test_*`
    // is a class method, which Minitest never runs.
    if let Some(def) = stmt.as_def_node() {
        if def.receiver().is_some() {
            return Ok(None);
        }
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
        return Ok(Some(Test { name, body, method_name: method_name.to_string(), toggle_flags: false }));
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

    // First positional argument: the test's name, known before the code
    // runs.
    let Some(args) = call.arguments() else {
        return Ok(None);
    };
    let Some(first_arg) = args.arguments().iter().next() else {
        return Ok(None);
    };
    let Some(name) = test_unroll::static_string(&first_arg) else {
        return Ok(None);
    };
    Ok(dsl_test(&call, name, Vec::new(), file, tags, consts)?.ok())
}

/// `test name, tags do … end` (`call`) as the test it defines, its name
/// already known: TestTags' name over the class's tags merged with the
/// test's own, and the block's body after `prelude` (the bindings an
/// unrolled loop gives it). Without a block ActiveSupport defines a test
/// that flunks. `Err((what, detail))` when the tags or the block are not
/// modeled; the caller names the statement.
#[allow(clippy::type_complexity)]
fn dsl_test<'pr>(
    call: &ruby_prism::CallNode<'pr>,
    name: String,
    prelude: Vec<Expr>,
    file: &str,
    tags: &TestTags,
    consts: &TagConstants<'pr>,
) -> IngestResult<Result<Test, (String, String)>> {
    let args: Vec<Node<'pr>> = call.arguments().map(|a| a.arguments().iter().collect()).unwrap_or_default();
    dsl_test_tagged(call, args.get(1..).unwrap_or_default(), name, prelude, file, tags, consts)
}

/// `dsl_test` of a call whose arguments after the name are not the
/// tags TestTags#test takes (TestGenerator's `generate_test(name,
/// location, tags:)`): `tag_args` are.
#[allow(clippy::type_complexity)]
fn dsl_test_tagged<'pr>(
    call: &ruby_prism::CallNode<'pr>,
    tag_args: &[Node<'pr>],
    name: String,
    prelude: Vec<Expr>,
    file: &str,
    tags: &TestTags,
    consts: &TagConstants<'pr>,
) -> IngestResult<Result<Test, (String, String)>> {
    // TestTags#test(name, tags = nil): the class's tags, merged with the
    // test's own.
    let (own_tags, toggle_flags) = match tag_args {
        [] => (Vec::new(), false),
        [arg] if arg.as_nil_node().is_some() => (Vec::new(), false),
        // A tags argument TestTags' name cannot be known from: the
        // statement is named as a gap by the caller.
        [arg] => {
            let not_literal = || {
                (
                    "tags are not literal".to_string(),
                    format!(
                        "test {name:?}, tags `{}`: neither a literal Hash nor a constant the class body assigns a \
                         literal Hash to above it; TestTags puts each tag in the Minitest method name",
                        source_excerpt(arg)
                    ),
                )
            };
            match literal_tags(consts.resolve(arg).unwrap_or(arg), file) {
                Ok(Some(found)) => found,
                Ok(None) => return Ok(Err(not_literal())),
                Err(IngestError::Unsupported { message, .. }) => {
                    return Ok(Err(("tags are not literal".into(), format!("test {name:?}: {message}"))))
                }
                Err(other) => return Err(other),
            }
        }
        _ => {
            return Ok(Err((
                "arguments are not a name and one tags Hash".into(),
                format!("test {name:?}: TestTags#test takes a name and one tags Hash"),
            )))
        }
    };
    let mut all_tags = tags.tags.clone();
    merge_tags(&mut all_tags, &own_tags);
    let tagged_name = name_with_tags(&name, &all_tags);
    let method_name = declarative_method_name(&tagged_name);

    // The block body — Prism exposes it via `call.block()` when present.
    let body = match call.block() {
        Some(block_ref) => match block_ref.as_block_node() {
            Some(block) => match block.body() {
                Some(body_node) => ingest_expr(&body_node, file)?,
                None => Expr::new(Span::synthetic(), ExprNode::Seq { exprs: vec![] }),
            },
            // `test "x", &blk`: the method is the block's, which is not
            // known here.
            None => {
                return Ok(Err((
                    "block is passed with `&`".into(),
                    format!("test {name:?}: its body is whatever the block is when the class body runs"),
                )))
            }
        },
        // ActiveSupport::Testing::Declarative#test without a block
        // defines a test that flunks, naming the test as TestTags
        // passed it on (with its tags).
        None => blockless_test_body(&tagged_name, super::util::node_span(&call.as_node(), file)),
    };
    let body = if prelude.is_empty() {
        body
    } else {
        let span = body.span;
        let mut exprs = prelude;
        match &*body.node {
            ExprNode::Seq { exprs: inner } => exprs.extend(inner.iter().cloned()),
            _ => exprs.push(body),
        }
        Expr::new(span, ExprNode::Seq { exprs })
    };

    Ok(Ok(Test { name, body, method_name, toggle_flags }))
}

/// The body ActiveSupport::Testing::Declarative#test defines for a
/// `test "name"` without a block: `flunk "No implementation provided
/// for name"`, one failed assertion.
fn blockless_test_body(name: &str, span: Span) -> Expr {
    let message = Expr::new(
        span,
        ExprNode::Lit { value: crate::expr::Literal::Str { value: format!("No implementation provided for {name}") } },
    );
    Expr::new(
        span,
        ExprNode::Send { recv: None, method: Symbol::from("flunk"), args: vec![message], block: None, parenthesized: true },
    )
}

#[path = "test_unroll.rs"]
mod test_unroll;

#[path = "test_macros.rs"]
mod test_macros;

/// Did ingest expand the test macro call at `offset` of `file` (its
/// tests run under the class that calls it), or name it a survey gap?
/// `TestMixins` names the calls it did neither.
pub(super) fn macro_call_expanded(file: &str, offset: usize) -> bool {
    test_macros::expanded(file, offset)
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

    /// Class-body macro calls are carried in source order, each placed
    /// by what is above it; a bare `private` and a `class << self` of
    /// methods are not statements to carry.
    #[test]
    fn class_body_statements_are_carried_in_order() {
        let src = r#"
class GcsUserTest < ActiveSupport::TestCase
  use_active_record_fixtures
  use_gcs
  self.allow_setup_mutations = true
  setup do
    @x = 1
  end
  let(:shop) { shops(:snowdevil) }
  test "a" do
    assert true
  end
  class << self
    def helper_macro; end
  end
  private
  attr_reader :x
end
"#;
        let tm = ingest_test_file(src.as_bytes(), "gcs_user_test.rb").expect("ingest").expect("test class");
        let carried: Vec<(String, bool, bool)> = tm
            .body_statements
            .iter()
            .map(|s| {
                let text = crate::emit::ruby::expr::emit_expr(&s.expr);
                (text.lines().next().unwrap_or_default().to_string(), s.after_definitions, s.after_setup_blocks)
            })
            .collect();
        let names: Vec<&str> = carried.iter().map(|(t, ..)| t.as_str()).collect();
        assert_eq!(names.len(), 5, "{carried:#?}");
        assert!(names[0].contains("use_active_record_fixtures") && names[1].contains("use_gcs"), "{carried:#?}");
        assert!(names[2].contains("allow_setup_mutations = true") && names[3].contains("let(:shop)"), "{carried:#?}");
        assert!(names[4].contains("attr_reader"), "{carried:#?}");
        let flags: Vec<(bool, bool)> = carried.iter().map(|(_, d, s)| (*d, *s)).collect();
        assert_eq!(flags, [(false, false), (false, false), (false, false), (false, true), (true, true)]);
        assert!(tm.helpers.iter().any(|m| m.name.as_str() == "helper_macro"
            && matches!(m.receiver, crate::dialect::MethodReceiver::Class)));
    }

    /// An explicit `tests Subject` is carried as written beside `target`
    /// (the Ruby emit replays it for a controller subject); a class that
    /// names no subject carries none.
    #[test]
    fn tests_macro_is_carried_only_when_written() {
        let src = "class Admin::ThingsControllerApiTest < ActionController::TestCase\n  tests Admin::ThingsController\n  test \"a\" do\n    assert true\n  end\nend\n";
        let tm = ingest_test_file(src.as_bytes(), "things_api_test.rb").expect("ingest").expect("test class");
        assert_eq!(tm.target.as_ref().map(|t| t.0.as_str()), Some("Admin::ThingsController"));
        let carried = tm.tests_macro.as_ref().expect("the macro is carried");
        assert_eq!(crate::emit::ruby::expr::emit_expr(&carried.expr), "tests Admin::ThingsController");
        assert!(tm.body_statements.is_empty(), "{:?}", tm.body_statements);

        let plain = "class ThingsControllerTest < ActionController::TestCase\n  test \"a\" do\n    assert true\n  end\nend\n";
        let tm = ingest_test_file(plain.as_bytes(), "things_test.rb").expect("ingest").expect("test class");
        assert!(tm.tests_macro.is_none());
    }

    /// A `setup do` that reads a class-body local is marked: the compiled
    /// method would lose the scope the block closes over.
    #[test]
    fn setup_block_reading_a_class_body_local_is_marked() {
        let reads = "class FilterTest < ActiveSupport::TestCase\n  default_filter = Filter.parameter_filter\n  setup do\n    Filter.parameter_filter = default_filter\n  end\n  test \"a\" do\n    assert true\n  end\nend\n";
        let tm = ingest_test_file(reads.as_bytes(), "filter_test.rb").expect("ingest").expect("test class");
        assert!(tm.setup_closes_over_class_locals);

        let own = "class FilterTest < ActiveSupport::TestCase\n  setup do\n    filter = Filter.parameter_filter\n    Filter.parameter_filter = filter\n    [1].each { |n| filter = n }\n  end\n  test \"a\" do\n    assert true\n  end\nend\n";
        let tm = ingest_test_file(own.as_bytes(), "filter_test.rb").expect("ingest").expect("test class");
        assert!(!tm.setup_closes_over_class_locals);
    }

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
        assert_eq!(helpers, ["Models::PostTest::Inner", "Models::Helper"]);
    }

    // Ingest reads every `.rb` file in the test directories, so a
    // support class in a module reaches it too. It is not a test.
    #[test]
    fn a_support_class_inside_a_module_is_not_a_test() {
        let src = "module Support\n  class Thing\n    def x\n      1\n    end\n  end\nend\n";
        let tms = ingest_test_files(src.as_bytes(), "thing.rb").expect("ingest");
        assert!(tms.is_empty());
    }

    // A top-level support class that declares no tests and inherits from
    // no class named like a test (`class MockStore`, Core's `class
    // TestAccessBlocker < Module`) is not the file's test: the file is a
    // support file, carried as a library file. One that declares tests,
    // or inherits a test-named base, still is.
    #[test]
    fn a_top_level_support_class_is_not_a_test() {
        let src = "class MockStore\n  class Bucket\n    def x\n      1\n    end\n  end\nend\n";
        assert!(ingest_test_files(src.as_bytes(), "mock_store.rb").expect("ingest").is_empty());
        let src = "class TestAccessBlocker < Module\n  def initialize(name)\n    super()\n  end\nend\n";
        assert!(ingest_test_files(src.as_bytes(), "test_access_blocker.rb").expect("ingest").is_empty());
        let src = "class Variant < ProductBaseTest\nend\n";
        let tms = ingest_test_files(src.as_bytes(), "variant.rb").expect("ingest");
        assert_eq!(tms.iter().map(|tm| tm.name.0.as_str()).collect::<Vec<_>>(), ["Variant"]);
        let src = "class Checks\n  def test_one\n    assert true\n  end\nend\n";
        let tms = ingest_test_files(src.as_bytes(), "checks.rb").expect("ingest");
        assert_eq!(tms.iter().map(|tm| tm.name.0.as_str()).collect::<Vec<_>>(), ["Checks"]);
    }

    // A class nested in a test class is that class's constant
    // (`StoreTest::Bucket`), whose `name` is that in Ruby.
    #[test]
    fn a_test_class_namespaces_its_nested_classes() {
        let src = "class StoreTest < ActiveSupport::TestCase\n  class Bucket\n  end\n\n  test \"t\" do\n    assert true\n  end\nend\n";
        let tms = ingest_test_files(src.as_bytes(), "store_test.rb").expect("ingest");
        let names: Vec<&str> = tms[0].inner_classes.iter().map(|lc| lc.name.0.as_str()).collect();
        assert_eq!(names, ["StoreTest::Bucket"]);
    }

    // A helper class, beside the test class or in its body, carries
    // what it nests under qualified names.
    #[test]
    fn a_helper_class_carries_its_nested_classes() {
        let src = r#"
module Api
  class PriceRule
    class DiscountCode < Base
    end
  end
end

class PriceRuleTest < ActiveSupport::TestCase
  class TestClass
    class Subclass < TestClass
      module Marks
        def self.mark
          1
        end
      end
    end
  end

  module Helpers
    def helped
      true
    end
  end

  test "t" do
    assert true
  end
end
"#;
        let tms = ingest_test_files(src.as_bytes(), "price_rule_test.rb").expect("ingest");
        let test = tms.iter().find(|tm| tm.name.0.as_str() == "PriceRuleTest").expect("test class");
        let names: Vec<&str> = test.inner_classes.iter().map(|lc| lc.name.0.as_str()).collect();
        assert_eq!(
            names,
            [
                "PriceRuleTest::TestClass",
                "PriceRuleTest::TestClass::Subclass",
                "PriceRuleTest::TestClass::Subclass::Marks",
                "PriceRuleTest::Helpers",
                "Api::PriceRule",
                "Api::PriceRule::DiscountCode",
            ]
        );
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

    fn method_names(src: &str) -> Vec<(String, bool)> {
        let tms = ingest_test_files(src.as_bytes(), "names_test.rb").expect("ingest");
        tms[0].tests.iter().map(|t| (t.method_name.clone(), t.toggle_flags)).collect()
    }

    #[test]
    fn method_names_are_native_minitest_names() {
        let src = "class NamesTest < ActiveSupport::TestCase\n  test \"#call ok?\", slow: true, reasons: [:a, 2], why: nil do\n  end\n  def test_x\n    assert false\n  end\n  def test_x\n    assert true\n  end\nend\n";
        let names = method_names(src);
        assert_eq!(
            names,
            [
                ("test_#call_ok?_tag:slow:true_tag:reasons:a:2_tag:why:".to_string(), false),
                // The later `def` redefines the method where it stood.
                ("test_x".to_string(), false),
            ]
        );
    }

    // A ToggleFlagsHelper `flags:` test defines no `test_x` natively,
    // only `_FLAGS:` variants run under flag configurations: it is left
    // out by name (a survey gap), not emitted as a `test_x` that would
    // stand in for, or collide with, the class's own `test "x"`.
    #[test]
    fn a_flags_test_is_left_out_by_name() {
        let src = "class NamesTest < ActiveSupport::TestCase\n  test \"x\" do\n    assert true\n  end\n  test \"x\", flags: { name: \"f\" } do\n  end\n  test \"y\", flags: { name: \"f\" }, slow: true do\n  end\nend\n";
        crate::ingest::survey::activate();
        let tms = ingest_test_files(src.as_bytes(), "names_test.rb").expect("ingest");
        let gaps: Vec<String> = crate::ingest::survey::drain().iter().map(|e| e.to_string()).collect();
        let names: Vec<&str> = tms[0].tests.iter().map(|t| t.method_name.as_str()).collect();
        assert_eq!(names, ["test_x"]);
        assert_eq!(gaps.len(), 2, "{gaps:#?}");
        assert!(gaps[0].contains("a ToggleFlagsHelper `flags:` test is not modeled"), "{}", gaps[0]);
        assert!(gaps[0].contains("(`NamesTest`, test \"x\", not `test_x`, line 5)"), "{}", gaps[0]);
        assert!(gaps[1].contains("not `test_y_tag:slow:true`, line 7)"), "{}", gaps[1]);
        // Outside the survey the class is refused.
        let err = ingest_test_files(src.as_bytes(), "names_test.rb").unwrap_err().to_string();
        assert!(err.contains("test class `NamesTest` refused: a ToggleFlagsHelper `flags:` test"), "{err}");
    }

    // ActiveSupport's `test` without a block defines a test that flunks,
    // with the name TestTags passed on.
    #[test]
    fn a_blockless_test_flunks_with_its_tagged_name() {
        let src = "class NamesTest < ActiveSupport::TestCase\n  tag(slow: true) do\n    test \"no block\"\n  end\nend\n";
        let tms = ingest_test_files(src.as_bytes(), "names_test.rb").expect("ingest");
        let test = &tms[0].tests[0];
        assert_eq!(test.method_name, "test_no_block_tag:slow:true");
        match &*test.body.node {
            ExprNode::Send { recv: None, method, args, block: None, .. } => {
                assert_eq!(method.as_str(), "flunk");
                assert!(matches!(
                    &*args[0].node,
                    ExprNode::Lit { value: crate::expr::Literal::Str { value } }
                        if value == "No implementation provided for no block_tag:slow:true"
                ));
            }
            other => panic!("{other:?}"),
        }
        // `test "x", &blk` declares its test as the class loads: the
        // statement is carried as written, the class lists the others.
        let src = "class NamesTest < ActiveSupport::TestCase\n  test \"from a proc\", &BODY\n  test \"kept\" do\n  end\nend\n";
        let tms = ingest_test_files(src.as_bytes(), "names_test.rb").expect("ingest");
        assert_eq!(tms[0].tests.iter().map(|t| t.method_name.as_str()).collect::<Vec<_>>(), ["test_kept"]);
        assert!(tms[0].tests_at_load);
        assert_eq!(tms[0].body_statements.len(), 1);
    }

    // TestGenerator's `generate_test` in the class body is a named gap; in
    // a `class << self` macro it is expanded at each call
    // (`test_macros`), and a call that cannot be expanded is named.
    #[test]
    fn a_generate_test_is_a_named_gap_unless_a_macro_call_expands() {
        let src = "class GenTest < ActiveSupport::TestCase\n  extend TestGenerator\n  generate_test(\"made\") do\n    assert true\n  end\n  test \"kept\" do\n  end\nend\n";
        let tms = ingest_test_files(src.as_bytes(), "gen_test.rb").expect("ingest");
        assert!(tms[0].tests_at_load, "generate_test declares its test as the class loads");
        assert_eq!(tms[0].tests.iter().map(|t| t.method_name.as_str()).collect::<Vec<_>>(), ["test_kept"]);
        let src = "class NamesTest < ActiveSupport::TestCase\n  extend TestGenerator\n  class << self\n    def checks(k)\n      generate_test(\"checks #{k}\") do\n      end\n    end\n  end\n  checks :a\n  test \"kept\" do\n  end\nend\n";
        let names: Vec<String> = method_names(src).into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, ["test_checks_a", "test_kept"]);
        // (Each source its own path: the registry keeps a path's first
        // text.) Without `extend TestGenerator`, `generate_test` is not its.
        // (And each class its own name: a class declared in two sources
        // may be given methods by the other.)
        let src = "class GenPlainTest < ActiveSupport::TestCase\n  class << self\n    def checks(k)\n      generate_test(\"checks #{k}\") do\n      end\n    end\n  end\n  checks :a\n  test \"kept\" do\n  end\nend\n";
        // The call is carried: it runs as the class loads, as Ruby does.
        let tms = ingest_test_files(src.as_bytes(), "gen_plain_test.rb").expect("ingest");
        assert!(tms[0].tests_at_load);
        assert_eq!(tms[0].tests.iter().map(|t| t.method_name.as_str()).collect::<Vec<_>>(), ["test_kept"]);
        // A call before the macro is defined runs no macro (native
        // raises NoMethodError): nothing is expanded, the call is carried
        // and raises as the class loads, as native does.
        let src = "class GenLateTest < ActiveSupport::TestCase\n  extend TestGenerator\n  checks :a\n  class << self\n    def checks(k)\n      generate_test(\"checks #{k}\") do\n      end\n    end\n  end\nend\n";
        let tms = ingest_test_files(src.as_bytes(), "gen_late_test.rb").expect("ingest");
        assert!(tms[0].tests_at_load && tms[0].tests.is_empty());
    }

    // TestTags' persistent rename aliases `test_x` onto the name the
    // second `test "x"` (declared under the tag) defined: one test is
    // left, with the first body.
    #[test]
    fn a_persistent_rename_onto_a_tagged_twin_keeps_the_first_body() {
        let src = "class NamesTest < ActiveSupport::TestCase\n  test \"x\" do\n    first\n  end\n  tag a: 1\n  test \"x\" do\n    second\n  end\n  test \"z\" do\n  end\nend\n";
        let tms = ingest_test_files(src.as_bytes(), "names_test.rb").expect("ingest");
        let tests: Vec<(&str, String)> = tms[0]
            .tests
            .iter()
            .map(|t| (t.method_name.as_str(), format!("{:?}", t.body.node)))
            .collect();
        assert_eq!(tests.len(), 2, "{tests:#?}");
        assert_eq!(tests[0].0, "test_x_tag:a:1");
        assert!(tests[0].1.contains("first") && !tests[0].1.contains("second"), "{}", tests[0].1);
        assert_eq!(tests[1].0, "test_z_tag:a:1");
        // The names defined as the body loads, before the rename.
        assert_eq!(
            tms[0].defined_tests,
            [("test_x".to_string(), true), ("test_x_tag:a:1".to_string(), true), ("test_z_tag:a:1".to_string(), true)]
        );
    }

    // `tag(SLOW)` and `test "x", SLOW` name a literal Hash the class
    // body assigns above them; any other `tag` argument is refused by
    // name, since its tags would be in the names of the tests after it.
    #[test]
    fn a_tag_constant_of_the_class_body_resolves() {
        let src = "class NamesTest < ActiveSupport::TestCase\n  SLOW = { slow: true }.freeze\n  OWNER = { owner: :billing }\n  tag(SLOW) do\n    test \"a\" do\n    end\n  end\n  test \"b\", OWNER do\n  end\n  tag OWNER\n  test \"c\" do\n  end\nend\n";
        let names: Vec<String> = method_names(src).into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, ["test_a_tag:slow:true", "test_b_tag:owner:billing", "test_c_tag:owner:billing"]);
        for (body, needle) in [
            ("  tag(LATER) do\n    test \"a\" do\n    end\n  end\n  LATER = { slow: true }\n", "(`tag LATER`, line 2)"),
            ("  tag Other::TAGS\n  test \"a\" do\n  end\n", "(`tag Other::TAGS`, line 2)"),
            ("  tag tags_for(:x)\n  test \"a\" do\n  end\n", "(`tag tags_for(:x)`, line 2)"),
            ("  LIST = [1]\n  tag LIST\n", "(`tag LIST`, line 3)"),
        ] {
            let src = format!("class NamesTest < ActiveSupport::TestCase\n{body}end\n");
            let err = ingest_test_files(src.as_bytes(), "names_test.rb").unwrap_err().to_string();
            assert!(err.contains("a test `tag` argument is neither a literal Hash nor a constant"), "{err}");
            assert!(err.contains(needle), "{needle}: {err}");
        }
        let src = "class NamesTest < ActiveSupport::TestCase\n  tag\n  tag(a: 1, &blk)\nend\n";
        let err = ingest_test_files(src.as_bytes(), "names_test.rb").unwrap_err().to_string();
        assert!(err.contains("a test `tag` with 0 arguments is not modeled"), "{err}");
        let src = "class NamesTest < ActiveSupport::TestCase\n  test \"b\", owner_tags do\n  end\nend\n";
        let tms = ingest_test_files(src.as_bytes(), "names_test.rb").expect("ingest");
        assert!(tms[0].tests_at_load && tms[0].tests.is_empty());
    }

    // A subclass whose tests are all inside `tag … do` blocks is a test
    // class: it declares tests as much as one that writes them bare.
    #[test]
    fn a_class_whose_tests_are_in_a_tag_block_is_a_test_class() {
        let src = "class Helper\nend\n\nclass Checks < GeneratorTestBase\n  tag(slow: true) do\n    test \"a\" do\n    end\n  end\nend\n";
        let tms = ingest_test_files(src.as_bytes(), "checks.rb").expect("ingest");
        let names: Vec<&str> = tms.iter().map(|tm| tm.name.0.as_str()).collect();
        assert_eq!(names, ["Checks"]);
        assert_eq!(tms[0].tests[0].method_name, "test_a_tag:slow:true");
    }

    // Core's `class Event < GlobalDB::ShopRecord; class ChargeTest <
    // ActiveSupport::TestCase`: a class whose body only declares
    // classes is a namespace, like a module, when it holds a test class.
    // The test class is reached under its qualified name; the namespace
    // is no fallback test class.
    #[test]
    fn a_test_class_in_a_namespace_class_is_reached() {
        let src = r#"
class Event < GlobalDB::ShopRecord
  class ChargeTest < ActiveSupport::TestCase
    test "a" do
    end
  end
end

module Billing
  class Mapper
    class Helper
    end

    class ResultTest < ActiveSupport::TestCase
      test "b" do
      end
    end
  end
end

class Plain
  class InnerTest < Minitest::Test
    def test_c
    end
  end
end
"#;
        let tms = ingest_test_files(src.as_bytes(), "event_test.rb").expect("ingest");
        let names: Vec<(&str, Vec<&str>)> = tms
            .iter()
            .map(|tm| (tm.name.0.as_str(), tm.tests.iter().map(|t| t.method_name.as_str()).collect()))
            .collect();
        assert_eq!(
            names,
            [
                ("Event::ChargeTest", vec!["test_a"]),
                ("Billing::Mapper::ResultTest", vec!["test_b"]),
                ("Plain::InnerTest", vec!["test_c"]),
            ]
        );
        let helpers: Vec<&str> = tms[1].inner_classes.iter().map(|lc| lc.name.0.as_str()).collect();
        assert!(helpers.contains(&"Billing::Mapper::Helper"), "{helpers:?}");
    }

    // A class with content of its own around a test class is not a
    // namespace: the test class is refused by name, with the reason in
    // the message ahead of the names (the survey buckets by it).
    #[test]
    fn a_test_class_in_a_class_with_content_is_refused_with_its_reason() {
        let src = "class Widget\n  def x\n  end\n\n  class WidgetTest < ActiveSupport::TestCase\n    test \"a\" do\n    end\n  end\nend\n\nclass OtherTest < ActiveSupport::TestCase\n  test \"b\" do\n  end\nend\n";
        let err = ingest_test_files(src.as_bytes(), "widget_test.rb").unwrap_err().to_string();
        assert!(
            err.contains(
                "test class `Widget::WidgetTest` refused: it is nested in a class with statements of its own besides \
                 class and module declarations, and reopening such a class around a test class is not modeled; its \
                 tests would not run (`Widget::WidgetTest` in `Widget`, line 5)"
            ),
            "{err}"
        );
        let src = "class OtherTest < ActiveSupport::TestCase\n  test \"b\" do\n  end\nend\n\nif ENV[\"X\"]\n  class CondTest < ActiveSupport::TestCase\n    test \"a\" do\n    end\n  end\nend\n";
        let err = ingest_test_files(src.as_bytes(), "cond_test.rb").unwrap_err().to_string();
        assert!(
            err.contains("test class `CondTest` refused: it is declared inside a conditional, which is not modeled; its tests would not run (`CondTest`, line 7)"),
            "{err}"
        );
    }

    // ActiveSupport's `method_defined?` sees inherited methods: a
    // subclass's `test "shared"` where its parent defines `test_shared`
    // raises as the class loads. The class is refused by name, and the
    // test classes below it with it; a `def` override is Ruby's own.
    #[test]
    fn a_test_redefining_an_inherited_test_is_refused() {
        let base = "class BaseEdgeTest < ActiveSupport::TestCase\n  test \"shared\" do\n  end\n  def test_by_def\n  end\nend\n";
        let sub = "class SubEdgeTest < BaseEdgeTest\n  def test_shared\n  end\n  test \"by def\" do\n  end\nend\n";
        let leaf = "class LeafEdgeTest < SubEdgeTest\n  test \"own\" do\n  end\nend\n";
        let ok = "class OkEdgeTest < BaseEdgeTest\n  def test_shared\n  end\nend\n";
        let mut app = crate::App::default();
        for (src, file) in [(base, "base.rb"), (sub, "sub.rb"), (leaf, "leaf.rb"), (ok, "ok.rb")] {
            for mut tm in ingest_test_files(src.as_bytes(), file).expect("ingest") {
                tm.source_file = Some(format!("test/{file}"));
                app.test_modules.push(tm);
            }
        }
        let err = refuse_inherited_test_redefinitions(&mut app.clone()).unwrap_err().to_string();
        assert!(err.contains("(`test_by_def is already defined in SubEdgeTest`, inherited from `BaseEdgeTest`)"), "{err}");
        crate::ingest::survey::activate();
        refuse_inherited_test_redefinitions(&mut app).expect("survey");
        let gaps: Vec<String> = crate::ingest::survey::drain().iter().map(|e| e.to_string()).collect();
        let names: Vec<&str> = app.test_modules.iter().map(|tm| tm.name.0.as_str()).collect();
        assert_eq!(names, ["BaseEdgeTest", "OkEdgeTest"]);
        assert_eq!(gaps.len(), 2, "{gaps:#?}");
        assert!(gaps[0].contains("test class `SubEdgeTest` refused: a `test` reuses the name of an inherited test method"), "{}", gaps[0]);
        assert!(gaps[1].contains("test class `LeafEdgeTest` refused: it inherits from `SubEdgeTest`"), "{}", gaps[1]);
    }

    #[test]
    fn a_tag_whose_name_text_is_unknown_is_refused() {
        for args in ["slow: SLOW", "**more", "slow: \"#{x}\""] {
            let src = format!("class NamesTest < ActiveSupport::TestCase\n  test \"a\", {args} do\n  end\nend\n");
            // Carried: `test` runs as the class loads and names the test.
            let tms = ingest_test_files(src.as_bytes(), "names_test.rb").expect("ingest");
            assert!(tms[0].tests_at_load && tms[0].tests.is_empty(), "{args}");
        }
        let src = "class NamesTest < ActiveSupport::TestCase\n  tag(owner: OWNER) do\n    test \"a\" do\n    end\n  end\nend\n";
        let err = ingest_test_files(src.as_bytes(), "names_test.rb").unwrap_err().to_string();
        assert!(err.contains("is not a literal"), "{err}");
    }
}

/// A test class's superclass is a name Ruby resolves in the modules
/// around the class: `class SkiplistTest < TestCase` inside `module
/// SqliteTestCompat` subclasses the `SqliteTestCompat::TestCase` the
/// helper defines. Where Rubydex resolves the written name to a test
/// class the app defines, that class is the parent, so the emitted
/// file loads it and the analyzer inherits its helpers. Any other
/// parent keeps its written path.
pub(super) fn resolve_test_parents(app: &mut crate::App) -> IngestResult<()> {
    let resolver = app.const_resolver.for_app(app);
    let defined: crate::det::HashSet<ClassId> = app.test_modules.iter().map(|tm| tm.name.clone()).collect();
    for tm in &mut app.test_modules {
        let (Some(span), Some(parent)) = (tm.parent_span, &tm.parent) else { continue };
        let path: Vec<Symbol> = parent.0.as_str().split("::").map(Symbol::from).collect();
        // The superclass is read before the class exists, so a name that
        // resolves to the class itself names an outer one in Ruby.
        if let Some(id) = resolver.namespace(span, &path).filter(|id| defined.contains(*id) && **id != tm.name) {
            tm.parent = Some(id.clone());
        }
    }
    refuse_inherited_test_redefinitions(app)
}

/// ActiveSupport::Testing::Declarative#test raises `test_x is already
/// defined in SubTest` when `method_defined?` sees the name, inherited
/// methods included: a subclass's `test "x"` where a test class it
/// inherits from defines `test_x` stops the class body loading, and the
/// run. Such a class is refused by name, and so is every test class
/// that inherits from it (its superclass would not exist). A `def
/// test_x` in the subclass overrides the inherited one, as Ruby does.
/// Ancestors outside the authored test classes (support files,
/// included modules) are not searched.
fn refuse_inherited_test_redefinitions(app: &mut crate::App) -> IngestResult<()> {
    let by_name: crate::det::HashMap<ClassId, usize> =
        app.test_modules.iter().enumerate().map(|(at, tm)| (tm.name.clone(), at)).collect();
    let parent_of = |at: usize| app.test_modules[at].parent.as_ref().and_then(|p| by_name.get(p).copied());
    let mut refused: Vec<(usize, String)> = Vec::new();
    for (at, tm) in app.test_modules.iter().enumerate() {
        let mut seen = vec![at];
        let mut ancestor = parent_of(at);
        let clash = 'walk: loop {
            let Some(up) = ancestor.filter(|up| !seen.contains(up)) else { break None };
            let defined = &app.test_modules[up].defined_tests;
            for (test, _) in tm.defined_tests.iter().filter(|(_, declared)| *declared) {
                if defined.iter().any(|(name, _)| name == test) {
                    break 'walk Some((test.clone(), app.test_modules[up].name.0.to_string()));
                }
            }
            seen.push(up);
            ancestor = parent_of(up);
        };
        if let Some((test, ancestor)) = clash {
            refused.push((
                at,
                format!(
                    "test class `{0}` refused: a `test` reuses the name of an inherited test method, which \
                     ActiveSupport raises on as the class body loads; its tests would not run (`{test} is already \
                     defined in {0}`, inherited from `{ancestor}`)",
                    tm.name.0
                ),
            ));
        }
    }
    if refused.is_empty() {
        return Ok(());
    }
    // Every test class below a refused one goes with it.
    let mut gone: crate::det::HashSet<usize> = refused.iter().map(|(at, _)| *at).collect();
    loop {
        let more: Vec<usize> = (0..app.test_modules.len())
            .filter(|at| !gone.contains(at) && parent_of(*at).is_some_and(|up| gone.contains(&up)))
            .collect();
        if more.is_empty() {
            break;
        }
        for at in more {
            let parent = app.test_modules[at].parent.as_ref().map(|p| p.0.to_string()).unwrap_or_default();
            refused.push((
                at,
                format!(
                    "test class `{}` refused: it inherits from `{parent}`, whose class body raises as it loads; \
                     its tests would not run",
                    app.test_modules[at].name.0
                ),
            ));
            gone.insert(at);
        }
    }
    refused.sort_by_key(|(at, _)| *at);
    for (at, message) in &refused {
        let file = app.test_modules[*at].source_file.clone().unwrap_or_default();
        super::survey::continue_or_fail(IngestError::Unsupported { file, message: message.clone() })?;
    }
    let mut at = 0;
    app.test_modules.retain(|_| {
        let keep = !gone.contains(&at);
        at += 1;
        keep
    });
    Ok(())
}

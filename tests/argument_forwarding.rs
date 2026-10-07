//! Full forwarding has a positional/keyword/block contract, not a naming convention.
//! Native controls deliberately distinguish all three from plausible miscompiles.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;

use roundhouse::analyze::{Analyzer, diagnose, nameless_rest_refusals_off_ruby};
use roundhouse::diagnostic::Severity;
use roundhouse::ingest::ingest_app_from_tree;

const LIBRARY: &str = r#"
class RequiredSink
  def combine(first, second, factor:, offset: 0, &blk)
    result = (first - second) * factor + offset
    blk ? blk.call(result) : result
  end
end
class NoKeywordSink
  def combine(first, second, &blk)
    result = first - second
    blk ? blk.call(result) : result
  end
end
class ShapeSink
  def combine(first, options, factor:)
    yield(first * factor + options[:factor])
  end
end
class AnonymousKeywordSink
  def combine(first, __fwd_kwargs, **)
    first - __fwd_kwargs
  end
end
class DiscardedKeywordSink
  def combine(first, second, *rest, **ignored)
    first - second + rest.length
  end
end
class Forwarder
  def call(...)
    helper = RequiredSink.new
    helper.combine(...)
  end
  def empty(...)
    helper = NoKeywordSink.new
    helper.combine(...)
  end
  def shapes(...)
    helper = ShapeSink.new
    helper.combine(...)
  end
  def discard(...)
    11
  end
  def anonymous_keywords(...)
    helper = AnonymousKeywordSink.new
    helper.combine(...)
  end
  def discarded_keywords(...)
    helper = DiscardedKeywordSink.new
    helper.combine(...)
  end
  def self.call(...)
    target(...)
  end
  def self.target(first, second, factor:)
    yield((first - second) * factor)
  end
  def leading(__fwd_args, ...)
    helper = RequiredSink.new
    helper.combine(__fwd_args, ...)
  end
  def collisions(...)
    __fwd_args = [1, 2]
    __fwd_kwargs = {factor: 99}
    __fwd_blk = nil
    helper = RequiredSink.new
    helper.combine(...)
  end
end
class Bridge
  def call(...)
    helper = Forwarder.new
    helper.call(...)
  end
end
class Child < RequiredSink
  def combine(...)
    super(...)
  end
end
class ImplicitChild < RequiredSink
  def combine(...)
    super
  end
end
class Inherited < Forwarder
end
class Ordinary
  def call(__fwd_args, __fwd_kwargs, &block)
    helper = RequiredSink.new
    begin
      helper.combine(*__fwd_args, __fwd_kwargs, &block)
    rescue ArgumentError
      "rejected"
    end
  end
end
class CompiledEntry
  def self.call
    helper = Forwarder.new
    helper.call(11, 4, factor: 3) { |r| r * 2 + 1 }
  end
end
class NamedNew
  def initialize(factor: 2)
    @factor = factor
  end
  def new(first, second, factor:)
    (first - second) * factor
  end
  def relay(...)
    new(...)
  end
end
"#;

const CONCERN: &str = r#"
module ForwardingConcern
  extend ActiveSupport::Concern
  def forwarded_concern(...)
    helper = RequiredSink.new
    helper.combine(...)
  end
  class_methods do
    def forwarded_class(...)
      helper = RequiredSink.new
      helper.combine(...)
    end
  end
end
"#;

const MODEL_METHODS: &str = r#"
  include ForwardingConcern
  def forwarded_model(...)
    helper = RequiredSink.new
    helper.combine(...)
  end
  def discard_model_keywords(first, __fwd_kwargs, **)
    first - __fwd_kwargs
  end
  def named_model(__fwd_args, *__fwd_kwargs, last, &__blk)
    result = __fwd_args - last + __fwd_kwargs.length
    __blk ? __blk.call(result) : result
  end
  def kwargs_model(*values, **__fwd_kwargs, &__fwd_blk)
    result = values.length + __fwd_kwargs.length
    __fwd_blk ? __fwd_blk.call(result) : result
  end
"#;

const ASSERTIONS: &str = r#"
def expect_value(expected, actual)
  raise "expected #{expected.inspect}, got #{actual.inspect}" unless expected == actual
end
def expect_argument_error
  raised = false
  begin
    yield
  rescue ArgumentError
    raised = true
  end
  raise "expected ArgumentError" unless raised
end
f = Forwarder.new
expect_value(23, f.call(11, 4, factor: 3) { |r| r + 2 })
expect_value(43, f.call(11, 4, factor: 3) { |r| r * 2 + 1 })
expect_value(21, f.call(11, 4, factor: 3))
expect_value(28, f.call(11, 4, factor: 3, offset: 5) { |r| r + 2 })
expect_value(9, f.empty(11, 4) { |r| r + 2 })
expect_value(9, f.empty(11, 4, **{}) { |r| r + 2 })
expect_value(75, f.shapes(11, {factor: 4}, factor: 3) { |r| r * 2 + 1 })
expect_value(11, f.discard(1, 2, 3, factor: 9) { raise "unused block" })
expect_value(7, f.anonymous_keywords(11, 4))
expect_value(7, f.anonymous_keywords(11, 4, factor: 3, extra: 9))
expect_value(9, f.discarded_keywords(11, 4, 8, 10, factor: 3))
expect_value(23, Forwarder.call(11, 4, factor: 3) { |r| r + 2 })
expect_value(23, f.leading(11, 4, factor: 3) { |r| r + 2 })
expect_value(23, f.collisions(11, 4, factor: 3) { |r| r + 2 })
expect_value(23, Bridge.new.call(11, 4, factor: 3) { |r| r + 2 })
expect_value(23, Child.new.combine(11, 4, factor: 3) { |r| r + 2 })
expect_value(23, ImplicitChild.new.combine(11, 4, factor: 3) { |r| r + 2 })
expect_value(23, Inherited.new.call(11, 4, factor: 3) { |r| r + 2 })
expect_value(23, Article.new.forwarded_model(11, 4, factor: 3) { |r| r + 2 })
expect_value(23, Article.new.forwarded_concern(11, 4, factor: 3) { |r| r + 2 })
expect_value(23, Article.forwarded_class(11, 4, factor: 3) { |r| r + 2 })
expect_value(7, Article.new.discard_model_keywords(11, 4, factor: 3, extra: 9))
expect_value(11, Article.new.named_model(11, 8, 10, 4) { |r| r + 2 })
expect_value(6, Article.new.kwargs_model(11, 4, factor: 3, extra: 9) { |r| r + 2 })
expect_value("rejected", Ordinary.new.call([11, 4], {factor: 3}) { |r| r + 2 })
expect_value(43, CompiledEntry.call)
expect_value(21, NamedNew.new.relay(11, 4, factor: 3))
expect_argument_error { f.call(11, 4) }
expect_argument_error { f.call(11, 4, factor: 3, unknown: 7) }
expect_argument_error { f.call(11, factor: 3) }
expect_argument_error { f.call(11, 4, 9, factor: 3) }
puts "forwarding contract passed"
"#;

#[test]
fn native_and_emitted_forwarding_preserve_arguments_keywords_and_block() {
    let native = Command::new("ruby")
        .args(["-ractive_support/concern", "-e"])
        .arg(format!(
            "{LIBRARY}\n{CONCERN}\nclass Article\n{MODEL_METHODS}\nend\n{ASSERTIONS}"
        ))
        .output()
        .expect("native Ruby control");
    assert!(
        native.status.success(),
        "{}",
        String::from_utf8_lossy(&native.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&native.stdout),
        "forwarding contract passed\n"
    );

    let run = emit_and_run::real_blog()
        .write("app/lib/forwarder.rb", LIBRARY)
        .write("app/models/concerns/forwarding_concern.rb", CONCERN)
        .edit(
            "app/models/article.rb",
            "class Article < ApplicationRecord\n",
            &format!("class Article < ApplicationRecord\n{MODEL_METHODS}\n"),
        )
        .run_ruby(ASSERTIONS);
    run.assert_passes();
    assert_eq!(run.stdout, "forwarding contract passed\n");
    let emitted = std::fs::read_to_string(run.emitted.join("app/models/forwarder.rb")).unwrap();
    assert!(emitted.contains("def call(...)"), "{emitted}");
    assert!(emitted.contains("helper.combine(...)"), "{emitted}");
    assert!(!emitted.contains("def call(*__fwd_args"), "{emitted}");
    let rbs = std::fs::read_to_string(run.emitted.join("sig/app/models/forwarder.rbs")).unwrap();
    assert!(
        rbs.contains(
            "def call: (*untyped, **untyped) ?{ (*untyped, **untyped) -> untyped } -> untyped"
        ),
        "{rbs}"
    );
    assert!(!rbs.contains("untyped ..."), "{rbs}");
    ruby_rbs::node::parse(&rbs).expect("forwarding sidecar parses as RBS");
    let anonymous =
        std::fs::read_to_string(run.emitted.join("app/models/anonymous_keyword_sink.rb")).unwrap();
    assert!(
        anonymous.contains("def combine(first, __fwd_kwargs, **)"),
        "{anonymous}"
    );
    let anonymous_rbs = std::fs::read_to_string(
        run.emitted
            .join("sig/app/models/anonymous_keyword_sink.rbs"),
    )
    .unwrap();
    ruby_rbs::node::parse(&anonymous_rbs)
        .expect("anonymous keyword declaration sidecar parses as RBS");
}

fn analyzed(source: &str) -> roundhouse::App {
    let mut app = ingest_app_from_tree(HashMap::from([(
        PathBuf::from("app/lib/probe.rb"),
        source.as_bytes().to_vec(),
    )]))
    .expect("ingest synthetic forwarding source");
    Analyzer::new(&app).analyze(&mut app);
    app
}

#[test]
fn forwarding_markers_are_not_values_or_gradual_escapes() {
    use roundhouse::diagnostic::DiagnosticKind;
    use roundhouse::expr::ExprNode;

    let sink = "class Sink; def self.target(a,b); a-b; end; end";
    let methods = "def relay(...); Sink.target(...); end; def opaque; yield; end";
    let mut app = ingest_app_from_tree(HashMap::from([
        (PathBuf::from("app/lib/sink.rb"), sink.as_bytes().to_vec()),
        (PathBuf::from("app/models/article.rb"), format!("class Article < ApplicationRecord; {methods}; end").into_bytes()),
    ])).unwrap();
    Analyzer::new(&app).analyze(&mut app);
    let mut pending: Vec<_> = app.models[0].methods().map(|m| &m.body).collect();
    let mut packets = Vec::new();
    let mut yields = Vec::new();
    while let Some(expr) = pending.pop() {
        match &*expr.node {
            ExprNode::ForwardArgs => packets.push(expr.span),
            ExprNode::Yield { .. } => yields.push(expr.span),
            _ => {}
        }
        expr.node.for_each_child(&mut |child| pending.push(child));
    }
    assert_eq!(packets.len(), 1);
    assert_eq!(yields.len(), 1);
    let diags = diagnose(&app);
    let gradual: Vec<_> = diags.iter().filter(|d| matches!(d.kind, DiagnosticKind::GradualUntyped { .. })).collect();
    assert!(!gradual.iter().any(|d| packets.contains(&d.span)), "{diags:?}");
    assert!(gradual.iter().any(|d| yields.contains(&d.span)), "{diags:?}");

    let script = "puts Article.new.relay(11,3)";
    native_result(&format!("{sink}; class Article; {methods}; end"), script, "8\n");
    let run = emit_and_run::real_blog()
        .write("app/lib/sink.rb", sink)
        .edit("app/models/article.rb", "class Article < ApplicationRecord\n", &format!("class Article < ApplicationRecord\n {methods}\n"))
        .run_ruby(script);
    run.assert_passes();
    assert_eq!(run.stdout, "8\n");
}

#[test]
fn flattened_optional_keyword_callee_is_honestly_unsupported() {
    let source = r#"
class Sink
  def call(first, second, factor: 2)
    (first - second) * factor
  end
end
class Probe
  def call(...)
    helper = Sink.new
    helper.call(...)
  end
end
"#;
    let app = analyzed(source);
    let errors: Vec<_> = diagnose(&app)
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(
        errors
            .iter()
            .any(|d| d.message.contains("forwarding") && d.message.contains("flattened")),
        "{errors:?}"
    );
}

#[test]
fn forwarding_declaration_and_call_round_trip_as_a_contract() {
    let source = "class Probe\n  def call(__fwd_args, ...)\n    target(__fwd_args, ...)\n  end\n  def target(first, second, factor:)\n    yield((first - second) * factor)\n  end\nend\n";
    let app = analyzed(source);
    let class = app
        .library_classes
        .iter()
        .find(|c| c.name.0.as_str() == "Probe")
        .unwrap();
    let emitted = format!(
        "class Probe\n{}end\n",
        class
            .methods
            .iter()
            .map(roundhouse::emit::ruby::emit_method)
            .collect::<String>()
    );
    let again = analyzed(&emitted);
    let second = again
        .library_classes
        .iter()
        .find(|c| c.name.0.as_str() == "Probe")
        .unwrap();
    assert_eq!(
        format!(
            "class Probe\n{}end\n",
            second
                .methods
                .iter()
                .map(roundhouse::emit::ruby::emit_method)
                .collect::<String>()
        ),
        emitted
    );
    assert!(emitted.contains("def call(__fwd_args, ...)"), "{emitted}");
    assert!(emitted.contains("target(__fwd_args, ...)"), "{emitted}");
}

#[test]
fn flattened_and_unknown_contracts_remain_errors_through_lowering() {
    for source in [
        "class Probe\n def call(...)\n target(...)\n end\n def target(a, b, factor: 2)\n (a-b)*factor\n end\nend",
        "class Probe\n def call(...)\n self.target(...)\n end\n def target(a, b, **kw)\n (a-b)*kw[:factor]\n end\nend",
        "class Parent\n def call(a, b, factor: 2)\n (a-b)*factor\n end\nend\nclass Child < Parent\n def call(...)\n super\n end\nend",
        "module Mixed\n def call(a, b, factor: 2)\n (a-b)*factor\n end\nend\nclass Parent\n def call(a,b,factor:)\n (a-b)*factor\n end\nend\nclass Child < Parent\n include Mixed\n def call(...)\n super(...)\n end\nend",
        "class Probe\n def call(...)\n absent(...)\n end\nend",
    ] {
        let native = Command::new("ruby")
            .args(["-c", "-e", source])
            .output()
            .unwrap();
        assert!(native.status.success(), "native legal source: {source}");
        let mut app = analyzed(source);
        let lower = roundhouse::session::analyze_and_lower(&mut app);
        let errors: Vec<_> = diagnose(&app)
            .into_iter()
            .chain(lower)
            .filter(|d| d.severity == Severity::Error)
            .collect();
        assert!(
            errors.iter().any(|d| d.message.contains("forwarding")),
            "{source}\n{errors:?}"
        );
    }
}

#[test]
fn keyword_target_gates_distinguish_ordinary_super_from_full_forwarding() {
    use roundhouse::diagnostic::DiagnosticKind;
    use roundhouse::project::{BuildTarget, target_files};

    for (source, expected) in [
        ("class Probe < StandardError; def initialize; options={}; super(**options); end; end",
         "keyword splat in ordinary super"),
        ("class Probe; def self.call(...); 11; end; def self.run; options={}; call(**options); end; end",
         "keyword splat into full argument forwarding"),
    ] {
        let app = analyzed(source);
        for target in [BuildTarget::Rust, BuildTarget::Typescript, BuildTarget::Spinel, BuildTarget::Roda] {
            let (_, diagnostics) = roundhouse::emit::diagnostics::scope(|| {
                target_files(&app, roundhouse::fixtures::real_blog(), target)
            });
            let gates: Vec<_> = diagnostics.iter().filter(|d| {
                matches!(&d.kind, DiagnosticKind::Unsupported { construct, .. }
                    if construct.as_str() == "keyword splat in ordinary super"
                        || construct.as_str() == "keyword splat into full argument forwarding")
            }).collect();
            assert_eq!(gates.len(), 1, "{target:?}: {source}: {diagnostics:?}");
            let gate = gates[0];
            assert_eq!(gate.severity, Severity::Error);
            assert!(!gate.span.is_synthetic());
            assert!(matches!(&gate.kind, DiagnosticKind::Unsupported { construct, target: Some(name), .. }
                if construct.as_str() == expected && name.as_str() == target.as_str()), "{gate:?}");
            if expected == "keyword splat in ordinary super" {
                assert!(gate.message.contains("argument ABI"), "{gate:?}");
            }
        }
    }
}

#[test]
fn source_archive_copies_unrepresented_formals_without_a_transpile_error() {
    use roundhouse::diagnostic::DiagnosticKind;
    use roundhouse::project::{BuildTarget, target_files};

    for formal in ["(a,b)", "**nil"] {
        let source = format!("class Probe; def call({formal}); 7; end; end\n");
        let app = analyzed(&source);
        let fixture = std::env::temp_dir().join(format!("roundhouse-source-archive-{}-{}", std::process::id(), formal.len()));
        std::fs::create_dir(&fixture).unwrap();
        std::fs::write(fixture.join("probe.rb"), &source).unwrap();
        for target in [BuildTarget::Blog, BuildTarget::Ruby] {
            let (files, diagnostics) = roundhouse::emit::diagnostics::scope(|| {
                target_files(&app, &fixture, target)
            });
            let formal_errors: Vec<_> = diagnostics.iter().filter(|d| {
                matches!(&d.kind, DiagnosticKind::Unsupported { construct, .. }
                    if construct.as_str() == "parameter declaration")
            }).collect();
            assert_eq!(formal_errors.len(), usize::from(target == BuildTarget::Ruby), "{target:?}: {diagnostics:?}");
            if target == BuildTarget::Blog {
                let files = files.expect("source archive");
                assert!(files.iter().any(|(path, text)| path == "probe.rb" && text == &source), "{files:?}");
            } else {
                assert_eq!(formal_errors[0].severity, Severity::Error);
            }
        }
        std::fs::remove_dir_all(&fixture).unwrap();
    }
}

#[test]
fn declaration_only_forwarders_are_gated_on_unverified_targets() {
    use roundhouse::project::{BuildTarget, target_files};
    let app = analyzed("class Probe\n def call(...)\n 11\n end\nend");
    for target in [
        BuildTarget::Rust,
        BuildTarget::Typescript,
        BuildTarget::Spinel,
        BuildTarget::Roda,
    ] {
        let (files, diags) = roundhouse::emit::diagnostics::scope(|| {
            target_files(&app, roundhouse::fixtures::real_blog(), target)
        });
        files.expect("project assembly");
        assert!(
            diags
                .iter()
                .any(|d| d.severity == Severity::Error && d.message.contains("forwarding")),
            "{target:?}: {diags:?}"
        );
    }
}

#[test]
fn unpreserved_controller_and_test_entry_declarations_are_rejected() {
    for formal in ["...", "**"] {
        let source = format!("class ProbeController < ApplicationController\n def call({formal})\n 11\n end\nend");
        let controller = roundhouse::ingest::ingest_controller(source.as_bytes(), "probe_controller.rb")
            .expect_err("controller forwarding is outside this slice");
        assert!(controller.to_string().contains("forwarding declaration"));
        for name in ["setup", "test_forwarding"] {
            let source = format!("class ProbeTest < ActiveSupport::TestCase\n def {name}({formal})\n 11\n end\nend");
            let err = roundhouse::ingest::ingest_test_file(source.as_bytes(), "probe_test.rb")
                .expect_err("test entrypoint forwarding is outside this slice");
            assert!(err.to_string().contains("forwarding declaration"));
        }
    }
}

#[test]
fn anonymous_keyword_forwarding_refuses_unverified_keyword_abis() {
    for source in [
        "class Probe; def call(__fwd_kwargs, **); missing(__fwd_kwargs, **); end; end",
        "class Probe; def target(**options); options; end; def call(**); target(**); end; end",
        "class Probe; def target; 7; end; def call(**); target(**); end; end",
    ] {
        let mut app = analyzed(source);
        let lower = roundhouse::session::analyze_and_lower(&mut app);
        let errors: Vec<_> = diagnose(&app).into_iter().chain(lower)
            .filter(|d| d.severity == Severity::Error).collect();
        assert!(errors.iter().any(|d| d.message.contains("keyword forwarding")), "{source}: {errors:?}");
    }
}

#[test]
fn unrepresented_formals_never_admit_a_forwarding_contract() {
    for (source, call, expected) in [
        (
            "class Probe; def target(*); 7; end; def call(...); target(...); end; end",
            "Probe.new.call(1)",
            "7",
        ),
        (
            "class Probe; def target((a,b)); 7; end; def call(...); target(...); end; end",
            "Probe.new.call([11,4])",
            "7",
        ),
        (
            "class Probe; def target(**nil); 7; end; def call(...); target(...); end; end",
            "Probe.new.call",
            "7",
        ),
        (
            "class Probe; def call((a,b),...); target(...); end; def target(first,factor:); first*factor; end; end",
            "Probe.new.call([11,4],7,factor:3)",
            "21",
        ),
    ] {
        let native = Command::new("ruby")
            .args(["-e", &format!("{source}; puts {call}")])
            .output()
            .unwrap();
        assert!(
            native.status.success(),
            "{}",
            String::from_utf8_lossy(&native.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&native.stdout).trim(), expected);
        let mut app = analyzed(source);
        let lowering = roundhouse::session::analyze_and_lower(&mut app);
        let errors: Vec<_> = diagnose(&app)
            .into_iter()
            .chain(lowering)
            .filter(|d| d.severity == Severity::Error)
            .collect();
        assert!(
            errors
                .iter()
                .any(|d| d.message.contains("parameter declaration")),
            "{source}: {errors:?}"
        );
    }
}

#[test]
fn custom_new_is_not_admitted_as_the_initializer_contract() {
    let source = "class Factory; def self.new(factor: 2); factor; end; def initialize; end; end; class Probe; def call(...); Factory.new(...); end; end";
    let native = Command::new("ruby")
        .args(["-e", &format!("{source}; puts Probe.new.call(factor: 3)")])
        .output()
        .unwrap();
    assert!(native.status.success());
    assert_eq!(String::from_utf8_lossy(&native.stdout), "3\n");
    let errors: Vec<_> = diagnose(&analyzed(source))
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(
        errors.iter().any(|d| d.message.contains("flattened")),
        "{errors:?}"
    );
}

#[test]
fn anonymous_keyword_rest_keeps_ordinary_and_forwarded_optional_keywords() {
    let source = "class Probe; def self.scale(n, factor: 2, **); n*factor; end; def self.run; Probe.scale(7, factor: 3); end; def self.call(...); scale(...); end; end";
    let script = "raise unless Probe.run == 21; raise unless Probe.call(7,factor:3) == 21; raise unless Probe.call(7) == 14; puts 'keyword identity passed'";
    let native = Command::new("ruby")
        .args(["-e", &format!("{source}; {script}")])
        .output()
        .unwrap();
    assert!(native.status.success());
    let run = emit_and_run::real_blog()
        .write("app/lib/probe.rb", source)
        .run_ruby(script);
    run.assert_passes();
    assert_eq!(run.stdout, "keyword identity passed\n");
}

#[test]
fn forwarded_operator_and_index_calls_keep_explicit_send_syntax() {
    let source = r#"
class Sink
  def [](first, second, factor:)
    yield((first-second)*factor)
  end
  def ==(first)
    first+2
  end
end
class Probe
  def index(...)
    Sink.new.[](...)
  end
  def equal(...)
    Sink.new.==(...)
  end
end
"#;
    let script = "raise unless Probe.new.index(11,4,factor:3) { |r| r*2+1 } == 43; raise unless Probe.new.equal(11) == 13; puts 'operator forwarding passed'";
    let native = Command::new("ruby")
        .args(["-e", &format!("{source}; {script}")])
        .output()
        .unwrap();
    assert!(native.status.success());
    let run = emit_and_run::real_blog()
        .write("app/lib/probe.rb", source)
        .run_ruby(script);
    run.assert_passes();
    assert_eq!(run.stdout, "operator forwarding passed\n");
}

#[test]
fn model_formal_rejection_survives_body_rewriting() {
    let sink = "class Sink; def target(n,factor:); n*factor; end; end";
    let method = "def relay((a,b),...); helper=Sink.new; helper.target(...); end";
    let mut app = ingest_app_from_tree(HashMap::from([
        (PathBuf::from("app/lib/sink.rb"), sink.as_bytes().to_vec()),
        (
            PathBuf::from("app/models/article.rb"),
            format!("class Article < ApplicationRecord; {method}; end").into_bytes(),
        ),
    ]))
    .unwrap();
    let expected = Some(roundhouse::dialect::UnsupportedFormal::Destructured);
    assert_eq!(
        app.models[0]
            .methods()
            .find(|m| m.name.as_str() == "relay")
            .unwrap()
            .unsupported_formals,
        expected
    );
    roundhouse::session::analyze_and_lower(&mut app);
    let declaration = app.models[0]
        .methods()
        .find(|m| m.name.as_str() == "relay")
        .unwrap();
    assert_eq!(declaration.unsupported_formals, expected);
    assert!(
        declaration.body.diagnostic.is_none(),
        "formal provenance is not a body annotation"
    );
    let (_, emission) = roundhouse::emit::diagnostics::scope(|| {
        roundhouse::project::target_files(
            &app,
            roundhouse::fixtures::real_blog(),
            roundhouse::project::BuildTarget::Ruby,
        )
    });
    assert!(
        emission
            .iter()
            .any(|d| d.severity == Severity::Error && d.message.contains("parameter declaration")),
        "{emission:?}"
    );
    let native = Command::new("ruby")
        .args([
            "-e",
            &format!(
                "{sink}; class Article; {method}; end; puts Article.new.relay([11,4],7,factor:3)"
            ),
        ])
        .output()
        .unwrap();
    assert!(native.status.success());
    assert_eq!(String::from_utf8_lossy(&native.stdout), "21\n");
    let run = emit_and_run::real_blog()
        .write("app/lib/sink.rb", sink)
        .edit(
            "app/models/article.rb",
            "class Article < ApplicationRecord\n",
            &format!("class Article < ApplicationRecord\n {method}\n"),
        )
        .run_ruby("puts Article.new.relay([11,4],7,factor:3)");
    assert!(
        run.errors
            .iter()
            .any(|e| e.contains("parameter declaration")),
        "errors={:?}; actual={}; stderr={}",
        run.errors,
        run.stdout,
        run.stderr
    );
}

#[test]
fn a_class_valued_local_is_not_assumed_to_be_an_instance() {
    let source = "class Sink; def target(factor:); factor; end; def self.target(factor: 2); factor; end; end; class Probe; def call(...); receiver=Sink; receiver.target(...); end; end";
    let script = "puts Probe.new.call(factor:3).inspect";
    let native = Command::new("ruby")
        .args(["-e", &format!("{source}; {script}")])
        .output()
        .unwrap();
    assert!(native.status.success());
    assert_eq!(String::from_utf8_lossy(&native.stdout), "3\n");
    let run = emit_and_run::real_blog()
        .write("app/lib/probe.rb", source)
        .run_ruby(script);
    assert!(
        run.errors.iter().any(|e| e.contains("forwarding")),
        "expected native3; errors={:?}; actual={}; stderr={}",
        run.errors,
        run.stdout,
        run.stderr
    );
}

#[test]
fn keyword_normalization_uses_the_effective_last_class_definition() {
    let source = "class Probe; def self.call(factor: 2); factor; end; def self.call(...); target(...); end; def self.target(factor:); factor; end; def self.run; Probe.call(factor:3); end; end";
    let script = "raise unless Probe.run == 3; puts 'effective definition passed'";
    let native = Command::new("ruby")
        .args(["-e", &format!("{source}; {script}")])
        .output()
        .unwrap();
    assert!(native.status.success());
    let run = emit_and_run::real_blog()
        .write("app/lib/probe.rb", source)
        .run_ruby(script);
    run.assert_passes();
    assert_eq!(run.stdout, "effective definition passed\n");
}

#[test]
fn runtime_source_retains_unrepresented_declaration_facts() {
    use roundhouse::dialect::UnsupportedFormal;
    for (source, expected) in [
        ("def call(*); 7; end", UnsupportedFormal::AnonymousRest),
        ("def call(**nil); 7; end", UnsupportedFormal::NoKeywords),
    ] {
        let methods = roundhouse::runtime_src::parse_methods(source).unwrap();
        assert_eq!(methods[0].unsupported_formals, Some(expected));
    }
}

#[test]
fn source_adapter_and_association_extension_retain_formal_facts() {
    use roundhouse::dialect::UnsupportedFormal;
    let source = roundhouse::ingest::ingest_library_class(
        b"class Probe; def self.call((a,b)); 7; end; end",
        "probe.rb",
    )
    .unwrap()
    .unwrap();
    let functions = roundhouse::lower::view_to_library::flatten_lcs_to_functions(&[source]);
    let restored = roundhouse::lower::module_funcs_to_library_class("Probe", &functions);
    assert_eq!(
        restored.methods[0].unsupported_formals,
        Some(UnsupportedFormal::Destructured)
    );

    let app = ingest_app_from_tree(HashMap::from([
        (PathBuf::from("app/models/article.rb"), b"class Article < ApplicationRecord; has_many :comments do; def unpack((a,b)); 7; end; end; end".to_vec()),
        (PathBuf::from("app/models/comment.rb"), b"class Comment < ApplicationRecord; end".to_vec()),
    ])).unwrap();
    let model = app
        .models
        .iter()
        .find(|m| m.name.0.as_str() == "Article")
        .unwrap();
    let lowered = roundhouse::lower::lower_model_to_library_class(model, &app.schema);
    let method = lowered
        .methods
        .iter()
        .find(|m| m.name.as_str() == "comments_unpack")
        .unwrap();
    assert_eq!(
        method.unsupported_formals,
        Some(UnsupportedFormal::Destructured)
    );
    let (_, emitted) = roundhouse::emit::diagnostics::scope(|| {
        roundhouse::project::target_files(
            &app,
            roundhouse::fixtures::real_blog(),
            roundhouse::project::BuildTarget::Ruby,
        )
    });
    assert!(
        emitted
            .iter()
            .any(|d| d.severity == Severity::Error && d.message.contains("parameter declaration")),
        "{emitted:?}"
    );
}

fn native_result(source: &str, script: &str, expected: &str) {
    let output = Command::new("ruby")
        .args(["-e", &format!("{source}; {script}")])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), expected);
}

#[test]
fn anonymous_block_and_virtual_destination_losses_are_refused() {
    for (source, script, expected, refusal) in [
        (
            "class Probe; def self.leaf; yield; end; def self.target(&); __blk=7; leaf(&); end; def self.call(...); target(...); end; end",
            "puts Probe.call {23}",
            "23\n",
            "anonymous block",
        ),
        (
            "class Parent; def call(...); target(...); end; def target(a,b,factor:); (a-b)*factor; end; end; class Child < Parent; def target(a,b,factor:2); (a-b)*factor; end; end",
            "puts Child.new.call(11,4,factor:3)",
            "21\n",
            "subclass contract",
        ),
        (
            "module Relay; def call(...); target(...); end; def target(a,b,factor:); (a-b)*factor; end; end; class Child; include Relay; def target(a,b,factor:2); (a-b)*factor; end; end",
            "puts Child.new.call(11,4,factor:3)",
            "21\n",
            "subclass contract",
        ),
    ] {
        native_result(source, script, expected);
        let run = emit_and_run::real_blog()
            .write("app/lib/probe.rb", source)
            .run_ruby(script);
        assert!(
            run.errors.iter().any(|e| e.contains(refusal)),
            "{refusal}: {:?}; actual={}; stderr={}",
            run.errors,
            run.stdout,
            run.stderr
        );
    }
}

#[test]
fn anonymous_model_block_destination_stays_refused_after_parameter_retention() {
    let methods = "def self.leaf; yield; end; def self.target(&); __blk=7; leaf(&); end; def self.call(...); target(...); end";
    let script = "puts Article.call {23}";
    native_result(&format!("class Article; {methods}; end"), script, "23\n");
    let run = emit_and_run::real_blog()
        .edit(
            "app/models/article.rb",
            "class Article < ApplicationRecord\n",
            &format!("class Article < ApplicationRecord\n{methods}\n"),
        )
        .run_ruby(script);
    assert!(
        run.errors.iter().any(|e| e.contains("anonymous block")),
        "errors={:?}; actual={}; stderr={}",
        run.errors,
        run.stdout,
        run.stderr
    );
}

#[test]
fn named_block_spelling_and_no_forwarding_anonymous_block_keep_legacy_behavior() {
    let source = "class Probe; def self.leaf; yield; end; def self.named(&__blk); leaf(&__blk); end; def self.call(...); named(...); end; def self.ordinary(&); leaf(&); end; end";
    let script = "puts Probe.call {23}; puts Probe.ordinary {43}";
    native_result(source, script, "23\n43\n");
    let run = emit_and_run::real_blog()
        .write("app/lib/probe.rb", source)
        .run_ruby(script);
    run.assert_passes();
    assert_eq!(run.stdout, "23\n43\n");
}

#[test]
fn anonymous_block_source_fact_survives_adapters_extensions_and_runtime_ingest() {
    let class = roundhouse::ingest::ingest_library_class(
        b"class Probe; def self.call(&); 7; end; end",
        "probe.rb",
    )
    .unwrap()
    .unwrap();
    assert!(class.methods[0].has_anonymous_block);
    let functions = roundhouse::lower::view_to_library::flatten_lcs_to_functions(&[class]);
    assert!(functions[0].has_anonymous_block);
    let restored = roundhouse::lower::module_funcs_to_library_class("Probe", &functions);
    assert!(restored.methods[0].has_anonymous_block);
    let model = roundhouse::ingest::ingest_model(
        b"class Article < ApplicationRecord; has_many :comments do; def call(&); 7; end; end; end",
        "article.rb",
        &Default::default(),
        &Default::default(),
    )
    .unwrap()
    .unwrap();
    let lowered = roundhouse::lower::lower_model_to_library_class(&model, &Default::default());
    assert!(
        lowered
            .methods
            .iter()
            .find(|m| m.name.as_str() == "comments_call")
            .unwrap()
            .has_anonymous_block
    );
    assert!(
        roundhouse::runtime_src::parse_methods("def call(&); 7; end").unwrap()[0]
            .has_anonymous_block
    );
    assert!(
        !roundhouse::runtime_src::parse_methods("def call(&__blk); 7; end").unwrap()[0]
            .has_anonymous_block
    );
}

#[test]
fn association_lexical_contract_is_not_guessed_from_the_model_owner() {
    native_result(
        "module Extension; def target(a,b,*tail); a-b; end; def call(...); target(...); end; end; class Proxy; include Extension; end",
        "puts Proxy.new.call(11,4,9)",
        "7\n",
    );
    let methods = "def target(...); 695; end; has_many :comments do; def target(a,b,*tail); a-b; end; def call(...); target(...); end; end";
    let run = emit_and_run::real_blog()
        .edit(
            "app/models/article.rb",
            "class Article < ApplicationRecord\n",
            &format!("class Article < ApplicationRecord\n{methods}\n"),
        )
        .run_ruby("puts Article.new.comments_call(11,4,9)");
    assert!(
        run.errors
            .iter()
            .any(|e| e.contains("declaration cannot be verified")),
        "{:?}",
        run.errors
    );
}

#[test]
fn runtime_full_declarations_refuse_both_entry_paths_and_project_legacy_keywords() {
    assert!(
        roundhouse::runtime_src::parse_methods("def call(...); 7; end")
            .unwrap_err()
            .contains("full forwarding")
    );
    assert!(
        roundhouse::runtime_src::parse_library_with_rbs(
            b"class Probe; def call(...); 7; end; end",
            "class Probe\n def call: (*untyped) -> Integer\nend",
            "probe.rb"
        )
        .unwrap_err()
        .contains("full forwarding")
    );
    let method = roundhouse::runtime_src::parse_methods("def call(kw); target(**kw); end")
        .unwrap()
        .remove(0);
    let emitted = roundhouse::emit::ruby::emit_method(&method);
    assert!(emitted.contains("target(kw)"), "{emitted}");
    assert!(!emitted.contains("**kw"), "{emitted}");
}

/// The ordinary `**value` lowering passes the hash positionally, so a
/// splat that can reach an admitted nameless rest is refused off the
/// CRuby tree (whose emitter writes the splat back), before lowering,
/// for a send and for a `super`. `check` itself no longer refuses it.
#[test]
fn a_keyword_splat_into_a_nameless_rest_is_refused_before_lowering() {
    for source in [
        "class Probe; def m(*, **kw); kw; end; def go(h); m(**h); end; end",
        "class Probe; def m(*); 7; end; end; class Kid < Probe; def m(**h); super(**h); end; end",
        "class Probe; def m(*); 7; end; def go(h); [1, m(**h)]; end; end",
    ] {
        let app = analyzed(source);
        let errors: Vec<_> = nameless_rest_refusals_off_ruby(&app).into_iter().filter(|d| d.severity == Severity::Error).collect();
        assert!(
            errors.iter().any(|d| d.message.contains("keyword splat into an anonymous positional rest")),
            "{source}: {errors:?}"
        );
        assert!(
            !diagnose(&app).iter().any(|d| d.message.contains("keyword splat into an anonymous positional rest")),
            "{source}: the CRuby tree writes the splat back"
        );
    }
}

/// Another class's `initialize(*)` does not make every `new(**h)` reach
/// a rest: a constant naming a source class whose effective destination,
/// and every override a self-send reaches, takes none is admitted. An
/// unproven receiver, a rest destination, or a rest override is not.
#[test]
fn a_keyword_splat_whose_exact_destination_has_no_nameless_rest_is_admitted() {
    const REST: &str = "class Loose; def initialize(*); end; end; ";
    let refused = |source: &str| {
        let app = analyzed(source);
        nameless_rest_refusals_off_ruby(&app)
            .into_iter()
            .any(|d| d.severity == Severity::Error && d.message.contains("keyword splat into an anonymous positional rest"))
    };
    for source in [
        "class Point; def initialize(x:, y: 0); @x = x; end; end; class Maker; def go(h); Point.new(**h); end; end",
        "class Base; def initialize(x:); @x = x; end; end; class Point < Base; end; class Maker; def go(h); Point.new(**h); end; end",
        "class Base; def m(a: 1); a; end; def go(h); m(**h); end; end",
    ] {
        assert!(!refused(&format!("{REST}{source}")), "{source}");
    }
    for source in [
        "class Maker; def go(h); Loose.new(**h); end; end",
        "class Maker; def go(k, h); k.new(**h); end; end",
        "class Base; def m(a: 1); a; end; def go(h); m(**h); end; end; class Kid < Base; def m(*); 7; end; end",
        "class Point < Loose; end; class Maker; def go(h); Point.new(**h); end; end",
    ] {
        assert!(refused(&format!("{REST}{source}")), "{source}");
    }
}

/// A receiver constant whose Ruby method lookup cannot reach any rest
/// owner of the selector is admitted even when its destination is an
/// unmodeled ancestor (Core's `BuildEventContext < Operation`, which
/// takes `initialize` from the `SmartProperties` gem). Reachable owners
/// stay refused: one in the receiver's superclass chain, any module
/// (mixins are also applied by calls), `Object`/`Kernel`, a reopened
/// owner, a class-side `new`, and, past an unmodeled superclass, an
/// owner gem code can name (installed gem source, a core class).
#[test]
fn a_keyword_splat_whose_receiver_lookup_cannot_reach_a_nameless_rest_is_admitted() {
    // Native controls: the unrelated rest is not reached; an owner mixed
    // in by a call, which no class body shows, is.
    native_result(
        "class Loose; def initialize(*a); @a = a; end; end; module SmartProperties; def initialize(**kw); @kw = kw; end; end; class Operation; include SmartProperties; end; module AccessAndAuth; class BuildEventContext < Operation; end; end",
        "p AccessAndAuth::BuildEventContext.new(**{}).instance_variable_get(:@kw)",
        "{}\n",
    );
    native_result(
        "module Rest; def initialize(*a); @a = a; end; end; class Point; end; Point.include(Rest)",
        "p Point.new(**{}).instance_variable_get(:@a)",
        "[]\n",
    );
    const REST: &str = "class Loose; def initialize(*, **kw); @kw = kw; end; end; ";
    const GEM: &str = "class GemError < StandardError; def initialize(*); super('gem'); end; end";
    let refused = |source: &str, vendored: Option<&str>| {
        let mut tree = HashMap::from([(PathBuf::from("app/lib/probe.rb"), format!("{REST}{source}").into_bytes())]);
        if let Some(gem) = vendored {
            tree.insert(PathBuf::from("app/lib/vendor/gems/kit-1.0.0/gem_error.rb"), gem.as_bytes().to_vec());
        }
        let mut app = ingest_app_from_tree(tree).expect("ingest synthetic forwarding source");
        Analyzer::new(&app).analyze(&mut app);
        nameless_rest_refusals_off_ruby(&app)
            .into_iter()
            .any(|d| d.severity == Severity::Error && d.message.contains("keyword splat into an anonymous positional rest"))
    };
    const MAKER: &str = "class Maker; def go(h); AccessAndAuth::BuildEventContext.new(request_schema: 1, **h); end; end";
    for (source, vendored) in [
        (format!("module AccessAndAuth; class BuildEventContext < Operation; end; end; {MAKER}"), None),
        (format!("class Operation; include SmartProperties; end; module AccessAndAuth; class BuildEventContext < Operation; end; end; {MAKER}"), None),
        ("class Other; def self.make(*); end; end; class Maker < ExternalBase; end; class Caller; def go(h); Maker.make(**h); end; end".to_string(), None),
        ("class Point; end; class Maker; def go(h); Point.new(**h); end; end".to_string(), Some(GEM)),
    ] {
        assert!(!refused(&source, vendored), "{source}");
    }
    for (source, vendored) in [
        ("class Kid < Loose; end; class Maker; def go(h); Kid.new(**h); end; end", None),
        ("class Mid < Loose; end; class Kid < Mid; end; class Maker; def go(h); Kid.new(**h); end; end", None),
        ("module A; class Base; def initialize(*); end; end; class Kid < Base; end; end; class Maker; def go(h); A::Kid.new(**h); end; end", None),
        ("module Rest; def initialize(*); end; end; class Point; end; class Maker; def go(h); Point.new(**h); end; end", None),
        ("module Rest; def initialize(*); end; end; class Point; include Rest; end; class Maker; def go(h); Point.new(**h); end; end", None),
        ("module Factory; def new(*); super(); end; end; class Point; extend Factory; end; class Maker; def go(h); Point.new(**h); end; end", None),
        ("class Object; def initialize(*); end; end; class Point; end; class Maker; def go(h); Point.new(**h); end; end", None),
        ("module Kernel; def initialize(*); end; end; class Point; end; class Maker; def go(h); Point.new(**h); end; end", None),
        ("class Loose; def other; end; end; class Point < Operation; end; class Maker; def go(h); Point.new(**h); end; end", None),
        ("class Base; def self.new(*); super(); end; end; class Kid < Base; end; class Maker; def go(h); Kid.new(**h); end; end", None),
        ("class Point < External; end; class Maker; def go(h); Point.new(**h); end; end", Some(GEM)),
        ("class StandardError; def initialize(*); end; end; class Failure < RuntimeError; end; class Maker; def go(h); Failure.new(**h); end; end", None),
        ("class Holder; Base = Builder.make; end; class Kid < Holder::Base; end; class Maker; def go(h); Kid.new(**h); end; end", None),
        ("class Point; end; class Maker; def go(h); Point.send(:new, **h); end; end", None),
    ] {
        assert!(refused(source, vendored), "{source}");
    }
}

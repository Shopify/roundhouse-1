//! Regression cases where inference used to certify an unexecutable program.
//! Both CLI gates must refuse; passing `check` alone is not admission evidence.
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn refuses(body: &str, expected: &str) {
    refuses_with(body, expected, &[]);
}

fn refuses_with(body: &str, expected: &str, extra: &[(&str, &str)]) {
    gate(body, expected, extra, false);
}

fn admits(body: &str) {
    gate(body, "", &[], true);
}

fn gate(body: &str, expected: &str, extra: &[(&str, &str)], admitted: bool) {
    let root = std::env::temp_dir().join(format!("rh_admission_{}_{}", std::process::id(), NEXT.fetch_add(1, Ordering::SeqCst)));
    for (path, text) in [
        ("app/controllers/application_controller.rb", "class ApplicationController < ActionController::Base\nend\n".to_string()),
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n".to_string()),
        ("db/schema.rb", "ActiveRecord::Schema.define do\n  create_table \"probes\" do |t|\n    t.string \"title\"\n  end\nend\n".to_string()),
        ("app/models/probe.rb", format!("class Probe < ApplicationRecord\n  validates :title, presence: true\n  def probe\n    {body}\n  end\nend\n")),
    ] {
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    for (path, text) in extra {
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    for args in [vec!["check"], vec!["--target", "ruby"]] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_roundhouse"));
        command.args(&args).arg(&root);
        if args[0] != "check" { command.arg("-o").arg(root.join("emitted")); }
        let output = command.output().unwrap();
        let text = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        if admitted {
            assert!(output.status.success(), "{args:?} refused {body}: {text}");
            continue;
        }
        assert!(!output.status.success(), "{args:?} accepted {body}: {text}");
        assert!(text.contains(expected), "{args:?} lost named refusal {expected}: {text}");
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn module_protocol_requires_a_module_object() {
    for body in ["self.define_method(:x) { 1 }", "define_method(:x) { 1 }", "class_eval { 2 }", "\"abc\".define_method(:x) { 1 }", "1.class_eval { 2 }", "self.const_get(:X)", "self.include?(1)"] {
        refuses(body, "Module protocol");
    }
}

#[test]
fn unresolved_declared_classes_are_errors() {
    refuses("v = T.let(1, GhostMoney); v.imaginary_method", "GhostMoney");
    refuses("v = T.let(1, GhostMoney); v.message.upcase", "GhostMoney");
}

#[test]
fn new_errors_api_requires_runtime_support() {
    // `added?`, `details`, `where`, `merge!`, `Error#options` and `delete`
    // have a runtime port now (runtime/spinel/active_model_errors.rb); the
    // rest of activemodel's errors.rb still has none and is refused by name.
    for body in ["errors.slice!(:title)", "errors.group_by_attribute", "errors.copy!(errors)", "errors.generate_message(:title, :blank)", "f = title.to_sym; errors.full_message(f, \"is taken\")"] {
        refuses(body, "ActiveModel::Errors");
    }
    // Not `ActiveModel::Error`: on emit, what an iteration yields is already the `ErrorDetail` the lowering iterates.
    refuses("errors.first.options", "no known method `options`");
    refuses("errors.of_kind?(title.to_sym, :blank)", "of_kind?");
    refuses("errors.details[:title].map { |detail| detail[:count] }", "no known method `[]`");
}

#[test]
fn errors_merge_details_and_added_are_admitted() {
    admits("errors.merge!(errors); errors.details[:title].map { |detail| detail[:error] }; errors.added?(:title, :blank)");
}

#[test]
fn errors_where_on_a_literal_field_is_admitted() {
    admits("errors.where(:title, :blank).map(&:full_message)");
}

#[test]
fn errors_full_message_on_a_literal_field_is_admitted() {
    admits("errors.full_message(:title, \"is taken\").upcase");
    admits("other = Probe.new; other.errors.full_message(:base, title.to_s)");
}

#[test]
fn errors_full_message_on_a_computed_receiver_is_admitted() {
    admits("Probe.create!(title: \"x\").errors.full_message(:title, \"is taken\").upcase");
}

#[test]
fn non_nil_assertions_require_runtime_support() {
    // `T.must` and `T.must_because` with a plain reason block lower to
    // sorbet's check (tests/non_nil_assertions_run.rs) and `x.not_nil!`
    // runs on the CRuby tree (tests/not_nil_type_toolkit.rs). A reason
    // block with parameters has no lowering.
    refuses("T.must_because(1) { |x| \"present\" }", "non-nil assertion");
}

/// The nil test the three non-nil assertions lower to (sorbet's raising
/// check for the two calls, an unchecked test that keeps the typer's
/// narrowing for `#: as !nil`) runs on the Ruby-family trees
/// (tests/non_nil_assertions_run.rs runs it). Other targets refuse each
/// site by name, rather than emitting a test that does not run there:
/// TypeScript wrote the generated local undeclared and threw a plain
/// Error where sorbet raises TypeError.
#[test]
fn non_nil_assertion_checks_are_refused_where_the_tree_cannot_run_them() {
    for body in [
        "title #: as !nil",
        "Probe.find_by(id: 1) #: as !nil",
        "T.must(Probe.find_by(id: 1))",
        "T.must_because(Probe.find_by(id: 1)) { \"a probe\" }",
    ] {
        let root = probe_app(body);
        let (ok, text) = run(&root, &["check"]);
        assert!(ok, "check refused {body}: {text}");
        let (ok, text) = run(&root, &["--target", "ruby"]);
        assert!(ok, "ruby refused {body}: {text}");
        for target in ["typescript", "rust"] {
            let (ok, text) = run(&root, &["--target", target]);
            assert!(!ok, "{target} accepted {body}: {text}");
            assert!(
                text.contains("non-nil assertion not supported") && text.contains(&format!("Ruby-family trees, not {target}")),
                "{target} lost named refusal for {body}: {text}"
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}

/// sorbet's class check of `T.let` / `T.cast` / `T.bind` /
/// `T.assert_type!` runs on the CRuby and JRuby trees, through the
/// overlay's SorbetRuntimeCheck (tests/author_casts_run.rs runs it).
/// Other targets refuse each site by name instead of dropping the check.
#[test]
fn sorbet_type_checks_are_refused_where_the_tree_cannot_run_them() {
    for body in [
        "T.let(title, String)",
        "T.cast(Probe.find_by(id: 1), T.nilable(Probe))",
        "T.assert_type!(title, T.any(String, Integer))",
        "T.let(Probe.all.to_a, T::Array[Probe])",
    ] {
        let root = probe_app(body);
        let (ok, text) = run(&root, &["check"]);
        assert!(ok, "check refused {body}: {text}");
        let (ok, text) = run(&root, &["--target", "ruby"]);
        assert!(ok, "ruby refused {body}: {text}");
        for target in ["typescript", "rust"] {
            let (ok, text) = run(&root, &["--target", target]);
            assert!(!ok, "{target} accepted {body}: {text}");
            assert!(
                text.contains("sorbet type check not supported") && text.contains(&format!("CRuby and JRuby trees, not {target}")),
                "{target} lost named refusal for {body}: {text}"
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }
    // A check that cannot fail is the value alone, on every target.
    let root = probe_app("T.let(nil, T.nilable(String))");
    for target in ["ruby", "typescript"] {
        let (ok, text) = run(&root, &["--target", target]);
        assert!(ok, "{target} refused a check that cannot fail: {text}");
    }
    std::fs::remove_dir_all(root).unwrap();
}

/// A sorbet type alias is the type object the Ruby trees' check reads
/// (`Title = SorbetRuntimeCheck.type_alias { String }`), and its static
/// type is the type it names. Every other tree drops it with the checks
/// that could read it: one no check reads must not stop that tree.
#[test]
fn a_type_alias_is_carried_where_its_check_runs() {
    let alias_app = |body: &str| {
        let root = probe_app(body);
        let model = root.join("app/models/probe.rb");
        let text = std::fs::read_to_string(&model).unwrap();
        std::fs::write(&model, text.replacen("  validates", "  Title = T.type_alias { T.nilable(String) }\n  validates", 1)).unwrap();
        root
    };
    let root = alias_app("T.let(title, Title).to_s.length");
    let (ok, text) = run(&root, &["check"]);
    assert!(ok, "check refused the alias-typed check: {text}");
    let (ok, text) = run(&root, &["--target", "ruby"]);
    assert!(ok, "ruby refused the alias-typed check: {text}");
    let emitted = walk(&root.join("emitted-ruby"));
    assert!(emitted.contains("Title = SorbetRuntimeCheck.type_alias { SorbetRuntimeCheck.nilable(String) }"), "{emitted}");
    assert!(emitted.contains("SorbetRuntimeCheck.cast(title, Probe::Title, \"T.let\""), "{emitted}");
    let (ok, text) = run(&root, &["--target", "typescript"]);
    assert!(!ok && text.contains("sorbet type check not supported"), "typescript lost the named refusal: {text}");
    std::fs::remove_dir_all(root).unwrap();
    // No check reads it: the tree that cannot carry it emits without it.
    let root = alias_app("T.let(nil, T.nilable(Title))");
    for target in ["ruby", "typescript"] {
        let (ok, text) = run(&root, &["--target", target]);
        assert!(ok, "{target} refused an alias no check reads: {text}");
    }
    let emitted = walk(&root.join("emitted-typescript"));
    assert!(!emitted.contains("type_alias") && !emitted.contains("SorbetRuntimeCheck"), "{emitted}");
    std::fs::remove_dir_all(root).unwrap();
}

/// Every file under `dir` but source maps (which embed the authored
/// Ruby), concatenated.
fn walk(dir: &std::path::Path) -> String {
    let mut out = String::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                if path.file_name().is_some_and(|n| n != "node_modules") {
                    stack.push(path);
                }
            } else if path.extension().is_some_and(|e| e == "map") {
            } else if let Ok(text) = std::fs::read_to_string(&path) {
                out.push_str(&text);
            }
        }
    }
    out
}

/// A type form the check cannot rebuild is refused at the site by name,
/// on `check` and on the Ruby tree alike.
#[test]
fn uncheckable_sorbet_types_are_refused_by_name() {
    refuses("T.let(title, T.proc.params(x: Integer))", "is a proc type without `returns` or `void`");
    refuses("T.let(title, [String, *[Integer]])", "splats an array into a tuple shape");
    refuses("T.cast(title, T.self_type)", "T.self_type");
}

#[test]
fn for_scope_is_the_enclosing_scope() {
    // `for` is lowered as `each` over the enclosing scope's locals
    // (tests/for_loop_cruby.rs runs it against CRuby), so the loop
    // variables and body locals read after the loop are admitted.
    for body in [
        "x = 10; for x in [1, 2]; end; x",
        "for x in []; end; [defined?(x), x]",
        "for x in [1, 2]; y = x * 10; end; y",
        "for p, q in [[1, 2], [3, 4]]; end; [p, q]",
        "for x in []; y = 1; end; [defined?(y), y]",
    ] {
        let root = probe_app(body);
        let (ok, text) = run(&root, &["check"]);
        assert!(ok, "check refused {body}: {text}");
        let (ok, text) = run(&root, &["--target", "ruby"]);
        assert!(ok, "ruby refused {body}: {text}");
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn registry_signatures_do_not_supply_missing_runtime() {
    for (body, name) in [
        ("ActiveRecord::Base.connected_to(role: :reading) { 7 }", "connected_to"),
        ("Benchmark.realtime { 1 }", "Benchmark"),
        ("\"2020-01-02\".to_date", "to_date"),
        ("\"1.5\".to_d", "to_d"),
        ("{}.to_sentence", "to_sentence"),
    ] { refuses(body, name); }
}

/// The probe app, written to a fresh directory.
fn probe_app(body: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("rh_admission_{}_{}", std::process::id(), NEXT.fetch_add(1, Ordering::SeqCst)));
    for (path, text) in [
        ("app/controllers/application_controller.rb", "class ApplicationController < ActionController::Base\nend\n".to_string()),
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n".to_string()),
        ("db/schema.rb", "ActiveRecord::Schema.define do\n  create_table \"probes\" do |t|\n    t.string \"title\"\n  end\nend\n".to_string()),
        ("app/models/probe.rb", format!("class Probe < ApplicationRecord\n  validates :title, presence: true\n  def probe\n    {body}\n  end\nend\n")),
    ] {
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    root
}

fn run(root: &std::path::Path, args: &[&str]) -> (bool, String) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_roundhouse"));
    command.args(args).arg(root);
    if args[0] != "check" { command.arg("-o").arg(root.join(format!("emitted-{}", args[1]))); }
    let output = command.output().unwrap();
    (output.status.success(), format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr)))
}

/// `Rails.event` / `Rails.error` answer the ported EventReporter and
/// ErrorReporter, which only the Ruby-family trees carry
/// (runtime/ruby/active_support_reporters.rb). Target-independent
/// `check` accepts them: a clean `check` does not imply availability on
/// every target (docs/guide/transpile.md). The ruby tree carries the
/// file and emits; a target without it refuses the read by name. That
/// the ruby tree's reporters run as activesupport's do is
/// tests/reporters_runtime.rs; that analysis resolves them is
/// tests/rails_reporters_and_process_clock.rs.
#[test]
fn ruby_family_reporters_are_refused_where_the_runtime_lacks_them() {
    for (body, shown) in [
        ("Rails.event.notify(\"probe\")", "`Rails.event` answers ActiveSupport::EventReporter"),
        ("Rails.error.set_context(probe: true)", "`Rails.error` answers ActiveSupport::ErrorReporter"),
    ] {
        let root = probe_app(body);
        let (ok, text) = run(&root, &["check"]);
        assert!(ok, "check refused {body}: {text}");
        let (ok, text) = run(&root, &["--target", "ruby"]);
        assert!(ok, "ruby refused {body}: {text}");
        assert!(root.join("emitted-ruby/runtime/active_support_reporters.rb").is_file(), "ruby tree lacks the reporters for {body}");
        for target in ["typescript", "rust"] {
            let (ok, text) = run(&root, &["--target", target]);
            assert!(!ok, "{target} accepted {body}: {text}");
            assert!(text.contains("runtime_constant") && text.contains(shown), "{target} lost named refusal {shown}: {text}");
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}

/// `Rails.configuration` is the CRuby/JRuby trees' object
/// (runtime/spinel/rails_configuration.rb), answering the config keys
/// ingest lifted. A read no lift rewrote reaches that object, so a tree
/// without the file refuses it by name, as it does `Rails.event`;
/// spinel's tree is one of them.
#[test]
fn rails_configuration_is_refused_where_the_runtime_lacks_it() {
    let body = "Rails.configuration.probe_setting";
    let shown = "`Rails.configuration` answers Rails::Configuration";
    let root = probe_app(body);
    let (ok, text) = run(&root, &["check"]);
    assert!(ok, "check refused {body}: {text}");
    let (ok, text) = run(&root, &["--target", "ruby"]);
    assert!(ok, "ruby refused {body}: {text}");
    assert!(root.join("emitted-ruby/runtime/rails_configuration.rb").is_file(), "ruby tree lacks Rails.configuration");
    for target in ["spinel", "rust"] {
        let (ok, text) = run(&root, &["--target", target]);
        assert!(!ok, "{target} accepted {body}: {text}");
        assert!(text.contains("runtime_constant") && text.contains(shown), "{target} lost named refusal {shown}: {text}");
    }
    std::fs::remove_dir_all(root).unwrap();
}

/// A class built on a ported ActiveModel/ActiveRecord type
/// (runtime/spinel/active_model_type.rb), the way Core's
/// Shopify::NormalizedStringType subclasses `ActiveRecord::Type::String`
/// and includes `ActiveRecord::Type::QueryPredicates`. The ruby tree
/// carries the port and emits the class; a target without the port
/// refuses the superclass and the include by name rather than emitting a
/// class whose ancestors do not exist.
#[test]
fn runtime_port_ancestors_are_refused_where_the_runtime_lacks_them() {
    let root = probe_app("NormalizedProbeType.new.cast(title)");
    let file = root.join("app/models/normalized_probe_type.rb");
    std::fs::write(&file, "class NormalizedProbeType < ActiveRecord::Type::String\n  include ActiveRecord::Type::QueryPredicates\n\n  def comparison_expression(expression)\n    expression\n  end\nend\n").unwrap();
    let (ok, text) = run(&root, &["check"]);
    assert!(ok, "check refused the port subclass: {text}");
    let (ok, text) = run(&root, &["--target", "ruby"]);
    assert!(ok, "ruby refused the port subclass: {text}");
    assert!(root.join("emitted-ruby/runtime/active_model_type.rb").is_file(), "ruby tree lacks the type port");
    let emitted = std::fs::read_to_string(root.join("emitted-ruby/app/models/normalized_probe_type.rb")).unwrap();
    assert!(!emitted.contains("not supported"), "ruby tree refuses inside the class: {emitted}");
    for target in ["typescript", "rust"] {
        let (ok, text) = run(&root, &["--target", target]);
        assert!(!ok, "{target} accepted the port subclass: {text}");
        for shown in ["NormalizedProbeType < ActiveRecord::Type::String", "NormalizedProbeType includes ActiveRecord::Type::QueryPredicates"] {
            assert!(text.contains("runtime_constant") && text.contains(shown), "{target} lost named refusal {shown}: {text}");
        }
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn application_tracers_are_not_global_stdlib() {
    refuses("ShopifyTracer.in_span(\"probe\") { 42 }", "ShopifyTracer");
}

#[test]
fn a_nominal_class_does_not_prove_operator_ownership() {
    refuses_with("Value.new + 1", "incompatible_binop", &[("app/lib/value.rb", "class Value\nend\n")]);
}

#[test]
fn unresolved_includes_are_load_time_errors() {
    refuses_with("Imported.new.zork", "GhostModule", &[("app/lib/imported.rb", "class Imported\n  include GhostModule\n  def zork; 1; end\nend\n")]);
}

#[test]
fn identity_is_invalidated_by_reassignment_and_branch_writes() {
    for body in [
        "k = Probe; k = Probe.new; k.define_method(:x) { 1 }",
        "k = Probe; if title; k = Probe.new; end; k.define_method(:x) { 1 }",
        "k = Probe; [1].each { |k| k.define_method(:x) { 1 } }",
        "k = Probe; [1].each { |*k| k.define_method(:x) { 1 } }",
        "k = Probe; k, x = [Probe.new, 1]; k.define_method(:x) { 1 }",
        "k = Probe; [Probe.new] => [k]; k.define_method(:x) { 1 }",
        "k = Probe; begin; raise StandardError; rescue => k; k.define_method(:x) { 1 }; end",
        "Probe.new.class.define_method(:x) { 1 }",
    ] { refuses(body, "Module protocol"); }
}

#[test]
fn cross_date_time_comparison_requires_activesupport_runtime() {
    refuses("Date.new(2020, 1, 1) < Time.utc(2020, 1, 2)", "incompatible_binop");
}

#[test]
fn instance_eval_changes_self_identity_in_class_side_methods() {
    refuses_with("Probe.go", "Module protocol", &[("app/models/probe.rb", "class Probe < ApplicationRecord\n  def self.go\n    Probe.new.instance_eval { self.define_method(:x) { 1 } }\n  end\nend\n")]);
}

#[test]
fn unused_unresolved_model_include_is_not_silent() {
    refuses_with("1", "GhostModule", &[("app/models/probe.rb", "class Probe < ApplicationRecord\n  include GhostModule\nend\n")]);
}

#[test]
fn object_extensions_without_runtime_are_refused() {
    // `to_param` on an AR model is synthesized + registered; refuse it on
    // a plain library object that has no runtime/lowering support.
    for body in ["self.to_query", "self.instance_values", "self.acts_like?(:probe)", "self.presence_in([1])"] {
        refuses(body, "Object extension");
    }
    refuses_with(
        "Value.new.to_param",
        "Object extension",
        &[("app/lib/value.rb", "class Value\nend\n")],
    );
}

#[test]
fn lambda_parameters_cannot_borrow_outer_class_identity() {
    refuses("k = Probe; f = ->(k) { k.define_method(:x) { 1 } }; f.call(Probe.new)", "Module protocol");
}

#[test]
fn dropped_block_capture_cannot_borrow_outer_identity() {
    // The capture is kept and shadows the outer `k`: the block's `k` is
    // whatever block the call passes, not the class `Probe`.
    let identity = "define_method requires a proven class/module object receiver";
    refuses("k = Probe; [1].each { |&k| k.define_method(:x) { 1 } }", identity);
    refuses("k = Probe; f = ->(&k) { k.define_method(:x) { 1 } }; f.call", identity);
}

#[test]
fn framework_prefix_does_not_prove_an_include_exists() {
    refuses_with("1", "ActiveModel::Ghost", &[("app/models/probe.rb", "class Probe < ApplicationRecord\n  include ActiveModel::Ghost\nend\n")]);
}

#[test]
fn native_module_callback_contract_is_not_an_instance_escape() {
    refuses_with("Hook.included(Probe.new)", "Module callback argument", &[("app/helpers/hook.rb", "module Hook\n  def self.included(base)\n    base.define_method(:hook_value) { 23 }\n  end\nend\n")]);
}

#[test]
fn library_nominal_operator_failures_are_not_hidden() {
    refuses_with("1", "incompatible_binop", &[("app/helpers/value.rb", "class Value\n  def bad\n    Value.new + 1\n  end\nend\n")]);
    for value_definition in ["class Value\nend\n", "class Value\n  def +(other); other; end\nend\n"] {
        let library = format!("{value_definition}class Other\nend\nclass UnionOperator\n  def bad(flag)\n    value = flag ? Value.new : Other.new\n    value + 1\n  end\nend\n");
        refuses_with("1", "incompatible_binop", &[("app/helpers/value.rb", &library)]);
    }
}

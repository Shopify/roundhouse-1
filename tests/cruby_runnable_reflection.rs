//! Ruby that CRuby runs is not refused on the CRuby tree.
//!
//! Three shapes from Core's unit-1000 round 3 emit, each a raise stub (or
//! worse, a dropped statement) in the emitted tree:
//!
//! - `model.extend(M) unless model < M` (row-level access
//!   `FilterManager#register_filter`): the Object#extend stub inside the
//!   `unless` read as an empty branch, so the emit was `nil if model < M`
//!   with the raise gone. A PostgreSQL app's tree is CRuby's, which has
//!   every object's singleton class, so the call is emitted as written;
//!   elsewhere the stub keeps its raise in the branch.
//! - `def self.extended(base) base.class_eval do def _example? ... end end`
//!   (customers' `ExampleCustomer`, extended by `Customer`): "Module
//!   protocol not supported", raised from `Customer`'s class body. CRuby
//!   answers Module protocol on any receiver itself.
//! - `T.let(->(_job) { false }, T.proc.params(job: Hedwig::Job).returns(
//!   T::Boolean))` (hedwig's configuration): "sorbet type check not
//!   supported". sorbet-runtime's `T::Types::Proc#valid?` is
//!   `obj.is_a?(::Proc)`, which is what the tree now checks.
//!
//! The expected lines are what CRuby with Core's sorbet-runtime 0.6.13502
//! prints for the same sources (CORE_GEM_HOME), so two identical wrong
//! answers still fail.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::path::Path;
use std::process::Command;

use roundhouse::project::BuildTarget;

const FILTERING: &str = r##"module ReflectionProbe
  module Filtering
    def filtered?
      true
    end
  end
end
"##;

const REGISTRY: &str = r##"module ReflectionProbe
  class Registry
    def self.register(klass)
      klass.extend(Filtering) unless klass < Filtering
      klass.filtered?
    end
  end
end
"##;

const EXAMPLE: &str = r##"module ReflectionProbe
  module Example
    class << self
      def extended(base)
        base.class_eval do
          def example?
            true
          end
        end
      end
    end

    def example
      new
    end
  end
end
"##;

const CUSTOMER: &str = "module ReflectionProbe\n  class Customer\n    extend Example\n  end\nend\n";

const CONFIGURATION: &str = r##"module ReflectionProbe
  class Configuration
    def initialize
      @predicate = T.let(->(j) { false }, T.proc.params(j: Integer).returns(T::Boolean))
      @callback = T.let(nil, T.nilable(T.proc.params(error: StandardError).void))
    end

    def predicate(j)
      @predicate.call(j)
    end

    def callback=(value)
      @callback = T.let(value, T.nilable(T.proc.params(error: StandardError).void))
    end

    def callback
      @callback
    end
  end
end
"##;

const REPORT: &str = r##"module ReflectionProbe
  class Report
    def self.lines
      plain = Class.new
      out = []
      out << "register: #{Registry.register(plain)}"
      out << "extended: #{plain.singleton_class.include?(Filtering)}"
      out << "example: #{Customer.example.example?}"
      config = Configuration.new
      out << "predicate: #{config.predicate(3)}"
      config.callback = proc { |error| error.message }
      out << "callback: #{config.callback.call(StandardError.new("boom"))}"
      begin
        config.callback = 42
      rescue TypeError => e
        out << "type error: #{e.message.lines.first.chomp}"
      end
      out
    end
  end
end
"##;

/// The probe's files, in the order a plain `require` loads them.
const PROBE: [(&str, &str); 6] = [
    ("filtering", FILTERING),
    ("registry", REGISTRY),
    ("example", EXAMPLE),
    ("customer", CUSTOMER),
    ("configuration", CONFIGURATION),
    ("report", REPORT),
];

const DATABASE_YML: &str = "default: &default\n  adapter: postgresql\n  encoding: unicode\n\ntest:\n  <<: *default\n  database: rh_e2e_reflection_probe\n";

/// What CRuby with Core's sorbet-runtime prints for `Report.lines`.
const EXPECTED: [&str; 6] = [
    "register: true",
    "extended: true",
    "example: true",
    "predicate: false",
    "callback: boom",
    "type error: T.let: Expected type T.nilable(T.proc.params(error: StandardError).returns(<VOID>)), got type Integer with value 42",
];

fn core_gem_home() -> String {
    std::env::var("CORE_GEM_HOME")
        .unwrap_or_else(|_| "/nix/store/kr4swvh5mw01fs3kixfa9afy57zsyihg-ruby-gems-dev/lib/ruby/gems/4.0.0".into())
}

/// `ruby` on Core's gem home (sorbet-runtime, and the pg gem the
/// PostgreSQL tree requires), under the jemalloc its native extensions
/// were built against.
fn core_ruby() -> Command {
    let gem_home = core_gem_home();
    let jemalloc = std::env::var("RH_JEMALLOC").unwrap_or_else(|_| {
        "/nix/store/c04j2i0fkin1bw5fab35gx896558knsd-jemalloc-5.3.0-unstable-2025-09-12/lib/libjemalloc.so.2".into()
    });
    assert!(Path::new(&gem_home).join("specifications").is_dir(), "CORE_GEM_HOME {gem_home} is not a gem home");
    let mut cmd = Command::new("ruby");
    cmd.env("GEM_HOME", &gem_home).env("GEM_PATH", &gem_home).env_remove("RUBYOPT");
    if Path::new(&jemalloc).exists() {
        cmd.env("LD_PRELOAD", &jemalloc);
    }
    cmd
}

fn run_lines(mut cmd: Command, side: &str) -> Vec<String> {
    let out = cmd.output().expect("spawn ruby");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "{side} run failed:\nstdout:\n{stdout}\nstderr:\n{}", String::from_utf8_lossy(&out.stderr));
    stdout.lines().map(str::to_string).collect()
}

/// The probe's sources on CRuby with Core's sorbet-runtime.
fn native_lines() -> Vec<String> {
    let dir = std::env::temp_dir().join(format!("rh_cruby_reflection_native_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut program = String::from("require \"sorbet-runtime\"\n");
    for (name, text) in PROBE {
        std::fs::write(dir.join(format!("{name}.rb")), text).unwrap();
        program.push_str(&format!("require_relative \"{name}\"\n"));
    }
    program.push_str("puts ReflectionProbe::Report.lines\n");
    std::fs::write(dir.join("native.rb"), program).unwrap();
    let mut cmd = core_ruby();
    cmd.arg("native.rb").current_dir(&dir);
    let lines = run_lines(cmd, "native");
    std::fs::remove_dir_all(&dir).unwrap();
    lines
}

fn pg_probe() -> emit_and_run::Overlay {
    let app = emit_and_run::empty_app()
        .write("roundhouse.yml", "database_adapter: postgresql\n")
        .write("config/database.yml", DATABASE_YML)
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("db/structure.sql", "CREATE TABLE shops (\n    id bigserial NOT NULL,\n    CONSTRAINT shops_pkey PRIMARY KEY (id)\n);\n");
    PROBE.iter().fold(app, |app, (name, text)| app.write(&format!("app/models/reflection_probe/{name}.rb"), text))
}

fn probe_file(emitted: &Path, name: &str) -> String {
    std::fs::read_to_string(emitted.join(format!("app/models/reflection_probe/{name}.rb")))
        .unwrap_or_else(|e| panic!("emitted {name}.rb: {e}"))
}

#[test]
fn the_postgresql_tree_runs_extend_module_protocol_and_proc_lets_as_native() {
    let (emitted, errors) = pg_probe().emit(BuildTarget::Ruby);
    let errors: Vec<&String> = errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect();
    assert!(errors.is_empty(), "{errors:#?}");

    let registry = probe_file(&emitted, "registry");
    assert!(registry.contains("klass.extend(ReflectionProbe::Filtering)"), "the extend was not emitted:\n{registry}");
    let example = probe_file(&emitted, "example");
    assert!(example.contains("base.class_eval do\n        def example?\n          true\n        end\n      end"), "the hook's class_eval was not emitted:\n{example}");
    let configuration = probe_file(&emitted, "configuration");
    // A lambda literal is a Proc: the check cannot fail, so the value
    // stands alone; any other value is checked by its class.
    assert!(configuration.contains("@predicate = ->(j) { false }"), "{configuration}");
    assert!(
        configuration.contains("SorbetRuntimeCheck.cast(value, SorbetRuntimeCheck.nilable(SorbetRuntimeCheck.proc_type({ error: StandardError }, SorbetRuntimeCheck.void)), \"T.let\""),
        "{configuration}"
    );
    for (name, _) in PROBE {
        let text = probe_file(&emitted, name);
        assert!(!text.contains("roundhouse:"), "{name}.rb keeps a stub:\n{text}");
    }

    let native = native_lines();
    assert_eq!(native, EXPECTED, "native");
    // The tree boots without a connection: nothing here reads the database.
    let mut cmd = core_ruby();
    cmd.arg("-e")
        .arg("require File.expand_path(\"main\", Dir.pwd)\nputs ReflectionProbe::Report.lines\n")
        .current_dir(&*emitted)
        .env("RAILS_ENV", "test")
        .env_remove("DATABASE_URL")
        .env_remove("ROUNDHOUSE_DATABASE_URL");
    assert_eq!(run_lines(cmd, "emitted"), native, "emitted");
}

/// Off the CRuby tree the instance extend stays the raise stub, and in an
/// `unless` it is still the branch's statement, not an empty branch the
/// emitter drops (`nil if klass < Filtering`).
#[test]
fn an_instance_extend_under_unless_keeps_its_raise_off_postgresql() {
    let run = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("app/models/reflection_probe/filtering.rb", FILTERING)
        .write("app/models/reflection_probe/registry.rb", REGISTRY)
        .run_ruby("begin\n  ReflectionProbe::Registry.register(Class.new)\nrescue RuntimeError => e\n  puts e.message\nend\n");
    let registry = probe_file(&run.emitted, "registry");
    assert!(
        registry.contains("if klass < ReflectionProbe::Filtering\n        nil\n      else\n        (raise \"roundhouse: Object#extend not supported (all targets)\")\n      end\n"),
        "the stub left its branch:\n{registry}"
    );
    assert!(run.success, "emitted program failed:\n{}\n{}", run.stdout, run.stderr);
    assert_eq!(run.stdout.trim_end(), "roundhouse: Object#extend not supported (all targets)");
}

const PROC_LETS: &str = r##"class ProcLets
  Formatter = T.let(proc do |value| value.to_s end, T.proc.params(value: Integer).returns(String))

  def initialize(values)
    @values = values
  end

  def lambda_literal
    T.let(->(job) { false }, T.proc.params(job: Integer).returns(T::Boolean))
  end

  def any_value
    T.let(@values.first, T.proc.params(job: Integer).returns(T::Boolean))
  end

  def nilable_void
    T.let(@values.first, T.nilable(T.proc.params(error: StandardError).void))
  end

  def bound
    T.cast(@values.first, T.proc.bind(String).returns(Integer))
  end

  def no_params
    T.let(@values.first, T.proc.returns(Integer))
  end

  def formatter
    Formatter.call(12)
  end
end
"##;

const PROC_SCRIPT: &str = r##"def seen(label)
  value = yield
  puts "#{label}: #{value.is_a?(Proc) ? "Proc" : value.inspect}"
rescue TypeError => e
  puts "#{label}: #{[e.class.name, e.message.sub(%r{Caller: .*/(lib/proc_lets\.rb:\d+)\z}, 'Caller: \\1')].inspect}"
end
seen("lambda_literal") { ProcLets.new([]).lambda_literal.call(1) }
seen("formatter") { ProcLets.new([]).formatter }
[["proc", proc { 1 }], ["lambda", -> { 1 }], ["method", 1.method(:+).to_proc], ["nil", nil], ["string", "x"], ["integer", 7]].each do |label, value|
  %i[any_value nilable_void bound no_params].each { |m| seen("#{m} #{label}") { ProcLets.new([value]).public_send(m) } }
end
"##;

/// A proc type at the top of a `T.let` / `T.cast` checks the value's
/// class, as sorbet-runtime does: any Proc passes, whatever its arity or
/// bind, and anything else raises sorbet's TypeError with sorbet's name
/// for the type and the `Caller:` line.
#[test]
fn a_proc_type_checks_that_the_value_is_a_proc_as_sorbet_does() {
    let run = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("app/.keep", "")
        .write("lib/proc_lets.rb", PROC_LETS)
        .run_ruby(PROC_SCRIPT);
    run.assert_passes();
    let dir = std::env::temp_dir().join(format!("rh_cruby_proc_lets_native_{}", std::process::id()));
    std::fs::create_dir_all(dir.join("lib")).unwrap();
    std::fs::write(dir.join("lib/proc_lets.rb"), PROC_LETS).unwrap();
    std::fs::write(dir.join("native.rb"), format!("require \"sorbet-runtime\"\nrequire_relative \"lib/proc_lets\"\n{PROC_SCRIPT}")).unwrap();
    let mut cmd = core_ruby();
    cmd.arg("native.rb").current_dir(&dir);
    let native = run_lines(cmd, "native");
    std::fs::remove_dir_all(&dir).unwrap();
    let emitted: Vec<String> = run.stdout.lines().map(str::to_string).collect();
    assert_eq!(native.len(), 2 + 6 * 4, "native:\n{native:#?}");
    assert_eq!(emitted, native, "emitted:\n{}", run.stdout);
    let line = |needle: &str| PROC_LETS.lines().position(|l| l.contains(needle)).expect(needle) + 1;
    for expected in [
        "lambda_literal: false".to_string(),
        "formatter: \"12\"".to_string(),
        "any_value method: Proc".to_string(),
        "nilable_void nil: nil".to_string(),
        format!(
            "any_value string: [\"TypeError\", \"T.let: Expected type T.proc.params(job: Integer).returns(T::Boolean), got type String with value \\\"x\\\"\\nCaller: lib/proc_lets.rb:{}\"]",
            line("T.let(@values.first, T.proc.params(job: Integer)")
        ),
        format!(
            "nilable_void integer: [\"TypeError\", \"T.let: Expected type T.nilable(T.proc.params(error: StandardError).returns(<VOID>)), got type Integer with value 7\\nCaller: lib/proc_lets.rb:{}\"]",
            line("T.nilable(T.proc.params(error: StandardError).void)")
        ),
        format!(
            "bound nil: [\"TypeError\", \"T.cast: Expected type T.proc.params().returns(Integer), got type NilClass\\nCaller: lib/proc_lets.rb:{}\"]",
            line("T.proc.bind(String)")
        ),
    ] {
        assert!(native.contains(&expected), "missing {expected:?} in native:\n{native:#?}");
    }
}

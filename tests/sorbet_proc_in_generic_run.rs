//! A proc type as a stdlib generic's argument
//! (`T.let({}.freeze, T::Hash[Symbol, T.proc.returns(T.untyped)])`,
//! Core's pipeline/base_state.rb) on the CRuby tree, against the same
//! program run natively on Core's sorbet-runtime (CORE_GEM_HOME,
//! `tests/support/rails_oracle.rb`).
//!
//! sorbet-runtime 0.6.13502's `TypedHash#valid?` (and every stdlib
//! generic's) checks the outer class only, so the proc type is never
//! checked; `T::Types::Proc#name` spells it in the failure message
//! (`T.proc.params().returns(T.untyped)`, `<VOID>` for `void`, the bind
//! left out). `ingest::sorbet_check` builds
//! `SorbetRuntimeCheck.proc_type({...}, returns)` for its name. Every
//! observation, message included, agrees with the native run. A proc
//! type outside a generic's arguments stays refused.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::process::Command;

const LIB: &str = r##"class Sorter
  Callbacks = T.type_alias { T::Hash[Symbol, T.proc.void] }

  def initialize(values)
    @values = values
  end

  def base_state
    T.let(@values.first, T::Hash[Symbol, T.proc.returns(T.untyped)])
  end

  def frozen_literal
    T.let({}.freeze, T::Hash[Symbol, T.proc.returns(T.untyped)])
  end

  def params
    T.cast(@values.first, T::Array[T.proc.params(x: Integer, y: T.nilable(String)).returns(String)])
  end

  def bound_void
    T.let(@values.first, T::Hash[String, T.proc.bind(String).void])
  end

  def nested
    T.assert_type!(@values.first, T.nilable(T::Array[T.nilable(T.proc.params(a: T::Array[T.proc.void]).returns(Integer))]))
  end

  def aliased
    T.let(@values.first, Callbacks)
  end
end
"##;

/// One observation per line, `label: value`; an exception is its class
/// and its message, the sorbet `Caller:` path from `lib/` on and an
/// object hash or a proc's address and file masked (they differ from run
/// to run).
const SCRIPT: &str = r##"def seen(label)
  value = begin
    yield
  rescue StandardError => e
    [e.class.name, e.message.sub(%r{Caller: .*/(lib/sorter\.rb:\d+)\z}, 'Caller: \1').gsub(/with hash -?\d+/, "with hash H").gsub(/#<Proc:[^>]*>/, "#<Proc>")]
  end
  puts "#{label}: #{value.inspect}"
end
VALUES = {
  "nil" => [], "{}" => [{}], "procs" => [{ a: -> { 1 } }], "not_procs" => [{ a: 1 }], "list" => [[1, "x"]],
  "str" => ["str"], "12" => [12],
}
%i[base_state params bound_void nested aliased].each do |m|
  VALUES.each { |label, values| seen("#{m} #{label}") { v = Sorter.new(values).public_send(m); v.is_a?(Hash) && v[:a].is_a?(Proc) ? "hash of proc" : v } }
end
seen("frozen_literal") { v = Sorter.new([]).frozen_literal; [v, v.frozen?] }
"##;

const LINES: usize = 5 * 7 + 1;

fn app() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("app/.keep", "")
        .write("test/test_helper.rb", "require \"active_support/test_case\"\n")
        .write("lib/sorter.rb", LIB)
}

/// The program on Core's sorbet-runtime, with the library at
/// `lib/sorter.rb` as in the app.
fn native(script: &str) -> String {
    let oracle = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/rails_oracle.rb");
    // Each call gets its own directory, so concurrent tests never share one.
    static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rh_proc_in_generic_native_{}_{seq}", std::process::id()));
    std::fs::create_dir_all(dir.join("lib")).unwrap();
    std::fs::write(dir.join("lib/sorter.rb"), LIB).unwrap();
    let program = format!(
        "require {oracle:?}\nRailsOracle.activate(\"activesupport\")\nrequire \"sorbet-runtime\"\n\
         abort \"native: sorbet-runtime is not Core's\" unless Gem.loaded_specs[\"sorbet-runtime\"].full_gem_path.start_with?(RailsOracle::CORE_GEM_HOME)\n\
         abort \"native: sorbet-runtime is not 0.6.13502\" unless Gem.loaded_specs[\"sorbet-runtime\"].version.to_s == \"0.6.13502\"\n\
         require_relative \"lib/sorter\"\n{script}"
    );
    std::fs::write(dir.join("native.rb"), program).unwrap();
    let out = Command::new("ruby").arg("native.rb").current_dir(&dir).output().expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "native run failed:\n{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    std::fs::remove_dir_all(&dir).unwrap();
    stdout
}

#[test]
fn proc_types_in_generics_check_like_the_native_run() {
    let native = native(SCRIPT);
    let native_lines: Vec<&str> = native.lines().collect();
    assert_eq!(native_lines.len(), LINES, "native:\n{native}");
    for expected in [
        "base_state {}: {}",
        "base_state not_procs: {a: 1}",
        "base_state 12: [\"TypeError\", \"T.let: Expected type T::Hash[Symbol, T.proc.params().returns(T.untyped)], got type Integer with value 12\\nCaller: lib/sorter.rb:9\"]",
        "params str: [\"TypeError\", \"T.cast: Expected type T::Array[T.proc.params(x: Integer, y: T.nilable(String)).returns(String)], got type String with value \\\"str\\\"\\nCaller: lib/sorter.rb:17\"]",
        "bound_void list: [\"TypeError\", \"T.let: Expected type T::Hash[String, T.proc.params().returns(<VOID>)], got T::Array[T.any(Integer, String)]\\nCaller: lib/sorter.rb:21\"]",
        "frozen_literal: [{}, true]",
    ] {
        assert!(native_lines.contains(&expected), "missing {expected:?} in native:\n{native}");
    }
    let emitted = app().run_ruby(SCRIPT);
    emitted.assert_passes();
    let emitted_lines: Vec<&str> = emitted.stdout.lines().collect();
    assert_eq!(emitted_lines.len(), LINES, "emitted:\n{}", emitted.stdout);
    for (e, n) in emitted_lines.iter().zip(&native_lines) {
        assert_eq!(e, n, "emitted:\n{}\nnative:\n{native}", emitted.stdout);
    }
}

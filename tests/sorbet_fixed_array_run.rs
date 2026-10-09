//! A tuple shape (`[Integer, String]`) as the type of `T.let`, `T.cast`
//! or `T.assert_type!`, written in place or through a type alias, on
//! the CRuby tree, against the same program run natively on Core's
//! sorbet-runtime (CORE_GEM_HOME, `tests/support/rails_oracle.rb`).
//!
//! sorbet-runtime 0.6.13502 coerces the array to `T::Types::FixedArray`:
//! an Array passes when it has exactly the declared length and each
//! element passes the type at its position; its name is the types in
//! brackets, and a failure's `got` is the element classes in brackets,
//! or the size when the length differs. `ingest::sorbet_check` builds
//! `SorbetRuntimeCheck.fixed_array([...])`, the overlay's port. Every
//! observation, message included, agrees with the native run, and a
//! checked tuple reads as the tuple type the analyzer knows (`first_succ`).
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::process::Command;

const LIB: &str = r##"module Pairs
  Row = T.type_alias { [Integer, T.nilable(String)] }
end

class Tupler
  Pair = T.type_alias { [Integer, String] }
  Nested = T.type_alias { [Pair, T::Boolean] }

  def initialize(values)
    @values = values
  end

  def direct
    T.let(@values.first, [Integer, String])
  end

  def nilable_member
    T.cast(@values.first, [T.nilable(Integer), String])
  end

  def alias_row
    T.let(@values.first, Pairs::Row)
  end

  def alias_pair
    T.let(@values.first, Pair)
  end

  def nilable_tuple
    T.let(@values.first, T.nilable([Integer, String]))
  end

  def union_tuples
    T.cast(@values.first, T.any([Integer], [String, Integer]))
  end

  def nested
    T.assert_type!(@values.first, Nested)
  end

  def empty_tuple
    T.let(@values.first, [])
  end

  def array_of_tuples
    T.let(@values.first, T::Array[[Integer, String]])
  end

  def shape_in_tuple
    T.let(@values.first, [{ a: Integer }, Symbol])
  end

  def first_succ
    T.let(@values.first, Pair).first + 1
  end
end
"##;

/// One observation per line, `label: value`; an exception is its class
/// and its message, the sorbet `Caller:` path from `lib/` on and an
/// object hash masked (it differs from run to run).
const SCRIPT: &str = r##"def seen(label)
  value = begin
    yield
  rescue StandardError => e
    [e.class.name, e.message.sub(%r{Caller: .*/(lib/tupler\.rb:\d+)\z}, 'Caller: \1').gsub(/with hash -?\d+/, "with hash H")]
  end
  puts "#{label}: #{value.inspect}"
end
VALUES = {
  "nil" => [], "[]" => [[]], "1" => [[1]], "1_a" => [[1, "a"]], "1_sym" => [[1, :a]], "1_a_2" => [[1, "a", 2]],
  "a_1" => [["a", 1]], "nil_a" => [[nil, "a"]], "1_nil" => [[1, nil]], "{}" => [{}], "str" => ["str"], "12" => [12],
  "nested" => [[[1, "a"], true]], "nested_bad" => [[[1, :a], nil]], "tuples" => [[[1, "a"], [2, "b"]]],
  "shape" => [[{ a: 1 }, :s]], "shape_bad" => [[{ a: "1" }, :s]],
}
%i[direct nilable_member alias_row alias_pair nilable_tuple union_tuples nested empty_tuple array_of_tuples shape_in_tuple first_succ].each do |m|
  VALUES.each { |label, values| seen("#{m} #{label}") { Tupler.new(values).public_send(m) } }
end
"##;

const LINES: usize = 11 * 17;

fn app() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("app/.keep", "")
        .write("test/test_helper.rb", "require \"active_support/test_case\"\n")
        .write("lib/tupler.rb", LIB)
}

/// The program on Core's sorbet-runtime, with the library at
/// `lib/tupler.rb` as in the app.
fn native(script: &str) -> String {
    let oracle = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/rails_oracle.rb");
    // Each call gets its own directory, so concurrent tests never share one.
    static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rh_fixed_array_native_{}_{seq}", std::process::id()));
    std::fs::create_dir_all(dir.join("lib")).unwrap();
    std::fs::write(dir.join("lib/tupler.rb"), LIB).unwrap();
    let program = format!(
        "require {oracle:?}\nRailsOracle.activate(\"activesupport\")\nrequire \"sorbet-runtime\"\n\
         abort \"native: sorbet-runtime is not Core's\" unless Gem.loaded_specs[\"sorbet-runtime\"].full_gem_path.start_with?(RailsOracle::CORE_GEM_HOME)\n\
         abort \"native: sorbet-runtime is not 0.6.13502\" unless Gem.loaded_specs[\"sorbet-runtime\"].version.to_s == \"0.6.13502\"\n\
         require_relative \"lib/tupler\"\n{script}"
    );
    std::fs::write(dir.join("native.rb"), program).unwrap();
    let out = Command::new("ruby").arg("native.rb").current_dir(&dir).output().expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "native run failed:\n{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    std::fs::remove_dir_all(&dir).unwrap();
    stdout
}

#[test]
fn tuple_shapes_check_like_the_native_run() {
    let native = native(SCRIPT);
    let native_lines: Vec<&str> = native.lines().collect();
    assert_eq!(native_lines.len(), LINES, "native:\n{native}");
    // What sorbet answers, spelled as it spells it: the exact length,
    // each position its own type, and both names in the message.
    for expected in [
        "direct 1_a: [1, \"a\"]",
        "direct 1: [\"TypeError\", \"T.let: Expected type [Integer, String], got array of size 1\\nCaller: lib/tupler.rb:14\"]",
        "direct 1_a_2: [\"TypeError\", \"T.let: Expected type [Integer, String], got array of size 3\\nCaller: lib/tupler.rb:14\"]",
        "direct a_1: [\"TypeError\", \"T.let: Expected type [Integer, String], got type [String, Integer]\\nCaller: lib/tupler.rb:14\"]",
        "direct {}: [\"TypeError\", \"T.let: Expected type [Integer, String], got type Hash with value {}\\nCaller: lib/tupler.rb:14\"]",
        "nilable_member nil_a: [nil, \"a\"]",
        "alias_row 1_nil: [1, nil]",
        "nilable_tuple nil: nil",
        "nilable_tuple 1: [\"TypeError\", \"T.let: Expected type T.nilable([Integer, String]), got type Array with value [1]\\nCaller: lib/tupler.rb:30\"]",
        "union_tuples a_1: [\"a\", 1]",
        "union_tuples 1_a: [\"TypeError\", \"T.cast: Expected type T.any([Integer], [String, Integer]), got type Array with value [1, \\\"a\\\"]\\nCaller: lib/tupler.rb:34\"]",
        "nested nested: [[1, \"a\"], true]",
        "nested nested_bad: [\"TypeError\", \"T.assert_type!: Expected type [[Integer, String], T::Boolean], got type [Array, NilClass]\\nCaller: lib/tupler.rb:38\"]",
        "empty_tuple []: []",
        "empty_tuple 1_a: [\"TypeError\", \"T.let: Expected type [], got array of size 2\\nCaller: lib/tupler.rb:42\"]",
        "array_of_tuples 1_sym: [1, :a]",
        "shape_in_tuple shape: [{a: 1}, :s]",
        "shape_in_tuple shape_bad: [\"TypeError\", \"T.let: Expected type [{a: Integer}, Symbol], got type [Hash, Symbol]\\nCaller: lib/tupler.rb:50\"]",
        "first_succ 1_a: 2",
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

/// A splat makes the length a runtime value, and a member the check
/// cannot read is refused under its own reason: both by name, at the
/// site, never skipped.
#[test]
fn an_unreadable_tuple_is_refused_by_name() {
    for (body, reason) in [
        ("T.let(@values.first, [Integer, *[String]])", "splats an array into a tuple shape"),
        ("T.let(@values.first, [Integer, T.proc.params(x: Integer)])", "proc type without `returns` or `void`"),
    ] {
        let lib = format!("class Odd\n  def initialize(values)\n    @values = values\n  end\n\n  def odd\n    {body}\n  end\nend\n");
        let (_, errors) = emit_and_run::empty_app()
            .write("roundhouse.yml", "test_paths:\n  - test\n")
            .write("app/.keep", "")
            .write("lib/odd.rb", &lib)
            .emit(roundhouse::project::BuildTarget::Ruby);
        assert!(errors.iter().any(|e| e.contains("not supported") && e.contains(reason)), "{body}: {errors:#?}");
    }
}

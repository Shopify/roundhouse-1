//! A hash shape (`{a: Integer, "b" => String}`) as the type of `T.let`,
//! `T.cast` or `T.assert_type!`, written in place or through a type
//! alias, on the CRuby tree, against the same program run natively on
//! Core's sorbet-runtime (CORE_GEM_HOME, `tests/support/rails_oracle.rb`).
//!
//! sorbet-runtime 0.6.13502 coerces the shape to `T::Types::FixedHash`:
//! a Hash passes when each declared key's value (`obj[key]`, so an
//! absent key reads nil, or the hash's default) passes its type and the
//! hash has no other key; its name and the failure's `got` spell the
//! hash as `{a: Integer, "b" => String}`. `ingest::sorbet_check` builds
//! `SorbetRuntimeCheck.fixed_hash({...})`, the overlay's port. Every
//! observation, message included, agrees with the native run.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::process::Command;

const LIB: &str = r##"module Shapes
  Row = T.type_alias { { sort_key: T.nilable(String) } }
end

class Sorter
  Entry = T.type_alias { { id: Integer, "label" => String, "odd-key": T.nilable(Symbol) } }
  Nested = T.type_alias { { inner: Entry, list: T::Array[Integer] } }

  def initialize(values)
    @values = values
  end

  def direct
    T.let(@values.first, { sort_key: T.nilable(String) })
  end

  def mixed_keys
    T.cast(@values.first, { "a" => Integer, b: T::Boolean })
  end

  def alias_row
    T.let(@values.first, Shapes::Row)
  end

  def alias_entry
    T.let(@values.first, Entry)
  end

  def nilable_shape
    T.let(@values.first, T.nilable({ a: Integer }))
  end

  def union_shapes
    T.cast(@values.first, T.any({ a: Integer }, { b: String }))
  end

  def nested
    T.assert_type!(@values.first, Nested)
  end

  def empty_shape
    T.let(@values.first, {})
  end

  def array_of_shapes
    T.let(@values.first, T::Array[{ a: Integer }])
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
    [e.class.name, e.message.sub(%r{Caller: .*/(lib/sorter\.rb:\d+)\z}, 'Caller: \1').gsub(/with hash -?\d+/, "with hash H")]
  end
  puts "#{label}: #{value.inspect}"
end
VALUES = {
  "nil" => [], "{}" => [{}], "sort_nil" => [{ sort_key: nil }], "sort_str" => [{ sort_key: "x" }],
  "sort_int" => [{ sort_key: 1 }], "sort_extra" => [{ sort_key: "x", extra: 1 }],
  "sort_string_key" => [{ "sort_key" => "x" }], "a1" => [{ a: 1 }], "a_str" => [{ a: "1" }],
  "mixed" => [{ "a" => 1, b: true }], "mixed_sym" => [{ a: 1, b: false }], "b_str" => [{ b: "s" }],
  "entry" => [{ id: 1, "label" => "l" }], "entry_full" => [{ id: 1, "label" => "l", "odd-key": :s }],
  "entry_bad" => [{ id: 1, "label" => :l }], "nested" => [{ inner: { id: 1, "label" => "l" }, list: [1] }],
  "nested_bad" => [{ inner: { id: "x" }, list: [] }], "default_1" => [Hash.new(1)],
  "default_1_a" => [Hash.new(1).merge(a: 2)], "array" => [[{ a: 1 }]], "str" => ["str"], "12" => [12],
}
%i[direct mixed_keys alias_row alias_entry nilable_shape union_shapes nested empty_shape array_of_shapes].each do |m|
  VALUES.each { |label, values| seen("#{m} #{label}") { Sorter.new(values).public_send(m) } }
end
"##;

const LINES: usize = 9 * 22;

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
    let dir = std::env::temp_dir().join(format!("rh_fixed_hash_native_{}_{seq}", std::process::id()));
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
fn hash_shapes_check_like_the_native_run() {
    let native = native(SCRIPT);
    let native_lines: Vec<&str> = native.lines().collect();
    assert_eq!(native_lines.len(), LINES, "native:\n{native}");
    // What sorbet answers, spelled as it spells it: the declared keys
    // only, an absent key read as nil or the default, and both names in
    // the message.
    for expected in [
        "direct {}: {}",
        "direct sort_nil: {sort_key: nil}",
        "direct sort_extra: [\"TypeError\", \"T.let: Expected type {sort_key: T.nilable(String)}, got type {sort_key: String, extra: Integer}\\nCaller: lib/sorter.rb:14\"]",
        "direct sort_string_key: [\"TypeError\", \"T.let: Expected type {sort_key: T.nilable(String)}, got type {\\\"sort_key\\\" => String}\\nCaller: lib/sorter.rb:14\"]",
        "direct default_1: [\"TypeError\", \"T.let: Expected type {sort_key: T.nilable(String)}, got type {}\\nCaller: lib/sorter.rb:14\"]",
        "mixed_keys mixed: {\"a\" => 1, b: true}",
        "mixed_keys mixed_sym: [\"TypeError\", \"T.cast: Expected type {\\\"a\\\" => Integer, b: T::Boolean}, got type {a: Integer, b: FalseClass}\\nCaller: lib/sorter.rb:18\"]",
        "alias_entry entry: {id: 1, \"label\" => \"l\"}",
        "alias_entry entry_bad: [\"TypeError\", \"T.let: Expected type {id: Integer, \\\"label\\\" => String, :\\\"odd-key\\\" => T.nilable(Symbol)}, got type {id: Integer, \\\"label\\\" => Symbol}\\nCaller: lib/sorter.rb:26\"]",
        "nilable_shape default_1: {}",
        "union_shapes 12: [\"TypeError\", \"T.cast: Expected type T.any({a: Integer}, {b: String}), got type Integer with value 12\\nCaller: lib/sorter.rb:34\"]",
        "nested nested_bad: [\"TypeError\", \"T.assert_type!: Expected type {inner: {id: Integer, \\\"label\\\" => String, :\\\"odd-key\\\" => T.nilable(Symbol)}, list: T::Array[Integer]}, got type {inner: Hash, list: Array}\\nCaller: lib/sorter.rb:38\"]",
        "empty_shape default_1: {}",
        "array_of_shapes {}: [\"TypeError\", \"T.let: Expected type T::Array[{a: Integer}], got T::Hash[T.noreturn, T.noreturn]\\nCaller: lib/sorter.rb:46\"]",
        "array_of_shapes array: [{a: 1}]",
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

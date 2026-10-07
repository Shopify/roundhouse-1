//! Author-written casts (`x #: as Integer`, `T.cast(x, Integer)`,
//! `T.let(x, String)`, `T.bind`, `T.assert_type!`) in the emitted Ruby,
//! against the same program run natively on Core's sorbet-runtime and
//! Core's ActiveSupport (CORE_GEM_HOME, `tests/support/rails_oracle.rb`).
//!
//! A cast used to emit a conversion when the value's type was unknown
//! (`(x).to_i`), so `nil #: as Integer` read `0` and `"12"` read `12`; and
//! `lower::blank` folded `n.present?` to `true` on the declared `Integer`,
//! where Rails answers `false` for the nil an empty list's `first` gives.
//!
//! The `#:` comment is inert natively and the value alone here. sorbet's
//! `T.let` / `T.cast` / `T.bind` / `T.assert_type!` check the value's
//! class at runtime, and the tree runs the same check
//! (`ingest::sorbet_check`, the CRuby overlay's sorbet_runtime_check.rb):
//! a value sorbet rejects raises sorbet's TypeError, message and
//! `Caller:` line included. Every observation agrees with the native run.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;
use std::process::Command;

const LIB: &str = r##"class Thing
  def name
    "thing"
  end
end

class Labeled
  def inspect
    "labeled:" + ("x" * 70)
  end
end

class Box
  extend T::Generic
  Elem = type_member
end

class Caster
  def initialize(values)
    @values = values
    @held = values.first #: as Integer
    @maybe = T.let(nil, T.nilable(Integer))
    @names = T.let([], T::Array[String])
  end

  def comment_value
    n = @values.first #: as Integer
    n
  end

  def comment_present
    n = @values.first #: as Integer
    n.present?
  end

  def comment_blank
    s = @values.first #: as String
    s.blank?
  end

  def comment_presence
    s = @values.first #: as String
    s.presence
  end

  def comment_nil
    n = @values.first #: as Integer
    n.nil?
  end

  def comment_to_s
    n = @values.first #: as Integer
    n.to_s
  end

  def comment_truthy
    n = @values.first #: as Integer
    n ? :yes : :no
  end

  def comment_nilable_present
    n = @values.first #: as Integer?
    n.present?
  end

  def comment_bool
    b = @values.first #: as bool
    [b, b.blank?]
  end

  def comment_array_present
    a = @values.first #: as Array[String]
    a.present?
  end

  def comment_ivar_present
    @held.present?
  end

  def comment_thing_present
    t = @values.first #: as Thing
    t.present?
  end

  def defaults
    [@maybe, @names]
  end

  def tcast_value
    T.cast(@values.first, Integer)
  end

  def tcast_present
    T.cast(@values.first, Integer).present?
  end

  def tlet_blank
    s = T.let(@values.first, String)
    s.blank?
  end

  def tlet_nilable
    T.let(@values.first, T.nilable(String))
  end

  def tlet_any
    T.let(@values.first, T.any(String, Integer))
  end

  def tlet_nilable_any
    T.let(@values.first, T.nilable(T.any(Symbol, Integer, Thing)))
  end

  def tlet_bool
    T.let(@values.first, T::Boolean)
  end

  def tcast_nilable_bool
    T.cast(@values.first, T.nilable(T::Boolean))
  end

  def tlet_bool_or_int
    T.let(@values.first, T.any(Integer, T::Boolean))
  end

  def tlet_array
    T.let(@values.first, T::Array[T.nilable(String)])
  end

  def tcast_hash
    T.cast(@values.first, T::Hash[Symbol, T.untyped])
  end

  def tlet_bare_array
    T.let(@values.first, Array)
  end

  def tlet_nilable_array
    T.let(@values.first, T.nilable(T::Array[Integer]))
  end

  def tlet_generic
    T.let(@values.first, Box[Integer])
  end

  def tcast_class_of
    T.cast(@values.first, T.class_of(Thing))
  end

  def tlet_untyped
    T.let(@values.first, T.untyped)
  end

  def tlet_unchecked
    T.let(@values.first, Integer, checked: false)
  end

  def tbind
    @values.first.instance_exec do
      T.bind(self, Integer)
      self + 1
    end
  end

  def tassert
    T.assert_type!(@values.first, Integer)
  end

  def tlet_multiline
    T.let(
      @values.first,
      T.any(
        Integer,
        Float,
      ),
    )
  end
end
"##;

/// One observation per line, `label: value`; an exception is its class
/// and its message, a sorbet `Caller:` path from `lib/` on and an object
/// hash or address masked (they differ from run to run).
const SCRIPT: &str = r##"def seen(label)
  value = begin
    yield
  rescue StandardError => e
    [e.class.name, e.message.sub(%r{Caller: .*/(lib/caster\.rb:\d+)\z}, 'Caller: \1').gsub(/with hash -?\d+/, "with hash H")]
  end
  puts "#{label}: #{value.inspect.gsub(/0x\h+/, "0x")}"
end
VALUES = { "nil" => [], "12" => [12], "'12'" => ["12"], "0" => [0], "''" => [""], "'  '" => ["  "], "false" => [false], "[]" => [[]] }
%i[comment_value comment_present comment_blank comment_presence comment_nil comment_to_s comment_truthy
   comment_nilable_present comment_bool comment_array_present comment_ivar_present].each do |m|
  VALUES.each { |label, values| seen("#{m} #{label}") { Caster.new(values).public_send(m) } }
end
seen("comment_thing_present nil") { Caster.new([]).comment_thing_present }
seen("comment_thing_present thing") { Caster.new([Thing.new]).comment_thing_present }
seen("defaults") { Caster.new([]).defaults }
CHECKED = VALUES.merge(
  "thing" => [Thing.new], "Thing" => [Thing], "labeled" => [Labeled.new], "box" => [Box.new],
  "true" => [true], "1.5" => [1.5], ":sym" => [:sym], "{}" => [{}], "[nil]" => [[nil]],
  "long" => ["y" * 80], "mixed" => [[1, "a", true, false]], "{a: 1}" => [{ a: 1, b: 2.5 }]
)
%i[tcast_value tcast_present tlet_blank tlet_nilable tlet_any tlet_nilable_any tlet_bool tcast_nilable_bool
   tlet_bool_or_int tlet_array tcast_hash tlet_bare_array tlet_nilable_array tlet_generic tcast_class_of
   tlet_untyped tlet_unchecked tbind tassert tlet_multiline].each do |m|
  CHECKED.each { |label, values| seen("#{m} #{label}") { Caster.new(values).public_send(m) } }
end
"##;

const COMMENT_LINES: usize = 11 * 8 + 2 + 1;
const CHECKED_LINES: usize = 20 * 20;

/// The 1-based line of LIB that contains `needle`; a needle ending in a
/// newline is the whole (trimmed) line.
fn lib_line(needle: &str) -> usize {
    LIB.lines()
        .position(|l| match needle.strip_suffix('\n') {
            Some(whole) => l.trim() == whole.trim(),
            None => l.contains(needle),
        })
        .expect(needle)
        + 1
}

fn app() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("app/.keep", "")
        .write("test/test_helper.rb", "require \"active_support/test_case\"\n")
        .write("lib/caster.rb", LIB)
}

/// The program on Core's sorbet-runtime and Core's ActiveSupport, with
/// the library at `lib/caster.rb` as in the app.
fn native(script: &str) -> String {
    let oracle = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/rails_oracle.rb");
    let dir = std::env::temp_dir().join(format!("rh_author_casts_native_{}", std::process::id()));
    std::fs::create_dir_all(dir.join("lib")).unwrap();
    std::fs::write(dir.join("lib/caster.rb"), LIB).unwrap();
    let program = format!(
        "require {oracle:?}\nRailsOracle.activate(\"activesupport\")\nrequire \"sorbet-runtime\"\n\
         abort \"native: sorbet-runtime is not Core's\" unless Gem.loaded_specs[\"sorbet-runtime\"].full_gem_path.start_with?(RailsOracle::CORE_GEM_HOME)\n\
         require \"active_support\"\nrequire \"active_support/core_ext/object/blank\"\nrequire_relative \"lib/caster\"\n{script}"
    );
    std::fs::write(dir.join("native.rb"), program).unwrap();
    let out = Command::new("ruby").arg("native.rb").current_dir(&dir).output().expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "native run failed:\n{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    std::fs::remove_dir_all(&dir).unwrap();
    stdout
}

#[test]
fn author_casts_answer_like_the_native_run() {
    let emitted = app().run_ruby(SCRIPT);
    emitted.assert_passes();
    let native = native(SCRIPT);
    let (emitted_lines, native_lines): (Vec<&str>, Vec<&str>) =
        (emitted.stdout.lines().collect(), native.lines().collect());
    assert_eq!(native_lines.len(), COMMENT_LINES + CHECKED_LINES, "native:\n{native}");
    assert_eq!(emitted_lines.len(), COMMENT_LINES + CHECKED_LINES, "emitted:\n{}", emitted.stdout);
    for (e, n) in emitted_lines.iter().zip(&native_lines) {
        assert_eq!(e, n, "emitted:\n{}\nnative:\n{native}", emitted.stdout);
    }
    // What the native run answers, so a change on both sides still shows.
    let caller = |line: usize| format!("\\nCaller: lib/caster.rb:{line}");
    let line_of = lib_line;
    for expected in [
        "comment_value nil: nil".to_string(),
        "comment_value '12': \"12\"".to_string(),
        "comment_present nil: false".to_string(),
        "comment_present 12: true".to_string(),
        "comment_present '  ': false".to_string(),
        "comment_blank '  ': true".to_string(),
        "comment_blank 0: false".to_string(),
        "comment_presence '': nil".to_string(),
        "comment_nil nil: true".to_string(),
        "comment_to_s nil: \"\"".to_string(),
        "comment_truthy nil: :no".to_string(),
        "comment_truthy 0: :yes".to_string(),
        "comment_nilable_present '': false".to_string(),
        "comment_bool 0: [0, false]".to_string(),
        "comment_bool '': [\"\", true]".to_string(),
        "comment_array_present []: false".to_string(),
        "comment_ivar_present nil: false".to_string(),
        "comment_thing_present nil: false".to_string(),
        "comment_thing_present thing: true".to_string(),
        "defaults: [nil, []]".to_string(),
        "tcast_value 12: 12".to_string(),
        "tlet_blank '  ': true".to_string(),
        // The values sorbet's class check rejects.
        format!("tcast_value nil: [\"TypeError\", \"T.cast: Expected type Integer, got type NilClass{}\"]", caller(line_of("T.cast(@values.first, Integer)\n"))),
        format!("tcast_value '12': [\"TypeError\", \"T.cast: Expected type Integer, got type String with value \\\"12\\\"{}\"]", caller(line_of("T.cast(@values.first, Integer)\n"))),
        format!("tcast_present nil: [\"TypeError\", \"T.cast: Expected type Integer, got type NilClass{}\"]", caller(line_of("T.cast(@values.first, Integer).present?"))),
        format!("tlet_blank nil: [\"TypeError\", \"T.let: Expected type String, got type NilClass{}\"]", caller(line_of("T.let(@values.first, String)"))),
        // Nilable, union, T::Boolean, generic.
        "tlet_nilable nil: nil".to_string(),
        format!("tlet_nilable 12: [\"TypeError\", \"T.let: Expected type T.nilable(String), got type Integer with value 12{}\"]", caller(line_of("T.nilable(String))"))),
        "tlet_any '12': \"12\"".to_string(),
        format!("tlet_any false: [\"TypeError\", \"T.let: Expected type T.any(Integer, String), got type FalseClass{}\"]", caller(line_of("T.any(String, Integer)"))),
        format!("tlet_nilable_any '12': [\"TypeError\", \"T.let: Expected type T.nilable(T.any(Integer, Symbol, Thing)), got type String with value \\\"12\\\"{}\"]", caller(line_of("T.nilable(T.any(Symbol"))),
        "tlet_bool false: false".to_string(),
        format!("tlet_bool 0: [\"TypeError\", \"T.let: Expected type T::Boolean, got type Integer with value 0{}\"]", caller(line_of("T.let(@values.first, T::Boolean)\n"))),
        format!("tcast_nilable_bool 0: [\"TypeError\", \"T.cast: Expected type T.nilable(T::Boolean), got type Integer with value 0{}\"]", caller(line_of("T.nilable(T::Boolean)"))),
        format!("tlet_bool_or_int '': [\"TypeError\", \"T.let: Expected type T.any(Integer, T::Boolean), got type String with value \\\"\\\"{}\"]", caller(line_of("T.any(Integer, T::Boolean)"))),
        "tlet_array [nil]: [nil]".to_string(),
        format!("tlet_array {{}}: [\"TypeError\", \"T.let: Expected type T::Array[T.nilable(String)], got T::Hash[T.noreturn, T.noreturn]{}\"]", caller(line_of("T::Array[T.nilable(String)]"))),
        format!("tcast_hash []: [\"TypeError\", \"T.cast: Expected type T::Hash[Symbol, T.untyped], got T::Array[T.noreturn]{}\"]", caller(line_of("T::Hash[Symbol, T.untyped]"))),
        format!("tcast_hash mixed: [\"TypeError\", \"T.cast: Expected type T::Hash[Symbol, T.untyped], got T::Array[T.any(Integer, String, T::Boolean)]{}\"]", caller(line_of("T::Hash[Symbol, T.untyped]"))),
        format!("tlet_array {{a: 1}}: [\"TypeError\", \"T.let: Expected type T::Array[T.nilable(String)], got T::Hash[Symbol, T.any(Float, Integer)]{}\"]", caller(line_of("T::Array[T.nilable(String)]"))),
        format!("tcast_hash [nil]: [\"TypeError\", \"T.cast: Expected type T::Hash[Symbol, T.untyped], got T::Array[NilClass]{}\"]", caller(line_of("T::Hash[Symbol, T.untyped]"))),
        format!("tlet_bare_array nil: [\"TypeError\", \"T.let: Expected type T::Array[T.untyped], got type NilClass{}\"]", caller(line_of("T.let(@values.first, Array)"))),
        format!("tlet_nilable_array 12: [\"TypeError\", \"T.let: Expected type T.nilable(T::Array[Integer]), got type Integer with value 12{}\"]", caller(line_of("T.nilable(T::Array[Integer])"))),
        format!("tlet_generic 12: [\"TypeError\", \"T.let: Expected type Box, got type Integer with value 12{}\"]", caller(line_of("Box[Integer]"))),
        "tcast_class_of Thing: Thing".to_string(),
        format!("tcast_class_of thing: [\"TypeError\", \"T.cast: Expected type T.class_of(Thing), got #<Thing:0x>{}\"]", caller(line_of("T.class_of(Thing)"))),
        format!("tlet_blank labeled: [\"TypeError\", \"T.let: Expected type String, got type Labeled with value labeled:xxxxxxxxxxxxxxxxxxx...xxxxxxxxxxxxxxxxxxxxxxxxxxxxxx{}\"]", caller(line_of("T.let(@values.first, String)"))),
        "tlet_untyped '12': \"12\"".to_string(),
        "tlet_unchecked '12': \"12\"".to_string(),
        "tbind 12: 13".to_string(),
        format!("tbind '12': [\"TypeError\", \"T.bind: Expected type Integer, got type String with value \\\"12\\\"{}\"]", caller(line_of("T.bind(self, Integer)"))),
        format!("tassert :sym: [\"TypeError\", \"T.assert_type!: Expected type Integer, got type Symbol with value :sym{}\"]", caller(line_of("T.assert_type!(@values.first, Integer)"))),
        "tlet_multiline 1.5: 1.5".to_string(),
    ] {
        assert!(native_lines.contains(&expected.as_str()), "missing {expected:?} in native:\n{native}");
    }
    // The multi-line call names the line it starts on.
    let multiline = line_of("    T.let(\n");
    assert!(
        native.contains(&format!("tlet_multiline nil: [\"TypeError\", \"T.let: Expected type T.any(Float, Integer), got type NilClass{}\"]", caller(multiline))),
        "native:\n{native}"
    );
}

/// The emitted Ruby: a `#:` comment is its value, a `T.let`-family call
/// is the overlay's check (or its value where the check cannot fail),
/// and no predicate is folded on a type only a cast declared.
#[test]
fn author_casts_emit_the_value_or_the_check() {
    let (emitted, errors) = app().emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let caster = std::fs::read_to_string(emitted.join("app/models/caster.rb")).unwrap();
    for conversion in [".to_i", "&.to_i", "include?(", "T.cast(", "T.let(", "T.bind(", "T.assert_type!(", "T.nilable", "T.any", "T::Boolean", "T::Array[", "T::Hash["] {
        assert!(!caster.contains(conversion), "{conversion} in\n{caster}");
    }
    let at = lib_line;
    let path = "File.expand_path(\"../../lib/caster.rb\", __dir__)";
    for expected in [
        "    @held = values.first\n".to_string(),
        "    @maybe = nil\n    @names = []\n".to_string(),
        "  def comment_value\n    n = @values.first\n    n\n  end\n".to_string(),
        "  def comment_present\n    n = @values.first\n    ActiveSupport.present?(n)\n  end\n".to_string(),
        "  def comment_blank\n    s = @values.first\n    ActiveSupport.blank?(s)\n  end\n".to_string(),
        "  def comment_presence\n    s = @values.first\n    ActiveSupport.presence(s)\n  end\n".to_string(),
        "  def comment_nil\n    n = @values.first\n    n.nil?\n  end\n".to_string(),
        "  def comment_to_s\n    n = @values.first\n    n.to_s\n  end\n".to_string(),
        "  def comment_nilable_present\n    n = @values.first\n    ActiveSupport.present?(n)\n  end\n".to_string(),
        "  def comment_array_present\n    a = @values.first\n    ActiveSupport.present?(a)\n  end\n".to_string(),
        "  def comment_ivar_present\n    ActiveSupport.present?(@held)\n  end\n".to_string(),
        "  def comment_thing_present\n    t = @values.first\n    !(t.nil?)\n  end\n".to_string(),
        format!("  def tcast_value\n    SorbetRuntimeCheck.cast(@values.first, Integer, \"T.cast\", {path}, {})\n  end\n", at("T.cast(@values.first, Integer)\n")),
        format!("  def tcast_present\n    ActiveSupport.present?(SorbetRuntimeCheck.cast(@values.first, Integer, \"T.cast\", {path}, {}))\n  end\n", at("T.cast(@values.first, Integer).present?")),
        format!("  def tlet_blank\n    s = SorbetRuntimeCheck.cast(@values.first, String, \"T.let\", {path}, {})\n    ActiveSupport.blank?(s)\n  end\n", at("T.let(@values.first, String)")),
        format!("SorbetRuntimeCheck.cast(@values.first, SorbetRuntimeCheck.nilable(SorbetRuntimeCheck.any(Symbol, Integer, Thing)), \"T.let\", {path}, {})", at("T.nilable(T.any(Symbol")),
        format!("SorbetRuntimeCheck.cast(@values.first, SorbetRuntimeCheck.boolean, \"T.let\", {path}, {})", at("T.let(@values.first, T::Boolean)\n")),
        format!("SorbetRuntimeCheck.cast(@values.first, SorbetRuntimeCheck.generic(\"T::Array\", ::Array, SorbetRuntimeCheck.nilable(String)), \"T.let\", {path}, {})", at("T::Array[T.nilable(String)]")),
        format!("SorbetRuntimeCheck.cast(@values.first, Box, \"T.let\", {path}, {})", at("Box[Integer]")),
        format!("SorbetRuntimeCheck.cast(@values.first, SorbetRuntimeCheck.class_of(Thing), \"T.cast\", {path}, {})", at("T.class_of(Thing)")),
        "  def tlet_untyped\n    @values.first\n  end\n".to_string(),
        "  def tlet_unchecked\n    @values.first\n  end\n".to_string(),
        format!("      SorbetRuntimeCheck.cast(self, Integer, \"T.bind\", {path}, {})\n      self + 1\n", at("T.bind(self, Integer)")),
        format!("  def tassert\n    SorbetRuntimeCheck.cast(@values.first, Integer, \"T.assert_type!\", {path}, {})\n  end\n", at("T.assert_type!(")),
    ] {
        assert!(caster.contains(&expected), "missing {expected:?} in\n{caster}");
    }
}

/// A cast the value's own type already backs still grounds: the literal
/// says String, so `blank?` is the String test.
#[test]
fn a_cast_the_value_backs_still_grounds() {
    let (emitted, errors) = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("app/.keep", "")
        .write(
            "lib/backed.rb",
            "class Backed\n  def label\n    s = \"x\" #: as String\n    s.blank?\n  end\nend\n",
        )
        .emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let backed = std::fs::read_to_string(emitted.join("app/models/backed.rb")).unwrap();
    assert!(backed.contains("s = \"x\"\n    (s || \"\").strip.empty?\n"), "{backed}");
}

/// A type the check cannot rebuild is refused at the site by name, on
/// every target: the tree never runs a `T.let` without sorbet's check.
#[test]
fn an_uncheckable_type_is_refused_by_name() {
    for (body, reason) in [
        ("T.let(@values.first, T.proc.returns(Integer))", "proc type"),
        ("T.let(@values.first, T.self_type)", "T.self_type"),
        ("T.cast(@values.first, T.attached_class)", "T.attached_class"),
        ("T.let(@values.first, [Integer, String])", "tuple shape"),
        ("T.let(@values.first, { a: Integer })", "hash shape"),
        ("T.let(@values.first, T.all(Comparable, Integer))", "intersection"),
        ("T.let(@values.first, T::Sig::WithoutRuntime)", "T::Sig::WithoutRuntime"),
        ("T.let(@values.first, Integer, checked: flag)", "checked: true"),
    ] {
        let lib = format!("class Odd\n  def initialize(values)\n    @values = values\n  end\n\n  def odd(flag = true)\n    {body}\n  end\nend\n");
        let (_, errors) = emit_and_run::empty_app()
            .write("roundhouse.yml", "test_paths:\n  - test\n")
            .write("app/.keep", "")
            .write("lib/odd.rb", &lib)
            .emit(BuildTarget::Ruby);
        assert!(
            errors.iter().any(|e| e.contains("not supported") && e.contains(reason)),
            "{body}: {errors:#?}"
        );
    }
}

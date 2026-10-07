//! Author-written casts (`x #: as Integer`, `T.cast(x, Integer)`,
//! `T.let(x, String)`) in the emitted Ruby, against the same program run
//! natively on Core's sorbet-runtime and Core's ActiveSupport
//! (CORE_GEM_HOME, `tests/support/rails_oracle.rb`).
//!
//! A cast used to emit a conversion when the value's type was unknown
//! (`(x).to_i`), so `nil #: as Integer` read `0` and `"12"` read `12`; and
//! `lower::blank` folded `n.present?` to `true` on the declared `Integer`,
//! where Rails answers `false` for the nil an empty list's `first` gives.
//!
//! The `#:` comment is inert natively, and every observation of it here
//! agrees with the native run: nil, wrong-typed and right-typed values,
//! and `present?` / `blank?` / `presence` / `nil?` / `to_s` / truthiness
//! after the cast. sorbet's `T.cast` / `T.let` agree on a value that
//! passes their class check. On a value that fails it they are the one
//! pinned divergence: natively a TypeError, here the value runs on
//! (docs/guide/transpile.md).
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;
use std::process::Command;

const LIB: &str = r##"class Thing
  def name
    "thing"
  end
end

class Caster
  def initialize(values)
    @values = values
    @held = values.first #: as Integer
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
end
"##;

/// One observation per line, `label: value`; an exception is its class
/// and the first line of its message (sorbet's adds a `Caller:` line).
const SCRIPT: &str = r##"def seen(label)
  value = begin
    yield
  rescue StandardError => e
    [e.class.name, e.message.lines.first.chomp]
  end
  puts "#{label}: #{value.inspect}"
end
VALUES = { "nil" => [], "12" => [12], "'12'" => ["12"], "0" => [0], "''" => [""], "'  '" => ["  "], "false" => [false], "[]" => [[]] }
%i[comment_value comment_present comment_blank comment_presence comment_nil comment_to_s comment_truthy
   comment_nilable_present comment_bool comment_array_present comment_ivar_present].each do |m|
  VALUES.each { |label, values| seen("#{m} #{label}") { Caster.new(values).public_send(m) } }
end
seen("comment_thing_present nil") { Caster.new([]).comment_thing_present }
seen("comment_thing_present thing") { Caster.new([Thing.new]).comment_thing_present }
seen("tcast_value 12") { Caster.new([12]).tcast_value }
seen("tcast_present 12") { Caster.new([12]).tcast_present }
seen("tlet_blank '  '") { Caster.new(["  "]).tlet_blank }
seen("tlet_blank 'x'") { Caster.new(["x"]).tlet_blank }
"##;

/// The values sorbet's class check rejects: the pinned divergence.
const REJECTED: &str = r##"seen("tcast_value nil") { Caster.new([]).tcast_value }
seen("tcast_value '12'") { Caster.new(["12"]).tcast_value }
seen("tcast_present nil") { Caster.new([]).tcast_present }
seen("tlet_blank nil") { Caster.new([]).tlet_blank }
"##;

const AGREED: usize = 11 * 8 + 2 + 4;

fn app() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("app/.keep", "")
        .write("test/test_helper.rb", "require \"active_support/test_case\"\n")
        .write("lib/caster.rb", LIB)
}

/// The program on Core's sorbet-runtime and Core's ActiveSupport.
fn native(script: &str) -> String {
    let oracle = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/rails_oracle.rb");
    let dir = std::env::temp_dir().join(format!("rh_author_casts_native_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("caster.rb"), LIB).unwrap();
    let program = format!(
        "require {oracle:?}\nRailsOracle.activate(\"activesupport\")\nrequire \"sorbet-runtime\"\n\
         abort \"native: sorbet-runtime is not Core's\" unless Gem.loaded_specs[\"sorbet-runtime\"].full_gem_path.start_with?(RailsOracle::CORE_GEM_HOME)\n\
         require \"active_support\"\nrequire \"active_support/core_ext/object/blank\"\nrequire_relative \"caster\"\n{script}"
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
    let script = format!("{SCRIPT}{REJECTED}");
    let emitted = app().run_ruby(&script);
    emitted.assert_passes();
    let native = native(&script);
    let (emitted_lines, native_lines): (Vec<&str>, Vec<&str>) =
        (emitted.stdout.lines().collect(), native.lines().collect());
    assert_eq!(native_lines.len(), AGREED + 4, "native:\n{native}");
    assert_eq!(emitted_lines.len(), AGREED + 4, "emitted:\n{}", emitted.stdout);
    for (e, n) in emitted_lines[..AGREED].iter().zip(&native_lines[..AGREED]) {
        assert_eq!(e, n, "emitted:\n{}\nnative:\n{native}", emitted.stdout);
    }
    // What the native run answers, so a change on both sides still shows.
    for expected in [
        "comment_value nil: nil",
        "comment_value '12': \"12\"",
        "comment_present nil: false",
        "comment_present 12: true",
        "comment_present '  ': false",
        "comment_blank '  ': true",
        "comment_blank 0: false",
        "comment_presence '': nil",
        "comment_nil nil: true",
        "comment_to_s nil: \"\"",
        "comment_truthy nil: :no",
        "comment_truthy 0: :yes",
        "comment_nilable_present '': false",
        "comment_bool 0: [0, false]",
        "comment_bool '': [\"\", true]",
        "comment_array_present []: false",
        "comment_ivar_present nil: false",
        "comment_thing_present nil: false",
        "comment_thing_present thing: true",
        "tcast_value 12: 12",
        "tlet_blank '  ': true",
    ] {
        assert!(native_lines.contains(&expected), "missing {expected:?} in native:\n{native}");
    }
    // The pinned divergence: sorbet's class check, which the tree does not
    // repeat.
    assert_eq!(
        native_lines[AGREED..],
        [
            "tcast_value nil: [\"TypeError\", \"T.cast: Expected type Integer, got type NilClass\"]",
            "tcast_value '12': [\"TypeError\", \"T.cast: Expected type Integer, got type String with value \\\"12\\\"\"]",
            "tcast_present nil: [\"TypeError\", \"T.cast: Expected type Integer, got type NilClass\"]",
            "tlet_blank nil: [\"TypeError\", \"T.let: Expected type String, got type NilClass\"]",
        ],
        "native:\n{native}"
    );
    assert_eq!(
        emitted_lines[AGREED..],
        ["tcast_value nil: nil", "tcast_value '12': \"12\"", "tcast_present nil: false", "tlet_blank nil: true"],
        "emitted:\n{}",
        emitted.stdout
    );
}

/// The emitted Ruby: every author cast is its value, and no predicate is
/// folded on a type only a cast declared.
#[test]
fn author_casts_emit_the_value_and_keep_the_runtime_predicate() {
    let (emitted, errors) = app().emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let caster = std::fs::read_to_string(emitted.join("app/models/caster.rb")).unwrap();
    for conversion in [".to_i", "&.to_i", "include?(", "T.cast", "T.let"] {
        assert!(!caster.contains(conversion), "{conversion} in\n{caster}");
    }
    for expected in [
        "    @held = values.first\n",
        "  def comment_value\n    n = @values.first\n    n\n  end\n",
        "  def comment_present\n    n = @values.first\n    ActiveSupport.present?(n)\n  end\n",
        "  def comment_blank\n    s = @values.first\n    ActiveSupport.blank?(s)\n  end\n",
        "  def comment_presence\n    s = @values.first\n    ActiveSupport.presence(s)\n  end\n",
        "  def comment_nil\n    n = @values.first\n    n.nil?\n  end\n",
        "  def comment_to_s\n    n = @values.first\n    n.to_s\n  end\n",
        "  def comment_nilable_present\n    n = @values.first\n    ActiveSupport.present?(n)\n  end\n",
        "  def comment_array_present\n    a = @values.first\n    ActiveSupport.present?(a)\n  end\n",
        "  def comment_ivar_present\n    ActiveSupport.present?(@held)\n  end\n",
        "  def comment_thing_present\n    t = @values.first\n    !(t.nil?)\n  end\n",
        "  def tcast_value\n    @values.first\n  end\n",
        "  def tcast_present\n    ActiveSupport.present?(@values.first)\n  end\n",
        "  def tlet_blank\n    s = @values.first\n    ActiveSupport.blank?(s)\n  end\n",
    ] {
        assert!(caster.contains(expected), "missing {expected:?} in\n{caster}");
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

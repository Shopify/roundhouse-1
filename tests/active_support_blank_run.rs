//! `blank?` / `present?` / `presence` on a receiver inference has no type
//! for (`lower::blank` routes it to `ActiveSupport.blank?(value)`),
//! against the same program on Core's ActiveSupport and ActiveRecord
//! (8.2.0.alpha, `tests/support/rails_oracle.rb`).
//!
//! The runtime used to answer `value.to_s.strip.empty?` for everything
//! but nil, false, Array and Hash: an object with its own `empty?` was
//! judged by its `to_s`, one whose `to_s` is blank was blank, `"\u00a0"`
//! was present and `"\0"` blank, and an empty Relation was present.
//! Rails answers class by class (core_ext/object/blank.rb), and an
//! object's own `blank?` wins.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;
use std::process::Command;

const LIB: &str = r##"class OwnEmpty
  def initialize(empty)
    @empty = empty
  end

  def empty?
    @empty
  end
end

class BlankToS
  def to_s
    "   "
  end
end

class OwnBlank
  def blank?
    true
  end
end

class Blanks
  def initialize(values)
    @values = values
  end

  def blank
    v = @values.first
    v.blank?
  end

  def present
    v = @values.first
    v.present?
  end

  def presence
    v = @values.first
    v.presence
  end
end
"##;

/// The value table, defined after the app (or the native models) load.
const VALUES: &str = r##"def seen(label)
  value = begin
    yield
  rescue StandardError => e
    [e.class.name, e.message]
  end
  puts "#{label}: #{value.inspect.gsub(/0x\h+/, "0x")}"
end
VALUES = {
  "nil" => nil, "false" => false, "true" => true,
  "''" => "", "'  '" => "  ", "'\\t\\n'" => "\t\n", "nbsp" => "\u00a0", "ideographic" => "\u3000", "nul" => "\0",
  "zwsp" => "\u200b", "'x'" => "x", "u16" => "  ".encode("UTF-16LE"), "u16x" => " x".encode("UTF-16LE"),
  "[]" => [], "[nil]" => [nil], "{}" => {}, "{a: nil}" => { a: nil }, ":''" => :"", ":a" => :a,
  "0" => 0, "0.0" => 0.0, "rational" => Rational(0), "time" => Time.at(0),
  "own_empty" => OwnEmpty.new(true), "own_full" => OwnEmpty.new(false), "own_nil_empty" => OwnEmpty.new(nil),
  "blank_to_s" => BlankToS.new, "own_blank" => OwnBlank.new, "object" => Object.new,
}
"##;

const SCRIPT: &str = r##"Article.create!(title: "present", body: "abcdefghij")
VALUES["empty_relation"] = Article.where(title: "absent")
VALUES["relation"] = Article.where(title: "present")
VALUES.each do |label, v|
  b = Blanks.new([v])
  seen("blank #{label}") { b.blank }
  seen("present #{label}") { b.present }
  seen("presence #{label}") { r = b.presence; r.equal?(v) ? :self : r }
end
"##;

/// The library at `lib/blanks.rb` of the blog.
fn app() -> emit_and_run::Overlay {
    emit_and_run::real_blog().write("lib/blanks.rb", LIB)
}

/// Run `program` in a scratch directory holding `lib/blanks.rb`.
fn ruby_in_scratch(name: &str, program: &str) -> String {
    let dir = std::env::temp_dir().join(format!("rh_as_blank_{name}_{}", std::process::id()));
    std::fs::create_dir_all(dir.join("lib")).unwrap();
    std::fs::write(dir.join("lib/blanks.rb"), LIB).unwrap();
    std::fs::write(dir.join("main.rb"), program).unwrap();
    let out = Command::new("ruby").arg("main.rb").current_dir(&dir).output().expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "{name} run failed:\n{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    std::fs::remove_dir_all(&dir).unwrap();
    stdout
}

/// The program on Core's ActiveSupport and ActiveRecord: the
/// `Object#blank?` reopens and a real Relation over in-memory SQLite.
fn native() -> String {
    let oracle = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/rails_oracle.rb");
    ruby_in_scratch(
        "native",
        &format!(
            "require {oracle:?}\nRailsOracle.activate(\"activesupport\", \"activerecord\")\n\
             require \"active_support\"\nrequire \"active_support/core_ext/object/blank\"\nrequire \"active_record\"\n\
             ActiveRecord::Base.establish_connection(adapter: \"sqlite3\", database: \":memory:\")\n\
             ActiveRecord::Schema.verbose = false\n\
             ActiveRecord::Schema.define {{ create_table(:articles) {{ |t| t.string :title; t.text :body }} }}\n\
             class Article < ActiveRecord::Base; end\n\
             require_relative \"lib/blanks\"\n{VALUES}{SCRIPT}"
        ),
    )
}

const LINES: usize = 3 * 31;

#[test]
fn untyped_blank_answers_like_rails() {
    let emitted = app().run_ruby(&format!("{VALUES}{SCRIPT}"));
    emitted.assert_passes();
    let native = native();
    let (emitted_lines, native_lines): (Vec<&str>, Vec<&str>) =
        (emitted.stdout.lines().collect(), native.lines().collect());
    assert_eq!(native_lines.len(), LINES, "native:\n{native}");
    assert_eq!(emitted_lines, native_lines, "emitted:\n{}\nnative:\n{native}", emitted.stdout);
    // What Rails answers, so a change on both sides still shows.
    for expected in [
        "blank '  ': true",
        "blank nbsp: true",
        "blank ideographic: true",
        "blank nul: false",
        "blank zwsp: false",
        "blank u16: true",
        "blank u16x: false",
        "blank :'': true",
        "blank time: false",
        "blank own_empty: true",
        "blank own_full: false",
        "blank own_nil_empty: false",
        "blank blank_to_s: false",
        "blank own_blank: true",
        "present own_blank: false",
        "blank object: false",
        "blank empty_relation: true",
        "presence empty_relation: nil",
        "blank relation: false",
        "presence relation: :self",
        "presence own_full: :self",
    ] {
        assert!(native_lines.contains(&expected), "missing {expected:?} in native:\n{native}");
    }
    let (tree, errors) = app().emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let blanks = std::fs::read_to_string(tree.join("app/models/blanks.rb")).unwrap();
    assert!(blanks.contains("ActiveSupport.blank?(v)"), "the untyped site is the runtime's:\n{blanks}");
}

/// The shared body the spinel tree compiles (runtime/ruby/active_support_ext.rb,
/// before the CRuby/JRuby trees replace it with `value.blank?`), on plain
/// Ruby with no `Object#blank?`: Rails' answer for every value but the
/// two its header names, an object's own `blank?` without `empty?` and
/// a String the UTF-8 regex cannot read.
#[test]
fn the_shared_body_answers_like_rails_class_by_class() {
    let shared = concat!(env!("CARGO_MANIFEST_DIR"), "/runtime/ruby/active_support_ext.rb");
    let script = format!(
        "load {shared:?}\nrequire_relative \"lib/blanks\"\n{VALUES}\
         VALUES.each {{ |label, v| seen(\"blank #{{label}}\") {{ ActiveSupport.blank?(v) }} }}\n"
    );
    let shared_out = ruby_in_scratch("shared", &script);
    let native = native();
    let native_blank: Vec<&str> =
        native.lines().filter(|l| l.starts_with("blank ") && !l.contains("relation:")).collect();
    let shared_lines: Vec<&str> = shared_out.lines().collect();
    assert_eq!(shared_lines.len(), native_blank.len(), "shared:\n{shared_out}\nnative:\n{native}");
    for (s, n) in shared_lines.iter().zip(&native_blank) {
        let gap = n.starts_with("blank own_blank:") || n.starts_with("blank u16");
        if gap {
            assert_ne!(s, n, "a named gap now agrees; update the header:\n{shared_out}");
        } else {
            assert_eq!(s, n, "shared:\n{shared_out}\nnative:\n{native}");
        }
    }
}

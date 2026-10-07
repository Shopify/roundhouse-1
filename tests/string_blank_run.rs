//! A String `blank?` on the CRuby tree, against the same program run
//! natively on Core's ActiveSupport (CORE_GEM_HOME,
//! `tests/support/rails_oracle.rb`).
//!
//! `lower::blank` grounds a String-typed `blank?` / `present?` /
//! `presence` / `compact_blank` as `r.strip.empty?`, for the targets
//! with no `String#blank?`. Rails' is `/\A[[:space:]]*\z/`, retried in
//! the string's own encoding when that raises
//! `Encoding::CompatibilityError`; `strip` takes NUL and only ASCII
//! space, so U+00A0, U+3000, U+2028 and a UTF-16 U+00A0 read present
//! and `"\0"` blank, each the other way round from Rails. The CRuby and JRuby
//! trees write Rails' `r.blank?`, which their overlay's `String#blank?`
//! answers; the spinel tree keeps `strip.empty?`, a gap the
//! `lower::blank` header names.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;
use std::process::Command;

const LIB: &str = r##"class BlankProbe
  extend T::Sig

  sig { params(value: String).returns(T::Boolean) }
  def blank(value)
    value.blank?
  end

  sig { params(value: String).returns(T::Boolean) }
  def present(value)
    value.present?
  end

  sig { params(value: String).returns(T.nilable(String)) }
  def presence(value)
    value.presence
  end

  sig { params(value: T.nilable(String)).returns(T::Boolean) }
  def nilable_blank(value)
    value.blank?
  end

  sig { params(value: T.nilable(String)).returns(T::Boolean) }
  def nilable_present(value)
    value.present?
  end

  sig { params(value: String).returns(T::Array[String]) }
  def compact(value)
    [value, "x"].compact_blank
  end
end
"##;

const SCRIPT: &str = r##"def seen(label)
  value = begin
    yield
  rescue StandardError => e
    [e.class.name, e.message.sub(/\nCaller: .*\z/m, "")]
  end
  puts "#{label}: #{value.inspect}"
end
VALUES = {
  "empty" => "", "space" => " ", "ascii_ws" => "\t\n\v\f\r ", "nbsp" => "\u00A0",
  "ideographic" => "\u3000", "line_sep" => "\u2028", "nul" => "\0", "padded" => " a ",
  "word" => "a", "utf16_space" => " ".encode("UTF-16LE"), "utf16_nbsp" => "\u00A0".encode("UTF-16LE"),
  "utf16_word" => "x".encode("UTF-16LE"), "binary_space" => " ".b, "invalid" => "\xff".dup.force_encoding("UTF-8"),
}
%i[blank present presence nilable_blank nilable_present compact].each do |m|
  VALUES.each { |label, value| seen("#{m} #{label}") { BlankProbe.new.public_send(m, value) } }
end
seen("nilable_blank nil") { BlankProbe.new.nilable_blank(nil) }
seen("nilable_present nil") { BlankProbe.new.nilable_present(nil) }
"##;

const LINES: usize = 6 * 14 + 2;

fn app() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("app/.keep", "")
        .write("test/test_helper.rb", "require \"active_support/test_case\"\n")
        .write("lib/blank_probe.rb", LIB)
}

/// The program on Core's ActiveSupport and sorbet-runtime.
fn native(script: &str) -> String {
    let oracle = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/rails_oracle.rb");
    // Each call gets its own directory, so concurrent tests never share one.
    static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rh_string_blank_native_{}_{seq}", std::process::id()));
    std::fs::create_dir_all(dir.join("lib")).unwrap();
    std::fs::write(dir.join("lib/blank_probe.rb"), LIB).unwrap();
    let program = format!(
        "require {oracle:?}\nRailsOracle.activate(\"activesupport\")\nrequire \"sorbet-runtime\"\n\
         abort \"native: activesupport is not Core's\" unless Gem.loaded_specs[\"activesupport\"].full_gem_path.start_with?(RailsOracle::CORE_GEM_HOME)\n\
         require \"active_support\"\nrequire \"active_support/core_ext/object/blank\"\nrequire \"active_support/core_ext/enumerable\"\n\
         require_relative \"lib/blank_probe\"\n{script}"
    );
    std::fs::write(dir.join("native.rb"), program).unwrap();
    let out = Command::new("ruby").arg("native.rb").current_dir(&dir).output().expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "native run failed:\n{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    std::fs::remove_dir_all(&dir).unwrap();
    stdout
}

/// The emitted file that defines `class BlankProbe`.
fn probe_source(root: &std::path::Path) -> String {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rb")
                && let Ok(text) = std::fs::read_to_string(&path)
                && text.contains("class BlankProbe")
            {
                return text;
            }
        }
    }
    panic!("no emitted class BlankProbe under {}", root.display());
}

#[test]
fn a_string_blank_answers_like_the_native_run() {
    let emitted = app().run_ruby(SCRIPT);
    emitted.assert_passes();
    let native = native(SCRIPT);
    let (emitted_lines, native_lines): (Vec<&str>, Vec<&str>) =
        (emitted.stdout.lines().collect(), native.lines().collect());
    assert_eq!(native_lines.len(), LINES, "native:\n{native}");
    assert_eq!(emitted_lines.len(), LINES, "emitted:\n{}", emitted.stdout);
    for (e, n) in emitted_lines.iter().zip(&native_lines) {
        assert_eq!(e, n, "emitted:\n{}\nnative:\n{native}", emitted.stdout);
    }
    // What the native run answers, where `strip.empty?` answers otherwise.
    for expected in [
        "blank nbsp: true",
        "blank ideographic: true",
        "blank line_sep: true",
        "blank nul: false",
        "blank utf16_space: true",
        "blank utf16_nbsp: true",
        "blank utf16_word: false",
        "present nbsp: false",
        "presence nul: \"\\u0000\"",
        "nilable_blank nbsp: true",
        "nilable_blank nil: true",
        "compact nbsp: [\"x\"]",
        "blank invalid: [\"ArgumentError\", \"invalid byte sequence in UTF-8\"]",
    ] {
        assert!(native_lines.contains(&expected), "missing {expected:?} in native:\n{native}");
    }
    // The grounded form is Rails' own send, not the strip.
    let source = probe_source(&emitted.emitted);
    assert!(source.contains(".blank?"), "{source}");
    assert!(!source.contains("strip"), "{source}");
}

#[test]
fn the_spinel_tree_keeps_the_strip() {
    let (emitted, errors) = app().emit(BuildTarget::Spinel);
    let _ = errors;
    let source = probe_source(&emitted);
    assert!(source.contains(".strip.empty?"), "{source}");
}

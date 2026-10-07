//! The three non-nil assertions Core writes (`T.must(x)`,
//! `T.must_because(x) { reason }` and `x #: as !nil`) in the emitted Ruby,
//! against the same program run natively on Core's sorbet-runtime
//! (CORE_GEM_HOME, the gem Core bundles).
//!
//! The two sorbet calls agree with sorbet-runtime on every observation:
//! a present value (`false` included) passes through, evaluated once, and
//! nil raises TypeError with sorbet's message. `T.must_because`'s reason
//! runs only on nil, in the caller's scope. `#: as !nil` agrees on present
//! values. On nil it is the one pinned divergence: natively the comment
//! is inert and nil runs on, and the emitted tree raises TypeError at the
//! assertion (tests/rbs_as_not_nil_check.rs says why).
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;
use std::process::Command;

const LIB: &str = r##"class Counter
  attr_reader :reasons, :reads

  def initialize(counts)
    @counts = counts
    @reasons = 0
    @reads = 0
  end

  def read
    @reads += 1
    @counts.first
  end

  def must_first
    T.must(read)
  end

  def because(wanted)
    T.must_because(@counts.find { |c| c == wanted }) { "no count #{wanted} (#{@reasons += 1})" }
  end

  def because_do
    found = T.must_because(read) do
      "the counts are empty"
    end
    found
  end

  def comment_first
    n = read #: as !nil
    n
  end

  def comment_chained
    first = read #: as !nil
    first.to_s
  end
end
"##;

/// One observation per line, each `label: value`; an exception is shown
/// as its class and message.
const SCRIPT: &str = r##"def seen(label)
  value = begin
    yield
  rescue StandardError => e
    [e.class.name, e.message]
  end
  puts "#{label}: #{value.inspect}"
end
present = Counter.new([3, 4])
seen("must present") { [present.must_first, present.reads] }
seen("must false") { Counter.new([false]).must_first }
seen("must nil") { Counter.new([]).must_first }
seen("because present") { [present.because(4), present.reasons] }
seen("because nil") { [present.because(9), present.reasons] }
seen("because nil again") { present.because(8) }
seen("reasons after nil") { present.reasons }
seen("because_do present") { Counter.new([false]).because_do }
seen("because_do nil") { Counter.new([]).because_do }
seen("comment present") { c = Counter.new([5]); [c.comment_first, c.reads] }
seen("comment chained") { Counter.new([6]).comment_chained }
"##;

/// The one observation `#: as !nil` is allowed to differ on: nil.
const COMMENT_NIL: &str = r##"seen("comment nil") { Counter.new([]).comment_first }
"##;

fn app() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("app/.keep", "")
        .write("test/test_helper.rb", "require \"active_support/test_case\"\n")
        .write("lib/counter.rb", LIB)
}

fn native(script: &str) -> String {
    let gems = std::env::var("CORE_GEM_HOME")
        .unwrap_or_else(|_| "/nix/store/kr4swvh5mw01fs3kixfa9afy57zsyihg-ruby-gems-dev/lib/ruby/gems/4.0.0".into());
    let dir = std::env::temp_dir().join(format!("rh_non_nil_native_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("counter.rb"), LIB).unwrap();
    let program = format!(
        "require \"sorbet-runtime\"\nabort \"native: sorbet-runtime is not Core's\" unless Gem.loaded_specs[\"sorbet-runtime\"].full_gem_path.start_with?({gems:?})\nrequire_relative \"counter\"\n{script}"
    );
    std::fs::write(dir.join("native.rb"), program).unwrap();
    let out = Command::new("ruby")
        .arg("native.rb")
        .current_dir(&dir)
        .env("GEM_HOME", &gems)
        .env("GEM_PATH", &gems)
        .output()
        .expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "native run failed:\n{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    std::fs::remove_dir_all(&dir).unwrap();
    stdout
}

#[test]
fn non_nil_assertions_answer_like_sorbet_runtime() {
    let script = format!("{SCRIPT}{COMMENT_NIL}");
    let emitted = app().run_ruby(&script);
    emitted.assert_passes();
    let native = native(&script);
    let (emitted_lines, native_lines): (Vec<&str>, Vec<&str>) = (emitted.stdout.lines().collect(), native.lines().collect());
    assert_eq!(emitted_lines.len(), 12, "emitted:\n{}", emitted.stdout);
    assert_eq!(native_lines.len(), 12, "native:\n{native}");
    // Every observation but the last agrees with sorbet-runtime.
    assert_eq!(emitted_lines[..11], native_lines[..11], "emitted:\n{}\nnative:\n{native}", emitted.stdout);
    for expected in [
        "must present: [3, 1]",
        "must false: false",
        "must nil: [\"TypeError\", \"Passed `nil` into T.must\"]",
        "because present: [4, 0]",
        "because nil: [\"TypeError\", \"Unexpected `nil` because no count 9 (1)\"]",
        "because nil again: [\"TypeError\", \"Unexpected `nil` because no count 8 (2)\"]",
        "reasons after nil: 2",
        "because_do present: false",
        "because_do nil: [\"TypeError\", \"Unexpected `nil` because the counts are empty\"]",
        "comment present: [5, 1]",
        "comment chained: \"6\"",
    ] {
        assert!(emitted_lines.contains(&expected), "missing {expected:?} in\n{}", emitted.stdout);
    }
    // The pinned divergence: nil runs on natively, the tree stops it.
    assert_eq!(native_lines[11], "comment nil: nil");
    assert_eq!(emitted_lines[11], "comment nil: [\"TypeError\", \"Passed `nil` into `#: as !nil`\"]");
}

/// The emitted Ruby keeps the receiver and adds the check once per
/// assertion: a written value's own trailing comment and the write's are
/// one comment, not two checks.
#[test]
fn each_assertion_is_one_check_in_the_emitted_ruby() {
    let (emitted, errors) = app().emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let counter = std::fs::read_to_string(emitted.join("app/models/counter.rb")).unwrap();
    assert_eq!(counter.matches("Passed `nil` into `#: as !nil`").count(), 2, "{counter}");
    assert_eq!(counter.matches("Passed `nil` into T.must\"").count(), 1, "{counter}");
    assert_eq!(counter.matches("Unexpected `nil` because ").count(), 2, "{counter}");
    assert!(counter.contains("= read).nil?"), "the receiver is evaluated as written: {counter}");
    assert!(!counter.contains("T.must(") && !counter.contains("T.must_because("), "no tree loads sorbet-runtime: {counter}");
}

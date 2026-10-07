//! One class reopened across files: shopify-money opens `class Money`
//! in about 18 files. `money.rb` holds the body, `version.rb` adds a
//! constant, and the rest wrap a nested class. Ruby has one class
//! object, and every reopen adds to it. Ingest recorded one library
//! class per declaration, and all of them emitted to the same
//! `app/models/money.rb`, so the last file written won. An empty
//! namespace reopen replaced the whole body, and no diagnostic was
//! reported. The emitted tree must print the same lines, in the same
//! order, as CRuby loading the authored files.
//!
//! A method two of the files both define is another matter: Ruby keeps
//! the def of the file it loads last, and the merge's walk order is not
//! shown to be that order, so it is reported.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const FILES: &[(&str, &str)] = &[
    (
        "app/services/reopen_cash.rb",
        r##"class ReopenCash
  include Comparable
  attr_reader :value

  def initialize(value)
    @value = value
  end

  def <=>(other)
    value <=> other.value
  end

  def label
    "ReopenCash(#{value}) v#{VERSION}"
  end
  alias_method :to_s, :label
end
"##,
    ),
    (
        "app/services/reopen_cash_part.rb",
        r#"class ReopenCash
  class Part
    def self.of(cash)
      "part of " + cash.value.to_s
    end
  end
end
"#,
    ),
    (
        "app/services/reopen_cash_version.rb",
        r#"class ReopenCash
  VERSION = "1.2"
end
"#,
    ),
    (
        "app/services/reopen_cash_zlater.rb",
        r#"class ReopenCash
  def doubled
    ReopenCash.new(value * 2)
  end

  def kind
    "early"
  end

  def kind
    "later"
  end
end
"#,
    ),
];

const SCRIPT: &str = r##"
a = ReopenCash.new(3)
puts "body kept: #{a.label}"
puts "alias kept: #{a.to_s}"
puts "comparable kept: #{a < ReopenCash.new(4)}"
puts "constant from another file: #{ReopenCash::VERSION}"
puts "nested from another file: #{ReopenCash::Part.of(a)}"
puts "method from another file: #{a.doubled.value}"
puts "later def in one file wins: #{a.kind}"
"##;

fn native_lines() -> Vec<String> {
    let mut source = String::new();
    for (_, text) in FILES {
        source.push_str(text);
        source.push('\n');
    }
    let native = emit_and_run::ruby()
        .arg("-e")
        .arg(format!("{source}\n{SCRIPT}"))
        .output()
        .expect("CRuby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    String::from_utf8_lossy(&native.stdout).lines().map(str::to_string).collect()
}

#[test]
fn a_class_reopened_across_files_keeps_every_body_as_in_cruby() {
    let expected = native_lines();
    for probe in [
        "body kept: ReopenCash(3) v1.2",
        "comparable kept: true",
        "nested from another file: part of 3",
        "method from another file: 6",
        "later def in one file wins: later",
    ] {
        assert!(expected.iter().any(|line| line == probe), "native control lacks {probe:?}: {expected:#?}");
    }

    let mut app = emit_and_run::real_blog();
    for (path, text) in FILES {
        app = app.write(path, text);
    }
    let run = app.run_ruby(SCRIPT);
    run.assert_passes();
    let actual: Vec<&str> = run.stdout.lines().collect();
    assert_eq!(actual, expected, "emitted Ruby diverges from CRuby line for line");
}

/// The class's own file holds every reopen's methods and its constant.
/// The nested class keeps a file of its own, since no reopen that holds
/// it is the class's body file.
#[test]
fn the_reopens_emit_as_one_class_body() {
    let mut app = emit_and_run::real_blog();
    for (path, text) in FILES {
        app = app.write(path, text);
    }
    let (tree, _errors) = app.emit(roundhouse::project::BuildTarget::Ruby);
    let emitted = std::fs::read_to_string(tree.join("app/models/reopen_cash.rb")).expect("app/models/reopen_cash.rb");
    for piece in ["def initialize(value)", "def doubled", "VERSION = \"1.2\"", "alias_method :to_s, :label"] {
        assert!(emitted.contains(piece), "{piece:?} missing from the emitted class:\n{emitted}");
    }
}

/// Two files define the same method: which one Ruby keeps depends on
/// the load order, so the later def is a warning at its site. The
/// same-file `kind` redefinition follows source order and is not.
#[test]
fn a_method_two_reopening_files_define_is_a_warning() {
    let mut source = roundhouse::det::HashMap::default();
    for (path, text) in FILES {
        source.insert(std::path::PathBuf::from(*path), text.as_bytes().to_vec());
    }
    let late = "class ReopenCash\n  def label\n    \"other file\"\n  end\nend\n";
    source.insert(std::path::PathBuf::from("app/services/reopen_cash_zz.rb"), late.as_bytes().to_vec());
    let app = roundhouse::ingest::ingest_app_from_tree(source).expect("ingest the reopens");
    let diagnostics = roundhouse::analyze::diagnose(&app);
    let reported: Vec<_> = diagnostics.iter().filter(|d| d.message.contains("method redefined across reopened files")).collect();
    assert_eq!(reported.len(), 1, "only label, and not the same-file kind: {reported:#?}");
    assert!(reported[0].message.contains("`label`"), "{reported:#?}");
    assert_eq!(reported[0].severity, roundhouse::diagnostic::Severity::Warning);
}

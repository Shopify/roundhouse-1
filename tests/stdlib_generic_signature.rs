//! A parameterized stdlib generic in a signature is the stdlib class,
//! whatever the lexical scope holds.
//!
//! Core's `module Metafields` defines `Metafields::Set`, and its computed
//! values declare `#: (String) -> Set[String]` and call `.include?` on
//! the answer. Sorbet reads `Set[String]` there as `T::Set[String]`
//! (`T.reveal_type`, srb 0.6.13502); only the bare `#: -> Set` is
//! `Metafields::Set`. The signature reader resolved both lexically, so
//! the value was a `Metafields::Set` without `include?`, and the send was
//! refused as Module protocol. A `#:` cast reads its type the same way.
//! Native Ruby on the same source is the oracle.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const METAFIELDS: &str = r#"module Metafields
  class Set
    def initialize(name)
      @name = name
    end
  end

  class Evaluator
    EMPTY = ::Set.new(["title"]).freeze

    class << self
      #: (String) -> Set[String]
      def exposed_attributes(owner_type)
        EMPTY
      end

      #: (String, String) -> bool
      def exposed?(owner_type, name)
        exposed_attributes(owner_type).include?(name)
      end

      #: (String) -> bool
      def cast_exposed?(name)
        names = EMPTY #: Set[String]
        names.include?(name)
      end
    end
  end
end
"#;

const REPORT: &str =
    "p [Metafields::Evaluator.exposed?(\"product\", \"title\"), Metafields::Evaluator.exposed?(\"product\", \"sku\"), Metafields::Evaluator.cast_exposed?(\"title\")]\n";

#[test]
fn a_parameterized_set_signature_is_the_stdlib_set() {
    let dir = std::env::temp_dir().join(format!("roundhouse-stdlib-generic-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("metafields.rb");
    std::fs::write(&file, METAFIELDS).unwrap();
    let native = std::process::Command::new("ruby")
        .arg("-e")
        .arg(format!("require \"set\"\nrequire {:?}\n{REPORT}", file.display().to_string()))
        .output()
        .expect("native Ruby control");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    let expected = String::from_utf8_lossy(&native.stdout).into_owned();
    assert_eq!(expected, "[true, false, true]\n", "native control");

    let (emitted, errors) = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("lib/metafields.rb", METAFIELDS)
        .emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{}", errors.join("\n"));
    let output = emit_and_run::ruby()
        .arg("-e")
        .arg(format!(
            "require \"set\"\nrequire \"./app/models/metafields/set\"\nrequire \"./app/models/metafields/evaluator\"\n{REPORT}"
        ))
        .current_dir(&emitted)
        .output()
        .expect("spawn ruby");
    assert!(output.status.success(), "emitted program failed in {}:\n{}", emitted.display(), String::from_utf8_lossy(&output.stderr));
    assert_eq!(String::from_utf8_lossy(&output.stdout), expected);
}

/// The bare spelling still names the lexical class, as sorbet reads it:
/// `#: -> Set` inside `Metafields` is `Metafields::Set`, which has no
/// `include?`.
#[test]
fn a_bare_set_signature_still_resolves_lexically() {
    let source = METAFIELDS.replace("#: (String) -> Set[String]", "#: (String) -> Set");
    let (_, errors) = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("lib/metafields.rb", &source)
        .emit(BuildTarget::Ruby);
    assert!(
        errors.iter().any(|e| e.contains("include? requires a proven class/module object receiver")),
        "{}",
        errors.join("\n")
    );
}

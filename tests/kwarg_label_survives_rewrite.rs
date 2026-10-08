//! A keyword argument keeps its label when a later pass rebuilds its value.
//!
//! `lower::helper_kwargs` moves a call's keywords into the positional slots
//! of the flattened callee and marks each moved value with its keyword. The
//! ruby family writes the marked values back as `key: value`. The marks live
//! on the value, so a pass that rebuilds it loses them. The scope-chain
//! rewrite rebuilt every send in a body that also starts a model query.
//! Core's `Growth::Dsl::PlaybookCompiler#call` was emitted as
//! `Growth::Dsl.validate(@source, path: @path, Growth::Dsl.growth_schema)`,
//! and 83 files of the r17 emit had the same defect. Ruby rejects such a
//! call when it parses the file, so the boot stopped with a `SyntaxError`.
//!
//! Native Ruby on the same source is the oracle. The model query sits on a
//! branch that this program never takes, so the native run needs no
//! database and no `Article`. The emitted body still contains the query,
//! and that is what makes the rewrite visit it.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const DSL: &str = r#"module Kwlab
  class Schema
    attr_reader :name

    def initialize(name)
      @name = name
    end
  end

  class Registry
    def initialize
      @entries = []
    end

    def register(handle, value_kind: nil, value_kinds: nil, validator: nil)
      @entries << [handle, value_kind, value_kinds, validator && validator.call(2)]
      @entries.last
    end
  end

  class << self
    def validate(source, path: nil, schema: nil, strict: false)
      [source, path, schema && schema.name, strict]
    end

    def growth_schema
      Schema.new("growth")
    end
  end
end
"#;

const COMPILER: &str = r#"module Kwlab
  class Compiler
    def initialize(source, path)
      @source = source
      @path = path
    end

    def call
      return Article.where(title: @source).to_a if @source.nil?
      [
        Kwlab.validate(@source, path: @path, schema: Kwlab.growth_schema),
        Kwlab.validate(@source, schema: Kwlab.growth_schema, strict: @path.end_with?(".rb")),
        Kwlab.validate(@source, path: @path.upcase, strict: true),
      ]
    end

    def register(kind)
      return Article.where(title: kind).to_a if kind.nil?
      registry = Kwlab::Registry.new
      registry.register(:h, value_kind: kind.to_sym, validator: validator_for(kind))
    end

    private

    def validator_for(kind)
      ->(n) { kind * n }
    end
  end
end
"#;

const REPORT: &str = r#"compiler = Kwlab::Compiler.new("play", "a.rb")
p compiler.call
p compiler.register("ab")
"#;

#[test]
fn rebuilt_keyword_values_keep_their_labels_and_run_like_native() {
    let dir = std::env::temp_dir().join(format!("roundhouse-kwarg-label-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let dsl = dir.join("dsl.rb");
    let compiler = dir.join("compiler.rb");
    std::fs::write(&dsl, DSL).unwrap();
    std::fs::write(&compiler, COMPILER).unwrap();
    let native = std::process::Command::new("ruby")
        .arg("-e")
        .arg(format!(
            "require {:?}\nrequire {:?}\n{REPORT}",
            dsl.display().to_string(),
            compiler.display().to_string()
        ))
        .output()
        .expect("native Ruby control");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        native.status.success(),
        "{}",
        String::from_utf8_lossy(&native.stderr)
    );
    let expected = String::from_utf8_lossy(&native.stdout).into_owned();
    assert_eq!(
        expected,
        "[[\"play\", \"a.rb\", \"growth\", false], [\"play\", nil, \"growth\", true], [\"play\", \"A.RB\", nil, true]]\n\
         [:h, :ab, nil, \"abab\"]\n",
        "native control"
    );

    let (emitted, errors) = emit_and_run::real_blog()
        .write("app/lib/kwlab/dsl.rb", DSL)
        .write("app/lib/kwlab/compiler.rb", COMPILER)
        .emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{}", errors.join("\n"));
    let source = std::fs::read_to_string(emitted.join("app/models/kwlab/compiler.rb"))
        .expect("emitted compiler");
    // The rewrite did visit these sends, or the run below proves nothing.
    assert!(
        source.contains("ActiveRecord::Relation.new(Article)"),
        "{source}"
    );
    for call in [
        "Kwlab.validate(@source, path: @path, schema: Kwlab.growth_schema)",
        r#"Kwlab.validate(@source, schema: Kwlab.growth_schema, strict: @path.end_with?(".rb"))"#,
        "Kwlab.validate(@source, path: @path.upcase, strict: true)",
        "registry.register(:h, value_kind: kind.to_sym, validator: validator_for(kind))",
    ] {
        assert!(source.contains(call), "missing `{call}` in:\n{source}");
    }

    let main = emitted.join("main").display().to_string();
    let output = emit_and_run::ruby()
        .arg("-e")
        .arg(format!(
            "require {main:?}\nMain.configure_default_adapter!\nrequire_relative \"app/models/kwlab/compiler\"\n{REPORT}"
        ))
        .current_dir(&emitted)
        .env("BLOG_DB", ":memory:")
        .output()
        .expect("spawn ruby");
    assert!(
        output.status.success(),
        "emitted program failed in {}:\n{}",
        emitted.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), expected);
}

/// The emit guard: every Ruby file of a ruby-family tree must parse.
#[test]
fn the_emit_guard_names_each_emitted_ruby_file_that_does_not_parse() {
    use roundhouse::project::{UnparsableRuby, unparsable_ruby_files};
    let files = vec![
        (
            "app/models/ok.rb".to_string(),
            "class Ok\n  def x = f(a, path: p)\nend\n".to_string(),
        ),
        (
            "app/models/dropped.rb".to_string(),
            "class Dropped\n  def x\n    f(a, path: p, g(b))\n  end\nend\n".to_string(),
        ),
        (
            "app/models/braced.rb".to_string(),
            "field :a, null: true { |x| x }\n".to_string(),
        ),
        ("config/routes.json".to_string(), "{ not ruby".to_string()),
    ];
    let bad = unparsable_ruby_files(&files, BuildTarget::Ruby);
    let found: Vec<(&str, usize)> = bad.iter().map(|u| (u.path.as_str(), u.line)).collect();
    assert_eq!(
        found,
        [("app/models/braced.rb", 1), ("app/models/dropped.rb", 3)],
        "{bad:?}"
    );
    assert!(
        bad.iter().all(|u: &UnparsableRuby| !u.message.is_empty()),
        "{bad:?}"
    );
    // Another target's output is not Ruby.
    assert!(unparsable_ruby_files(&files, BuildTarget::Typescript).is_empty());
}

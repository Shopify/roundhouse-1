//! A local of a module body keeps its scope for the constants after it.
//!
//! Core's `Podding::Database::SqlHelper` binds `build_union_regexp =
//! ->(parts) { … }` in the module body and builds a dozen constants by
//! calling it. Native Ruby on the same source is the oracle; the emitted
//! app must build the same constants. A local whose value a reader could
//! tell apart from a copy is refused by name rather than dropped.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;

const SQL_HELPER: &str = r##"module Podish
  module SqlHelper
    build_union_regexp = ->(parts) do
      parts = parts.map { |part| /\A\(?\s*#{part}/i }
      Regexp.union(parts)
    end

    build_prefixed = lambda do |parts|
      parts.map { |parts| "#{PREFIX}#{parts}" }.join(",")
    end

    PREFIX = "rh:"
    LIMIT = 3

    READ_VERBS = [:select, :show, :set].freeze
    READ_QUERY = build_union_regexp.call(READ_VERBS) #: Regexp
    WRITE_VERBS = [:insert, :update, :delete].freeze
    WRITE_QUERY = build_union_regexp.call(WRITE_VERBS) #: Regexp
    PREFIXED = build_prefixed.call(READ_VERBS.first(LIMIT))

    def self.read?(sql)
      READ_QUERY.match?(sql)
    end
  end
end
"##;

const REPORT: &str = r#"p Podish::SqlHelper::READ_QUERY
p Podish::SqlHelper::WRITE_QUERY
p Podish::SqlHelper::PREFIXED
p Podish::SqlHelper.read?("(  SELECT 1"), Podish::SqlHelper.read?("insert into t")
p Podish::SqlHelper::WRITE_QUERY.match?("UPDATE t")
"#;

#[test]
fn emitted_constants_match_native_ruby() {
    let dir = std::env::temp_dir().join(format!("roundhouse-body-locals-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("sql_helper.rb");
    std::fs::write(&file, SQL_HELPER).unwrap();
    let native = std::process::Command::new("ruby")
        .arg("-e")
        .arg(format!("require {:?}\n{REPORT}", file.display().to_string()))
        .output()
        .expect("native Ruby control");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    let expected = String::from_utf8_lossy(&native.stdout).into_owned();
    assert!(expected.contains("true\nfalse\ntrue\n"), "native control: {expected}");

    let (emitted, errors) =
        emit_and_run::real_blog().write("app/lib/podish/sql_helper.rb", SQL_HELPER).emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{}", errors.join("\n"));
    let main = emitted.join("main").display().to_string();
    let output = emit_and_run::ruby()
        .arg("-e")
        .arg(format!("require {main:?}\nMain.configure_default_adapter!\n{REPORT}"))
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

/// Plain `check` (no `--continue`): the refusal is an
/// `ingest_unsupported` error at the read, not a silently dropped file.
fn check(source: &str) -> String {
    let root = std::env::temp_dir().join(format!(
        "roundhouse-body-locals-refusal-{}-{}",
        std::process::id(),
        source.len()
    ));
    let _ = std::fs::remove_dir_all(&root);
    for (path, text) in [
        ("db/schema.rb", "ActiveRecord::Schema.define do\nend\n"),
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n"),
        ("app/lib/podish/registry.rb", source),
    ] {
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_roundhouse"))
        .arg("check")
        .arg(&root)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&root);
    format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
}

#[test]
fn an_observable_local_is_refused_by_name() {
    for (source, reason) in [
        (
            "module Podish\n  module Registry\n    store = Object.new\n    DEFAULT = store\n  end\nend\n",
            "registry.rb:4:15: error[ingest_unsupported]: class-body local `store` holds a value that is not a closure-free lambda or an immediate literal",
        ),
        (
            "module Podish\n  module Registry\n    base = 2\n    scale = ->(x) { x * base }\n    TWO = scale.call(1)\n  end\nend\n",
            "class-body local `scale` is a lambda that closes over `base`",
        ),
        (
            "module Podish\n  module Registry\n    build = ->(x) { x }\n    BUILDERS = [build]\n  end\nend\n",
            "class-body lambda `build` is read other than as the receiver of `.call`",
        ),
        (
            "module Podish\n  module Registry\n    n = 1\n    n = 2\n    N = n\n  end\nend\n",
            "class-body local `n` is assigned more than once",
        ),
    ] {
        let text = check(source);
        assert!(text.contains(reason), "missing `{reason}` in:\n{text}");
        assert!(text.contains("error[ingest_unsupported]"), "not an ingest_unsupported error:\n{text}");
    }
}

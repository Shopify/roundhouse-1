//! A local of a module body keeps its scope for the constants after it.
//!
//! Core's `Podding::Database::SqlHelper` binds `build_union_regexp =
//! ->(parts) { … }` in the module body and builds a dozen constants by
//! calling it. Native Ruby on the same source is the oracle; the emitted
//! app must build the same constants. Any other value is stored once and
//! read from there; a lambda that closes over a local, a lambda passed as
//! a value and a local assigned twice are refused by name rather than
//! dropped.

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
    check_with(source, &[])
}

fn check_with(source: &str, args: &[&str]) -> String {
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
        .args(args)
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
    // Survey mode goes on without the file too, and says so at the read.
    let survey = check_with(
        "module Podish\n  module Registry\n    n = 1\n    n = 2\n    N = n\n  end\nend\n",
        &["--continue"],
    );
    assert!(survey.contains("registry.rb:5:9: error[ingest_unsupported]: class-body local `n`"), "{survey}");
}

/// Any other local is evaluated once, where the source evaluated it, and
/// every reader sees that one object. Core's `Shopify::GeoIP` builds its
/// `LOAD_PATH` from `real_production = Rails.env.production? && …`;
/// Fulfillments models pass `params_accessors = %i[…]` to class-level
/// calls. Native Ruby on the same source is the oracle for the values,
/// identity, evaluation count, a read inside a block, and the module's
/// `constants` (a stored local is a private constant, not listed).
const STORED: &str = r#"module Stored
  log = []
  store = Object.new
  built = log.push(:built).size
  label = "Label"
  greeting = "Hello #{label}"
  real_production = ENV["RH_STORED_PRODUCTION"] == "1" && !ENV["CI"]

  DEFAULT = store
  ALSO = store
  COUNTS = [built, built].freeze
  LOG = log
  LABEL = label
  SAME_LABEL = label
  GREETING = greeting
  LOAD_PATH = ["vendor", *(real_production ? [] : ["fixtures"])].freeze
  SCALED = [1, 2].map { |x| x * built }

  def self.report
    [DEFAULT.equal?(ALSO), COUNTS, LOG, LABEL.equal?(SAME_LABEL), LABEL.frozen?, GREETING, LOAD_PATH, SCALED, constants.sort]
  end
end
"#;

const STORED_REPORT: &str = "p Stored.report\n";

#[test]
fn any_other_local_is_evaluated_once_as_in_native_ruby() {
    let dir = std::env::temp_dir().join(format!("roundhouse-body-locals-stored-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("stored.rb");
    std::fs::write(&file, STORED).unwrap();
    let native = std::process::Command::new("ruby")
        .arg("-e")
        .arg(format!("require {:?}\n{STORED_REPORT}", file.display().to_string()))
        .env_remove("RH_STORED_PRODUCTION")
        .output()
        .expect("native Ruby control");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    let expected = String::from_utf8_lossy(&native.stdout).into_owned();
    assert!(
        expected.starts_with("[true, [1, 1], [:built], true, false, \"Hello Label\", [\"vendor\", \"fixtures\"], [1, 2], [:ALSO,"),
        "native control: {expected}"
    );

    let (emitted, errors) = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("lib/stored.rb", STORED)
        .emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{}", errors.join("\n"));
    let output = emit_and_run::ruby()
        .arg("-e")
        .arg(format!("require \"./app/models/stored\"\n{STORED_REPORT}"))
        .current_dir(&emitted)
        .env_remove("RH_STORED_PRODUCTION")
        .output()
        .expect("spawn ruby");
    assert!(output.status.success(), "emitted program failed in {}:\n{}", emitted.display(), String::from_utf8_lossy(&output.stderr));
    assert_eq!(String::from_utf8_lossy(&output.stdout), expected);
}

/// Core's GraphQL types bind a description heredoc to a body local and
/// pass it to `field` (`display_name_description = <<~DESC`). Under
/// `# frozen_string_literal: true` a static literal is one frozen object,
/// so each read is `"…".freeze`: native Ruby on the same source is the
/// oracle for value, identity and frozenness.
const DESCRIPTIONS: &str = r#"# frozen_string_literal: true

module Descriptions
  display_name = <<~DESC
    The full name of the customer. If the first_name and
    last_name are not available, the email address.
  DESC
  short = "Short."

  NAME = display_name
  ALSO = display_name
  BOTH = [display_name, short].freeze

  def self.same?
    NAME.equal?(ALSO) && NAME.equal?(BOTH.first) && NAME.frozen?
  end
end
"#;

const DESCRIPTIONS_REPORT: &str = "p Descriptions::NAME, Descriptions::BOTH, Descriptions.same?\n";

#[test]
fn a_frozen_string_local_is_one_frozen_object_as_in_native_ruby() {
    let dir = std::env::temp_dir().join(format!("roundhouse-body-locals-frozen-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("descriptions.rb");
    std::fs::write(&file, DESCRIPTIONS).unwrap();
    let native = std::process::Command::new("ruby")
        .arg("-e")
        .arg(format!("require {:?}\n{DESCRIPTIONS_REPORT}", file.display().to_string()))
        .output()
        .expect("native Ruby control");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    let expected = String::from_utf8_lossy(&native.stdout).into_owned();
    assert!(expected.ends_with("true\n"), "native control: {expected}");

    let (emitted, errors) = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("lib/descriptions.rb", DESCRIPTIONS)
        .emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{}", errors.join("\n"));
    let output = emit_and_run::ruby()
        .arg("-e")
        .arg(format!("require \"./app/models/descriptions\"\n{DESCRIPTIONS_REPORT}"))
        .current_dir(&emitted)
        .output()
        .expect("spawn ruby");
    assert!(output.status.success(), "emitted program failed in {}:\n{}", emitted.display(), String::from_utf8_lossy(&output.stderr));
    assert_eq!(String::from_utf8_lossy(&output.stdout), expected);
}

/// A stored local whose value has no inferred type is refused at its
/// reads by the name the source wrote, not by its hidden constant's.
#[test]
fn an_untyped_stored_local_is_reported_by_its_source_name() {
    let text = check("module Podish\n  module Registry\n    reason = Ghost.new\n    FIRST = { reason: reason }\n  end\nend\n");
    assert!(text.contains("class-body local `reason` holds a value with no inferred type"), "{text}");
    assert!(!text.contains("RH_BODY_LOCAL"), "{text}");
}

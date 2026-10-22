use super::emit_and_run;

/// `validates :attr, acceptance:`. Rails' `AcceptanceValidator` adds
/// "must be accepted" unless `Array(accept).include?(value)`, with
/// `accept:` defaulting to `["1", true]` and `allow_nil:` to `true`. A
/// Boolean column is cast before the check, so `"1"` and `"yes"` are
/// `true` there and `""` is nil. Answers are Rails 8.1.4's for the same
/// model and records.
fn members() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n")
        .write(
            "db/schema.rb",
            "ActiveRecord::Schema.define do\n  create_table \"members\", force: :cascade do |t|\n    t.string \"name\"\n    t.boolean \"active\"\n  end\nend\n",
        )
        .write(
            "app/models/member.rb",
            r#"class Member < ApplicationRecord
  attr_accessor :agreement, :terms

  validates :agreement, acceptance: { allow_nil: false, accept: [true, 'true', '1'] }, on: :create
  validates :terms, acceptance: true
  validates :active, acceptance: true
end
"#,
        )
}

const MEMBERS: &str = r##"def errs(agreement, terms, active)
  m = Member.new
  m.name = "x"
  m.agreement = agreement
  m.terms = terms
  m.active = active
  m.valid?
  m.errors.to_a
end
got = [
  errs("1", nil, nil),
  errs(nil, "1", "1"),
  errs(true, true, true),
  errs("true", "true", "0"),
  errs("yes", "0", "yes"),
  errs(false, false, false),
]
want = [
  [],
  ["Agreement must be accepted"],
  [],
  ["Terms must be accepted", "Active must be accepted"],
  ["Agreement must be accepted", "Terms must be accepted"],
  ["Agreement must be accepted", "Terms must be accepted", "Active must be accepted"],
]
raise "member errors: #{got.inspect}" unless got == want
m = Member.new
m.name = "x"
m.agreement = "1"
m.save!
m.agreement = nil
raise "on: :create checked an update: #{m.errors.to_a.inspect}" unless m.valid?
puts "acceptance passed"
"##;

#[test]
fn acceptance_matches_rails() {
    let run = members().run_ruby(MEMBERS);
    run.assert_passes();
    assert!(run.stdout.contains("acceptance passed"), "{}", run.stdout);
}

/// The strict targets emit the check with no emit error. A Boolean
/// column compares only the accepted Booleans.
#[test]
fn strict_targets_emit_the_acceptance_check_without_errors() {
    use roundhouse::project::BuildTarget;
    for target in [BuildTarget::Rust, BuildTarget::Typescript, BuildTarget::Go, BuildTarget::Crystal] {
        let (emitted, errors) = members().emit(target);
        assert!(errors.is_empty(), "{target:?}: {errors:#?}");
        if target == BuildTarget::Rust {
            let member = std::fs::read_to_string(emitted.join("src/models/member.rs")).expect("member.rs");
            let validate = &member[member.find("pub fn validate(").expect("validate")..];
            let validate = &validate[..validate.find("\n    }\n").unwrap_or(validate.len())];
            let active = validate.lines().find(|l| l.contains("self.active")).expect("the active check");
            assert!(active.contains("== true"), "{active}");
            assert!(!active.contains("\"1\""), "{active}");
        }
    }
}

const SIGNUP_SCHEMA: &str =
    "ActiveRecord::Schema.define do\n  create_table \"signups\", force: :cascade do |t|\n    t.string \"name\"\n  end\nend\n";
const SIGNUP: &str = "class Signup < ApplicationRecord\n  attribute :consent, :boolean\n  validates :consent, acceptance: true\nend\n";
const APPLICATION_RECORD: &str = "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n";

/// `attribute :consent, :boolean` with no column: on the ruby family its
/// writer stores Rails' cast (nil and `""` are nil, and skipped), so the
/// check runs there. Rails 8.1.4's answers.
#[test]
fn acceptance_on_a_boolean_attribute_matches_rails() {
    let run = emit_and_run::empty_app()
        .write("app/models/application_record.rb", APPLICATION_RECORD)
        .write("db/schema.rb", SIGNUP_SCHEMA)
        .write("app/models/signup.rb", SIGNUP)
        .run_ruby(
            r##"got = ["1", "0", "yes", "", nil, true, false, "false"].map do |v|
  s = Signup.new
  s.name = "x"
  s.consent = v
  s.valid?
  s.errors.to_a
end
want = [[], ["Consent must be accepted"], [], [], [], [], ["Consent must be accepted"], ["Consent must be accepted"]]
raise "signup errors: #{got.inspect}" unless got == want
puts "attribute acceptance passed"
"##,
        );
    run.assert_passes();
    assert!(run.stdout.contains("attribute acceptance passed"), "{}", run.stdout);
}

/// A strict target's Boolean attribute field turns nil and `""` into
/// `false`, so there the check would reject what Rails skips: the rule
/// is left out and reported for that target, and the ruby target, which
/// runs it, reports nothing.
#[test]
fn acceptance_on_a_boolean_attribute_is_a_strict_target_gap() {
    let root = std::env::temp_dir().join(format!("roundhouse-acceptance-cli-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for (path, body) in [
        ("app/models/application_record.rb", APPLICATION_RECORD),
        ("app/models/signup.rb", SIGNUP),
        ("db/schema.rb", SIGNUP_SCHEMA),
    ] {
        let file = root.join("app_src").join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, body).unwrap();
    }
    let warnings = |target: &str| {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_roundhouse"))
            .args(["--target", target, "--allow-unsupported", "-o"])
            .arg(root.join(format!("out-{target}")))
            .arg(root.join("app_src"))
            .output()
            .expect("run roundhouse");
        String::from_utf8_lossy(&out.stderr)
            .lines()
            .filter(|l| l.contains("validates :consent, acceptance: not supported"))
            .count()
    };
    let rust = warnings("rust");
    let ruby = warnings("ruby");
    let rust_model = std::fs::read_to_string(root.join("out-rust/src/models/signup.rs")).unwrap_or_default();
    std::fs::remove_dir_all(&root).ok();
    assert_eq!(rust, 1, "the rule is reported on rust");
    assert_eq!(ruby, 0, "the ruby target lowers the check");
    assert!(!rust_model.contains("must be accepted"), "{rust_model}");
}

/// A name declared by `attribute` of another type is still the gap: its
/// writer stores the input verbatim, where Rails casts it.
#[test]
fn acceptance_on_a_string_attribute_is_a_reported_gap() {
    let warnings = emit_and_run::empty_app()
        .write("app/models/application_record.rb", APPLICATION_RECORD)
        .write("db/schema.rb", SIGNUP_SCHEMA)
        .write("app/models/signup.rb", "class Signup < ApplicationRecord\n  attribute :code, :string\n  validates :code, acceptance: { accept: 'yes' }\nend\n")
        .ingest_warnings();
    assert!(
        warnings.iter().any(|w| w.contains("validates :code, acceptance:") && w.contains("uncast")),
        "{warnings:?}"
    );
}

/// An attribute the model does not declare is a reported gap that
/// refuses the whole call: Rails defines a virtual accessor for it, which
/// this build does not synthesize. The `presence:` beside it goes too.
#[test]
fn acceptance_on_an_undeclared_attribute_is_a_reported_gap() {
    let app = || emit_and_run::empty_app()
        .write("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n")
        .write(
            "db/schema.rb",
            "ActiveRecord::Schema.define do\n  create_table \"members\", force: :cascade do |t|\n    t.string \"name\"\n  end\nend\n",
        )
        .write(
            "app/models/member.rb",
            "class Member < ApplicationRecord\n  validates :agreement, acceptance: true, presence: true\nend\n",
        );
    let warnings = app().ingest_warnings();
    assert!(
        warnings.iter().any(|w| w.contains("validates :agreement, acceptance:") && w.contains("does not declare")),
        "{warnings:?}"
    );
    let (emitted, errors) = app().emit(roundhouse::project::BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let member = std::fs::read_to_string(emitted.join("app/models/member.rb")).expect("member.rb");
    assert!(!member.contains("must be accepted") && !member.contains("can't be blank"), "{member}");
}

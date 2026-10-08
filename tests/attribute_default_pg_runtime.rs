//! A direct `attribute :col[, :type], default: value` over a column
//! (Core's `attribute :state, :string, default: ACTIVE`, 55 such calls,
//! some in STI subclasses), against native Rails 8.2 on the same local
//! PostgreSQL database.
//!
//! A new record not given the attribute starts from the declared
//! default, cast through the attribute's type (`"7"` on an integer
//! column is 7), and a Proc default is called for each record. A given
//! value, `nil` included, wins, and a loaded record reads its row. The
//! declaration applies to its class and subclasses: `SpecialWidget#kind`
//! starts at "special", `Widget#kind` at the column's own default. The
//! model compiles the columns as retypable, its `initialize` asks the
//! class for a declared default, and the calls run as written at class
//! load. A call with other options stays refused.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;
#[path = "support/pg_probe.rs"]
mod pg_probe;

const STRUCTURE: &str = r#"CREATE TABLE widgets (
    id bigserial NOT NULL,
    type character varying,
    name character varying,
    state character varying DEFAULT 'old'::character varying,
    count integer,
    flag boolean,
    token character varying,
    kind character varying,
    CONSTRAINT widgets_pkey PRIMARY KEY (id)
);
"#;

const DATABASE_YML: &str = r#"default: &default
  adapter: postgresql
  encoding: unicode

test:
  <<: *default
  database: rh_e2e_attribute_default
"#;

const APPLICATION_RECORD: &str = r#"class ApplicationRecord < ActiveRecord::Base
  self.abstract_class = true
end
"#;

const WIDGET: &str = r#"class Widget < ApplicationRecord
  FRESH = "fresh"

  attribute :state, :string, default: FRESH
  attribute :count, :integer, default: "7"
  attribute :flag, :boolean, default: "1"
  attribute :token, default: -> { "t-" + name.to_s }
end
"#;

const SPECIAL_WIDGET: &str = r#"class SpecialWidget < Widget
  attribute :kind, :string, default: "special"
end
"#;

const PROBE: &str = r##"class AttributeDefaultProbe
  def self.lines
    out = []
    w = Widget.new
    out << "new: #{w.state.inspect} #{w.count.inspect} #{w.flag.inspect} #{w.token.inspect} #{w.kind.inspect}"
    given = Widget.new(state: "given", count: 2, flag: false, token: nil)
    out << "given: #{given.state.inspect} #{given.count.inspect} #{given.flag.inspect} #{given.token.inspect}"
    s = SpecialWidget.new
    out << "sti: #{s.kind.inspect} #{s.state.inspect} #{Widget.new.kind.inspect}"
    a = Widget.create!(name: "a")
    b = Widget.create!(name: "b", state: nil, count: nil)
    c = SpecialWidget.create!(name: "c")
    out << "raw: #{Widget.order(:id).map { |r| [r.name, r.state, r.count, r.flag, r.token, r.kind] }.inspect}"
    out << "loaded: #{Widget.find(b.id).state.inspect} #{Widget.find(b.id).count.inspect} #{Widget.find(c.id).class} #{Widget.find(c.id).kind.inspect}"
    out << "where: #{Widget.where(state: "fresh").count} #{Widget.where(count: 7).count} #{a.reload.flag.inspect}"
    out << "types: #{Widget.type_for_attribute(:count).type.inspect} #{Widget.type_for_attribute(:token).type.inspect}"
    out
  end
end
"##;

/// What native Rails 8.2 answers (checked on the task PostgreSQL), so two
/// identical wrong answers still fail.
const EXPECTED: [&str; 7] = [
    r#"new: "fresh" 7 true "t-Widget" nil"#,
    r#"given: "given" 2 false nil"#,
    r#"sti: "special" "fresh" nil"#,
    r#"raw: [["a", "fresh", 7, true, "t-Widget", nil], ["b", nil, nil, true, "t-Widget", nil], ["c", "fresh", 7, true, "t-Widget", "special"]]"#,
    r#"loaded: nil nil SpecialWidget "special""#,
    "where: 2 2 true",
    "types: :integer :string",
];

/// What native Rails requires, in order.
const LOADS: [&str; 4] = [
    "app/models/application_record",
    "app/models/widget",
    "app/models/special_widget",
    "app/models/attribute_default_probe",
];

const FILES: [(&str, &str); 8] = [
    ("roundhouse.yml", "database_adapter: postgresql\n"),
    ("config/database.yml", DATABASE_YML),
    ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
    ("db/structure.sql", STRUCTURE),
    ("app/models/application_record.rb", APPLICATION_RECORD),
    ("app/models/widget.rb", WIDGET),
    ("app/models/special_widget.rb", SPECIAL_WIDGET),
    ("app/models/attribute_default_probe.rb", PROBE),
];

fn overlay() -> emit_and_run::Overlay {
    FILES.iter().fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text))
}

/// `check`'s errors other than the PostgreSQL adapter notice every
/// PostgreSQL probe app carries. The `attribute` calls are claimed: they
/// run as written at class load.
fn real_errors(errors: &[String]) -> Vec<&String> {
    errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect()
}

/// The compiler side: the model's `initialize` asks for the declared
/// default of each named column, and the calls are carried.
#[test]
fn attribute_default_runs_at_class_load() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    let widget = std::fs::read_to_string(emitted.join("app/models/widget.rb")).unwrap();
    for expected in [
        r#"self.class._attribute_default("state")"#,
        r#"self.class._attribute_default("kind")"#,
        r#"["state", "count", "flag", "token", "kind"]"#,
        "  attribute :state, :string, default: FRESH\n",
    ] {
        assert!(widget.contains(expected), "missing {expected:?} in:\n{widget}");
    }
    let special = std::fs::read_to_string(emitted.join("app/models/special_widget.rb")).unwrap();
    assert!(special.contains(r#"  attribute :kind, :string, default: "special""#), "special_widget:\n{special}");
}

/// A `default:` call with another option stays refused.
#[test]
fn attribute_default_with_other_options_is_refused() {
    let app = overlay().write(
        "app/models/widget.rb",
        "class Widget < ApplicationRecord\n  attribute :state, :string, default: \"x\", limit: 5\nend\n",
    );
    let (_emitted, errors) = app.emit(roundhouse::project::BuildTarget::Ruby);
    assert!(
        real_errors(&errors).iter().any(|e| e.contains("attribute") && e.contains("Widget")),
        "{}",
        errors.join("\n")
    );
}

/// The models natively and emitted, same PostgreSQL.
#[test]
#[ignore = "requires pg gem, Core's pinned Rails and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn attribute_default_matches_native_rails_on_postgresql() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    let (native, emitted_lines) = pg_probe::native_and_emitted(
        "attribute-default",
        &FILES,
        &emitted,
        "rh_e2e_attribute_default",
        "AttributeDefaultProbe",
        &LOADS,
    );
    assert_eq!(native, EXPECTED, "native");
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    assert_eq!(emitted_lines, native, "emitted");
}

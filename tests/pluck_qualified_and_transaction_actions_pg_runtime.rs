//! `pluck("table.column", ...)` through a raw join and
//! `transaction_include_any_action?` from an `after_commit` body, against
//! native Rails 8.2 on the same local PostgreSQL database.
//!
//! Core's ProductMapping::GroupingUpdateTriggerService plucks
//! `"#{group_products_table}.product_grouping_rule_id"` beside the joined
//! table's `"#{groups_table}.updated_at"`; the runtime wrote the string as
//! `"table".table.column` (PG::UndefinedTable, then every later statement
//! of the test PG::InFailedSqlTransaction). Retail::RoleAssignment's
//! `all_transaction_actions` asks `transaction_include_any_action?([action])`
//! of itself, which the runtime only answered as `_transaction_include_*?`.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;
#[path = "support/pg_probe.rs"]
mod pg_probe;

const STRUCTURE: &str = r#"CREATE TABLE groups (
    id bigserial NOT NULL,
    name character varying,
    CONSTRAINT groups_pkey PRIMARY KEY (id)
);

CREATE TABLE members (
    id bigserial NOT NULL,
    group_id bigint,
    label character varying,
    score integer,
    CONSTRAINT members_pkey PRIMARY KEY (id)
);
"#;

const DATABASE_YML: &str = r#"default: &default
  adapter: postgresql
  encoding: unicode

test:
  <<: *default
  database: rh_e2e_pluck_qualified
"#;

const APPLICATION_RECORD: &str = r#"class ApplicationRecord < ActiveRecord::Base
  self.abstract_class = true
end
"#;

const GROUP: &str = r#"class Group < ApplicationRecord
  has_many :members
end
"#;

const MEMBER: &str = r#"class Member < ApplicationRecord
  belongs_to :group

  after_commit :note_actions

  def self.noted
    @noted ||= []
  end

  def note_actions
    self.class.noted << [:create, :update, :destroy].select { |action| transaction_include_any_action?([action]) }
  end
end
"#;

const PROBE: &str = r##"class QualifiedProbe
  def self.lines
    out = []
    group = Group.create!(name: "g")
    Member.create!(group_id: group.id, label: "a", score: 1)
    Member.create!(group_id: group.id, label: "b", score: nil)
    join = "INNER JOIN groups ON groups.id = members.group_id"
    rows = Member.joins(join).where(groups: { name: "g" }).order(:id).pluck("members.label", "members.score", "groups.name")
    out << "joined: #{rows.inspect}"
    out << "one: #{Member.joins(join).order(:id).pluck("members.label").inspect}"
    out << "raw: #{Member.order(:id).pluck(Arel.sql("upper(label)")).inspect}"
    out << "plain: #{Member.order(:id).pluck(:label, "score").inspect}"
    member = Member.first
    member.update!(label: "c")
    member.destroy!
    out << "actions: #{Member.noted.inspect}"
    out
  end
end
"##;

const EXPECTED: [&str; 5] = [
    r#"joined: [["a", 1, "g"], ["b", nil, "g"]]"#,
    r#"one: ["a", "b"]"#,
    r#"raw: ["A", "B"]"#,
    r#"plain: [["a", 1], ["b", nil]]"#,
    r#"actions: [[:create], [:create], [:update], [:destroy]]"#,
];

const LOADS: [&str; 5] = [
    "app/models/application_record",
    "app/models/group",
    "app/models/member",
    "app/models/qualified_probe",
    "app/models/qualified_probe",
];

const FILES: [(&str, &str); 8] = [
    ("roundhouse.yml", "database_adapter: postgresql\n"),
    ("config/database.yml", DATABASE_YML),
    ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
    ("db/structure.sql", STRUCTURE),
    ("app/models/application_record.rb", APPLICATION_RECORD),
    ("app/models/group.rb", GROUP),
    ("app/models/member.rb", MEMBER),
    ("app/models/qualified_probe.rb", PROBE),
];

fn real_errors(errors: &[String]) -> Vec<&String> {
    errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect()
}

#[test]
#[ignore = "requires pg gem, Core's pinned Rails and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn qualified_pluck_and_transaction_actions_match_native_rails_on_postgresql() {
    let overlay = FILES.iter().fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text));
    let (emitted, errors) = overlay.emit(roundhouse::project::BuildTarget::Ruby);
    let (native, emitted_lines) = pg_probe::native_and_emitted(
        "pluck-qualified",
        &FILES,
        &emitted,
        "rh_e2e_pluck_qualified",
        "QualifiedProbe",
        &LOADS[..4],
    );
    assert_eq!(native, EXPECTED, "native");
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    assert_eq!(emitted_lines, native, "emitted");
}

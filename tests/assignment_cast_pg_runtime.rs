//! Assignment casts each column through its type, and `attributes`
//! answers every column (id too) in schema order, against native Rails
//! 8.2 on the same local PostgreSQL database. Covers a column of each
//! adapter type, plus a declared type over a column (`attribute :legacy,
//! :integer`, the path Core's `:model_uuid_as_integer` takes).
#[path = "support/emit_and_run.rs"]
mod emit_and_run;
#[path = "support/pg_probe.rs"]
mod pg_probe;

const STRUCTURE: &str = r#"CREATE TABLE gadgets (
    id bigserial NOT NULL,
    count integer,
    total bigint DEFAULT 0 NOT NULL,
    active boolean,
    ratio double precision,
    price numeric(10,2),
    day date,
    happened_at timestamp(6) without time zone,
    name character varying,
    body text,
    uid uuid,
    legacy character varying,
    owner_id bigint,
    CONSTRAINT gadgets_pkey PRIMARY KEY (id)
);

CREATE TABLE owners (
    id bigserial NOT NULL,
    CONSTRAINT owners_pkey PRIMARY KEY (id)
);
"#;

const DATABASE_YML: &str = r#"default: &default
  adapter: postgresql
  encoding: unicode

test:
  <<: *default
  database: rh_e2e_assignment_cast
"#;

const APPLICATION_RECORD: &str = r#"class ApplicationRecord < ActiveRecord::Base
  self.abstract_class = true
end
"#;

const GADGET: &str = r#"class Gadget < ApplicationRecord
  attribute :legacy, :integer
  belongs_to :owner, optional: true
end
"#;

const OWNER: &str = r#"class Owner < ApplicationRecord
end
"#;

const PROBE: &str = r##"class AssignmentCastProbe
  COLUMNS = %i[count total active ratio price day happened_at name body uid legacy]

  def self.show(g)
    COLUMNS.map { |c| "#{c}=#{fmt(g.public_send(c))}" }.join(" ")
  end

  # The instant and the calendar day, not their zone presentation or
  # ActiveSupport's Date#inspect.
  def self.fmt(v)
    case v
    when Time then "Time(#{v.utc.iso8601(6)})"
    when Date then "Date(#{v.iso8601})"
    else v.inspect
    end
  end

  def self.lines
    out = []
    g = Gadget.new(count: "7", total: "12", active: "0", ratio: "1.5", price: "9.99", day: "2026-10-08",
      happened_at: "2026-10-08 12:34:56", name: 7, body: :sym, uid: "A0EEBC99-9C0B-4EF8-BB6D-6BB9BD380A11", legacy: "5")
    out << "new: #{show(g)}"
    g.count = 8.9
    g.total = true
    g.active = "yes"
    g.ratio = 2
    g.price = 3
    g.day = "2026-01-02"
    g.happened_at = "2026-01-02T03:04:05Z"
    g.name = :other
    g.body = 1.5
    g.legacy = 6.7
    out << "write: #{show(g)}"
    g.count = ""
    g.active = ""
    g.ratio = " "
    g.price = ""
    g.day = ""
    g.happened_at = ""
    g.name = ""
    g.legacy = ""
    out << "blank: #{show(g)}"
    g.count = "abc"
    g.active = "off"
    g.ratio = "x"
    g.day = "garbage"
    out << "junk: #{show(g)}"
    g[:count] = "10"
    g[:active] = "t"
    g[:ratio] = "0.25"
    out << "index: #{show(g)}"
    g[:day] = "2026-03-04"
    g[:happened_at] = "2026-03-04 05:06:07"
    out << "index read: #{[:count, :active, :ratio, :price, :day, :happened_at, :uid, :legacy].map { |c| fmt(g[c]) }.join(" ")}"
    g.save!
    out << "saved: #{show(g.reload)}"
    g.update!(count: "11", active: "false", price: "1.239", total: "-3", happened_at: "2026-02-03 04:05:06")
    out << "update: #{show(g)}"
    out << "reload: #{show(g.reload)}"
    out << "new attributes: #{Gadget.new.attributes.inspect}"
    out << "attributes: #{g.attributes.keys.inspect} #{g.attributes.values_at("id", "count", "active", "price", "day", "happened_at", "uid", "legacy").map { |v| fmt(v) }.join(" ")} #{g.attributes["id"] == g.id}"
    g.update!(count: nil, price: nil)
    g[:active] = nil
    g[:ratio] = nil
    g.save!
    out << "nil: #{show(g)} | #{show(g.reload)}"
    o = Owner.create!
    g.update!(owner: o)
    fk = [g.owner_id == o.id]
    g.update!(owner_id: o.id.to_s)
    fk << (g.owner_id == o.id)
    g.update!(owner_id: nil)
    fk << g.reload.owner_id.inspect
    g.owner_id = o.id
    g[:owner_id] = nil
    fk << g.owner_id.inspect
    out << "fk: #{fk.join(" ")}"
    out
  end
end
"##;

/// What native Rails 8.2 answers (checked on the task PostgreSQL), so two
/// identical wrong answers still fail.
const EXPECTED: [&str; 13] = [
    r#"new: count=7 total=12 active=false ratio=1.5 price=0.999e1 day=Date(2026-10-08) happened_at=Time(2026-10-08T12:34:56.000000Z) name="7" body="sym" uid="a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11" legacy=5"#,
    r#"write: count=8 total=1 active=true ratio=2.0 price=0.3e1 day=Date(2026-01-02) happened_at=Time(2026-01-02T03:04:05.000000Z) name="other" body="1.5" uid="a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11" legacy=6"#,
    r#"blank: count=nil total=1 active=nil ratio=nil price=nil day=nil happened_at=nil name="" body="1.5" uid="a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11" legacy=nil"#,
    r#"junk: count=0 total=1 active=false ratio=0.0 price=nil day=nil happened_at=nil name="" body="1.5" uid="a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11" legacy=nil"#,
    r#"index: count=10 total=1 active=true ratio=0.25 price=nil day=nil happened_at=nil name="" body="1.5" uid="a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11" legacy=nil"#,
    r#"index read: 10 true 0.25 nil Date(2026-03-04) Time(2026-03-04T05:06:07.000000Z) "a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11" nil"#,
    r#"saved: count=10 total=1 active=true ratio=0.25 price=nil day=Date(2026-03-04) happened_at=Time(2026-03-04T05:06:07.000000Z) name="" body="1.5" uid="a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11" legacy=nil"#,
    r#"update: count=11 total=-3 active=false ratio=0.25 price=0.124e1 day=Date(2026-03-04) happened_at=Time(2026-02-03T04:05:06.000000Z) name="" body="1.5" uid="a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11" legacy=nil"#,
    r#"reload: count=11 total=-3 active=false ratio=0.25 price=0.124e1 day=Date(2026-03-04) happened_at=Time(2026-02-03T04:05:06.000000Z) name="" body="1.5" uid="a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11" legacy=nil"#,
    r#"new attributes: {"id" => nil, "count" => nil, "total" => 0, "active" => nil, "ratio" => nil, "price" => nil, "day" => nil, "happened_at" => nil, "name" => nil, "body" => nil, "uid" => nil, "legacy" => nil, "owner_id" => nil}"#,
    r#"attributes: ["id", "count", "total", "active", "ratio", "price", "day", "happened_at", "name", "body", "uid", "legacy", "owner_id"] 1 11 false 0.124e1 Date(2026-03-04) Time(2026-02-03T04:05:06.000000Z) "a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11" nil true"#,
    r#"nil: count=nil total=-3 active=nil ratio=nil price=nil day=Date(2026-03-04) happened_at=Time(2026-02-03T04:05:06.000000Z) name="" body="1.5" uid="a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11" legacy=nil | count=nil total=-3 active=nil ratio=nil price=nil day=Date(2026-03-04) happened_at=Time(2026-02-03T04:05:06.000000Z) name="" body="1.5" uid="a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11" legacy=nil"#,
    r#"fk: true true nil nil"#,
];

/// What native Rails requires, in order.
const LOADS: [&str; 4] = [
    "app/models/application_record",
    "app/models/owner",
    "app/models/gadget",
    "app/models/assignment_cast_probe",
];

const FILES: [(&str, &str); 8] = [
    ("roundhouse.yml", "database_adapter: postgresql\n"),
    ("config/database.yml", DATABASE_YML),
    ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
    ("db/structure.sql", STRUCTURE),
    ("app/models/application_record.rb", APPLICATION_RECORD),
    ("app/models/gadget.rb", GADGET),
    ("app/models/owner.rb", OWNER),
    ("app/models/assignment_cast_probe.rb", PROBE),
];

fn overlay() -> emit_and_run::Overlay {
    FILES.iter().fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text))
}

/// `check`'s errors other than the PostgreSQL adapter notice every
/// PostgreSQL probe app carries.
fn real_errors(errors: &[String]) -> Vec<&String> {
    errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect()
}

/// The model files natively and emitted, same PostgreSQL.
#[test]
#[ignore = "requires pg gem, Core's pinned Rails and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn assignment_cast_matches_native_rails_on_postgresql() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    let (native, emitted_lines) = pg_probe::native_and_emitted(
        "assignment-cast",
        &FILES,
        &emitted,
        "rh_e2e_assignment_cast",
        "AssignmentCastProbe",
        &LOADS,
    );
    assert_eq!(native, EXPECTED, "native");
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    assert_eq!(emitted_lines, native, "emitted");
}

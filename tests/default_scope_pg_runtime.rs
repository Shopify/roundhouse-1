//! `default_scope` against native Rails 8.2 on the same local
//! PostgreSQL database: the scope filters `all`, the finders, counts,
//! `exists?` and association readers; seeds a new record's attributes
//! from its `where` values; orders `first` and `all`; `extending` adds
//! its module's methods to every relation; `unscoped` drops it, as a
//! relation and around a block.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;
#[path = "support/pg_probe.rs"]
mod pg_probe;

const STRUCTURE: &str = r#"CREATE TABLE shelves (
    id bigserial NOT NULL,
    CONSTRAINT shelves_pkey PRIMARY KEY (id)
);

CREATE TABLE docs (
    id bigserial NOT NULL,
    shelf_id bigint,
    name character varying,
    is_not_deleted boolean,
    CONSTRAINT docs_pkey PRIMARY KEY (id)
);

CREATE TABLE tags (
    id bigserial NOT NULL,
    title character varying,
    status character varying,
    CONSTRAINT tags_pkey PRIMARY KEY (id)
);

CREATE TABLE notes (
    id bigserial NOT NULL,
    type character varying,
    title character varying,
    status character varying,
    CONSTRAINT notes_pkey PRIMARY KEY (id)
);
"#;

const DATABASE_YML: &str = r#"default: &default
  adapter: postgresql
  encoding: unicode

test:
  <<: *default
  database: rh_e2e_default_scope
"#;

const APPLICATION_RECORD: &str = r#"class ApplicationRecord < ActiveRecord::Base
  self.abstract_class = true
end
"#;

const SHELF: &str = r#"class Shelf < ApplicationRecord
  has_many :docs
end
"#;

const DOC: &str = r#"class Doc < ApplicationRecord
  default_scope { where(is_not_deleted: true) }
  belongs_to :shelf, optional: true

  def self.listed
    Doc.order(:name).pluck(:name).join(",")
  end
end
"#;

const TAG_FINDERS: &str = r#"module TagFinders
  def live
    where.not(status: "removed")
  end
end
"#;

const TAG: &str = r#"class Tag < ApplicationRecord
  default_scope { order(:title) }
  default_scope { extending(TagFinders) }
end
"#;

const NOTE: &str = r#"class Note < ApplicationRecord
  default_scope { where(status: "live") }
end
"#;

/// An STI subclass's own default_scope, after its parent's.
const PINNED: &str = r#"class Pinned < Note
  default_scope { order(:title) }
end
"#;

const PROBE: &str = r##"class DefaultScopeProbe
  def self.names(docs)
    docs.map(&:name).sort.join(",")
  end

  def self.lines
    out = []
    shelf = Shelf.create!
    Doc.create!(name: "a", shelf: shelf)
    Doc.create!(name: "b", shelf: shelf)
    gone = Doc.unscoped.create!(name: "gone", shelf: shelf, is_not_deleted: false)
    fresh = Doc.new(name: "n")
    out << "new: #{fresh.is_not_deleted.inspect} #{Doc.unscoped.new.is_not_deleted.inspect}"
    out << "all: #{Doc.count} #{names(Doc.all)} #{Doc.where(name: "gone").count} #{Doc.exists?(gone.id)}"
    found = begin
      Doc.find(gone.id).name
    rescue ActiveRecord::RecordNotFound => e
      e.class.name
    end
    out << "find: #{found} #{Doc.find_by(name: "gone").inspect} #{Doc.find_by(name: "a").name}"
    out << "unscoped: #{Doc.unscoped.count} #{Doc.unscoped.find(gone.id).name} #{Doc.unscoped { Doc.count }} #{Doc.count}"
    out << "association: #{shelf.docs.count} #{names(shelf.docs)} #{shelf.docs.where(name: "gone").exists?}"
    out << "arel: #{Doc.listed}"
    out << "belongs_to: #{gone.shelf == shelf}"
    gone.update!(is_not_deleted: true)
    out << "restored: #{Doc.count} #{names(Doc.where(shelf_id: shelf.id))}"
    Tag.create!(title: "c", status: "live")
    Tag.create!(title: "a", status: "removed")
    Tag.create!(title: "b", status: "live")
    out << "order: #{Tag.all.map(&:title).join(",")} #{Tag.first.title} #{Tag.last.title} #{Tag.reorder(:id).first.title}"
    out << "extending: #{Tag.all.live.map(&:title).join(",")} #{Tag.where(status: "live").live.count}"
    Note.create!(title: "z")
    Pinned.create!(title: "y")
    Pinned.create!(title: "x")
    Pinned.unscoped.create!(title: "w", status: "draft")
    out << "sti: #{Pinned.new.status} #{Pinned.all.map(&:title).join(",")} #{Pinned.count} #{Note.count} #{Pinned.unscoped.count} #{Note.unscoped.count} #{Pinned.first.title}"
    out
  end
end
"##;

/// What native Rails 8.2 answers (checked on the task PostgreSQL), so two
/// identical wrong answers still fail.
const EXPECTED: [&str; 11] = [
    "new: true nil",
    "all: 2 a,b 0 false",
    "find: ActiveRecord::RecordNotFound nil a",
    "unscoped: 3 gone 3 2",
    "association: 2 a,b false",
    "arel: a,b",
    "belongs_to: true",
    "restored: 3 a,b,gone",
    "order: a,b,c a c c",
    "extending: b,c 2",
    "sti: live x,y 2 3 3 4 x",
];

/// What native Rails requires, in order.
const LOADS: [&str; 8] = [
    "app/models/application_record",
    "app/models/shelf",
    "app/models/doc",
    "app/models/tag_finders",
    "app/models/tag",
    "app/models/note",
    "app/models/pinned",
    "app/models/default_scope_probe",
];

const FILES: [(&str, &str); 12] = [
    ("roundhouse.yml", "database_adapter: postgresql\n"),
    ("config/database.yml", DATABASE_YML),
    ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
    ("db/structure.sql", STRUCTURE),
    ("app/models/application_record.rb", APPLICATION_RECORD),
    ("app/models/shelf.rb", SHELF),
    ("app/models/doc.rb", DOC),
    ("app/models/tag_finders.rb", TAG_FINDERS),
    ("app/models/tag.rb", TAG),
    ("app/models/note.rb", NOTE),
    ("app/models/pinned.rb", PINNED),
    ("app/models/default_scope_probe.rb", PROBE),
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
fn default_scope_matches_native_rails_on_postgresql() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    let (native, emitted_lines) = pg_probe::native_and_emitted(
        "default-scope",
        &FILES,
        &emitted,
        "rh_e2e_default_scope",
        "DefaultScopeProbe",
        &LOADS,
    );
    assert_eq!(native, EXPECTED, "native");
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    assert_eq!(emitted_lines, native, "emitted");
}

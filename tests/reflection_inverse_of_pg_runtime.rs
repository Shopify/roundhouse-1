//! `Reflection#inverse_of` / `inverse_name` / `has_inverse?` and
//! `connection.supports_insert_returning?` against native Rails 8.2 on the
//! same local PostgreSQL database.
//!
//! Core's query layer reads `association.reflection.inverse_of` to seat the
//! owner on each loaded target (the reflection answered NoMethodError), and
//! `BulkSqlOperations::Create` asks the connection whether INSERT ...
//! RETURNING is supported before picking its statement.
//!
//! The model files run natively (Core's pinned Rails, over
//! `establish_connection`) and on the emitted program, each on a freshly
//! created `rh_e2e_inverse_of`; every line `InverseProbe.lines` prints must
//! be equal, and equal to what Rails is known to answer.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;
#[path = "support/pg_probe.rs"]
mod pg_probe;

const STRUCTURE: &str = r#"CREATE TABLE authors (
    id bigserial NOT NULL,
    CONSTRAINT authors_pkey PRIMARY KEY (id)
);
CREATE TABLE books (
    id bigserial NOT NULL,
    author_id bigint,
    title character varying,
    CONSTRAINT books_pkey PRIMARY KEY (id)
);
CREATE TABLE profiles (
    id bigserial NOT NULL,
    author_id bigint,
    CONSTRAINT profiles_pkey PRIMARY KEY (id)
);
CREATE TABLE essays (
    id bigserial NOT NULL,
    author_id bigint,
    title character varying,
    CONSTRAINT essays_pkey PRIMARY KEY (id)
);
CREATE TABLE pets (
    id bigserial NOT NULL,
    owner_id bigint,
    CONSTRAINT pets_pkey PRIMARY KEY (id)
);
CREATE TABLE notes (
    id bigserial NOT NULL,
    author_id bigint,
    CONSTRAINT notes_pkey PRIMARY KEY (id)
);
"#;

const DATABASE_YML: &str = r#"default: &default
  adapter: postgresql
  encoding: unicode

test:
  <<: *default
  database: rh_e2e_inverse_of
"#;

const APPLICATION_RECORD: &str = r#"class ApplicationRecord < ActiveRecord::Base
  self.abstract_class = true
end
"#;

const AUTHOR: &str = r#"class Author < ApplicationRecord
  has_many :books
  has_one :profile
  has_many :essays, -> { where(title: "x") }
  has_many :pets, inverse_of: :owner, foreign_key: :owner_id
  has_many :notes, inverse_of: false
end
"#;

const BOOK: &str = "class Book < ApplicationRecord\n  belongs_to :author, optional: true\nend\n";
const PROFILE: &str = "class Profile < ApplicationRecord\n  belongs_to :author, optional: true\nend\n";
const ESSAY: &str = "class Essay < ApplicationRecord\n  belongs_to :author, optional: true\nend\n";
const PET: &str = "class Pet < ApplicationRecord\n  belongs_to :owner, class_name: \"Author\", optional: true\nend\n";
const NOTE: &str = "class Note < ApplicationRecord\n  belongs_to :author, optional: true\nend\n";

const PROBE: &str = r##"class InverseProbe
  def self.show(klass, name)
    r = klass.reflect_on_association(name)
    "#{klass}.#{name}: #{r.has_inverse?.inspect} #{r.inverse_of&.name.inspect} #{r.inverse_of&.active_record&.name.inspect}"
  end

  def self.lines
    out = []
    out << show(Author, :books)
    out << show(Author, :profile)
    out << show(Author, :essays)
    out << show(Author, :pets)
    out << show(Author, :notes)
    out << show(Book, :author)
    out << show(Profile, :author)
    out << show(Essay, :author)
    out << show(Pet, :owner)
    out << show(Note, :author)
    out << "returning: #{Author.connection.supports_insert_returning?.inspect}"
    out
  end
end
"##;

/// What native Rails 8.2 answers (checked on the task PostgreSQL), so two
/// identical wrong answers still fail.
const EXPECTED: [&str; 11] = [
    r#"Author.books: :author :author "Book""#,
    r#"Author.profile: :author :author "Profile""#,
    "Author.essays: nil nil nil",
    r#"Author.pets: :owner :owner "Pet""#,
    "Author.notes: false nil nil",
    "Book.author: nil nil nil",
    r#"Profile.author: :profile :profile "Author""#,
    "Essay.author: nil nil nil",
    "Pet.owner: nil nil nil",
    "Note.author: nil nil nil",
    "returning: true",
];

const LOADS: [&str; 8] = [
    "app/models/application_record",
    "app/models/author",
    "app/models/book",
    "app/models/profile",
    "app/models/essay",
    "app/models/pet",
    "app/models/note",
    "app/models/inverse_probe",
];

const FILES: [(&str, &str); 12] = [
    ("roundhouse.yml", "database_adapter: postgresql\n"),
    ("config/database.yml", DATABASE_YML),
    ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
    ("db/structure.sql", STRUCTURE),
    ("app/models/application_record.rb", APPLICATION_RECORD),
    ("app/models/author.rb", AUTHOR),
    ("app/models/book.rb", BOOK),
    ("app/models/profile.rb", PROFILE),
    ("app/models/essay.rb", ESSAY),
    ("app/models/pet.rb", PET),
    ("app/models/note.rb", NOTE),
    ("app/models/inverse_probe.rb", PROBE),
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
fn reflection_inverse_of_matches_native_rails_on_postgresql() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    let (native, emitted) = pg_probe::native_and_emitted(
        "inverse-of",
        &FILES,
        &emitted,
        "rh_e2e_inverse_of",
        "InverseProbe",
        &LOADS,
    );
    assert_eq!(native, EXPECTED, "native");
    assert_eq!(emitted, native, "emitted");
}

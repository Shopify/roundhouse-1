//! Models that store a column through an app coder with `serialize`
//! (`serialize :tags, coder: PipeCoder, type: Array`; Core has some 200
//! of them, mostly `coder: VersionedSerializer.column(...)`), against
//! native Rails 8.2 on the same local PostgreSQL database.
//!
//! Rails builds the column serializer from the coder (a class with
//! `load`/`dump`, an instance, or a class constructed with the attribute
//! name and type) and wraps it in a `type:` check when one is given, then
//! decorates the column's type with Type::Serialized: reads load, writes
//! and binds dump, a nil column loads as `type.new`, a value equal to the
//! empty load stores NULL and a value of the wrong class raises
//! SerializationTypeMismatch. A bytea column reads and binds the coder's
//! binary String. The model compiles the column as retypable
//! and the call runs as written at class load against the runtime's
//! `serialize`. The JSON and YAML coders keep their own paths.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;
#[path = "support/pg_probe.rs"]
mod pg_probe;

const STRUCTURE: &str = r#"CREATE TABLE notes (
    id bigserial NOT NULL,
    title character varying,
    tags text,
    meta character varying,
    body text,
    points bytea,
    CONSTRAINT notes_pkey PRIMARY KEY (id)
);
"#;

const DATABASE_YML: &str = r#"default: &default
  adapter: postgresql
  encoding: unicode

test:
  <<: *default
  database: rh_e2e_serialize_coder
"#;

const APPLICATION_RECORD: &str = r#"class ApplicationRecord < ActiveRecord::Base
  self.abstract_class = true
end
"#;

const NOTE: &str = r#"class Note < ApplicationRecord
  serialize :tags, coder: PipeCoder, type: Array
  # A coder built in a class-body local, as Core's Checkout builds
  # its `line_items` one: Ruby runs the write where it stands.
  pairs = PairsCoder.new(";")
  serialize :meta, coder: pairs, type: Hash
  serialize :body, coder: UpcaseCoder
  serialize :points, coder: PackCoder, type: Array
end
"#;

/// A coder to a binary String, over a bytea column.
const PACK_CODER: &str = r#"class PackCoder
  def self.load(payload)
    payload.nil? ? nil : payload.unpack("N*")
  end

  def self.dump(value)
    value.nil? ? nil : value.pack("N*")
  end
end
"#;

/// The same table without the coders: the stored text.
const RAW_NOTE: &str = r#"class RawNote < ApplicationRecord
  self.table_name = "notes"
end
"#;

/// A coder class: it answers `load`, so Rails uses the class itself.
const PIPE_CODER: &str = r#"class PipeCoder
  def self.load(payload)
    payload.nil? ? nil : payload.split("|")
  end

  def self.dump(value)
    value.nil? ? nil : value.join("|")
  end
end
"#;

/// A coder instance.
const PAIRS_CODER: &str = r##"class PairsCoder
  def initialize(separator)
    @separator = separator
  end

  def load(payload)
    return {} if payload.nil? || payload.empty?
    payload.split(@separator).to_h { |pair| pair.split("=", 2) }
  end

  def dump(value)
    value.map { |k, v| "#{k}=#{v}" }.join(@separator)
  end
end
"##;

/// A coder class without `load`: Rails constructs it with the attribute
/// name and type.
const UPCASE_CODER: &str = r#"class UpcaseCoder
  def initialize(attr_name, object_class)
    @attr_name = attr_name
    @object_class = object_class
  end

  def load(payload)
    payload.nil? ? nil : payload.downcase
  end

  def dump(value)
    value.nil? ? nil : value.upcase
  end
end
"#;

const PROBE: &str = r##"class SerializeCoderProbe
  def self.lines
    out = []
    fresh = Note.new(title: "f")
    out << "new: #{fresh.tags.inspect} #{fresh.meta.inspect} #{fresh.body.inspect}"
    a = Note.create!(title: "a", tags: ["x", "y"], meta: { "k" => "v", "n" => "1" }, body: "Hello", points: [1, 258])
    b = Note.create!(title: "b")
    out << "read: #{a.tags.inspect} #{a.meta.inspect} #{a.body.inspect} #{b.tags.inspect} #{b.meta.inspect}"
    out << "raw: #{RawNote.order(:id).map { |r| [r.tags, r.meta, r.body] }.inspect}"
    out << "pluck: #{Note.order(:id).pluck(:tags).inspect}"
    out << "where: #{Note.where(tags: ["x", "y"]).count} #{Note.where(body: "hello").count}"
    found = Note.find(a.id)
    out << "find: #{found.tags.inspect} #{found.meta.inspect} #{found.body.inspect}"
    out << "bytea: #{found.points.inspect} #{b.points.inspect} #{RawNote.find(a.id).points.inspect} #{RawNote.find(a.id).points.encoding} #{Note.where(points: [1, 258]).count}"
    b.tags = ["q"]
    b.meta = {}
    b.save!
    out << "update: #{b.reload.tags.inspect} #{b.meta.inspect} #{RawNote.find(b.id).tags.inspect} #{RawNote.find(b.id).meta.inspect}"
    found.tags << "z"
    found.save!
    out << "in place: #{Note.find(a.id).tags.inspect}"
    begin
      found.tags = "oops"
      found.save!
      out << "mismatch: none"
    rescue ActiveRecord::SerializationTypeMismatch => e
      out << "mismatch: #{e.message}"
    end
    b[:tags] += ["r"]
    out << "index: #{b[:tags].inspect} #{b.tags.inspect} #{b[:meta].inspect}"
    out << "types: #{Note.type_for_attribute(:tags).class} #{Note.type_for_attribute(:tags).type.inspect} #{Note.type_for_attribute(:meta).type.inspect}"
    out
  end
end
"##;

/// What native Rails 8.2 answers (checked on the task PostgreSQL), so two
/// identical wrong answers still fail.
const EXPECTED: [&str; 12] = [
    "new: [] {} nil",
    r#"read: ["x", "y"] {"k" => "v", "n" => "1"} "hello" [] {}"#,
    r#"raw: [["x|y", "k=v;n=1", "HELLO"], [nil, nil, nil]]"#,
    r#"pluck: [["x", "y"], []]"#,
    "where: 1 1",
    r#"find: ["x", "y"] {"k" => "v", "n" => "1"} "hello""#,
    r#"bytea: [1, 258] [] "\x00\x00\x00\x01\x00\x00\x01\x02" ASCII-8BIT 1"#,
    r#"update: ["q"] {} "q" nil"#,
    r#"in place: ["x", "y", "z"]"#,
    r#"mismatch: can't serialize `tags`: was supposed to be a Array, but was a String. -- "oops""#,
    r#"index: ["q", "r"] ["q", "r"] {}"#,
    "types: ActiveRecord::Type::Serialized :text :string",
];

/// What native Rails requires, in order.
const LOADS: [&str; 8] = [
    "app/models/pipe_coder",
    "app/models/pack_coder",
    "app/models/pairs_coder",
    "app/models/upcase_coder",
    "app/models/application_record",
    "app/models/note",
    "app/models/raw_note",
    "app/models/serialize_coder_probe",
];

const FILES: [(&str, &str); 12] = [
    ("roundhouse.yml", "database_adapter: postgresql\n"),
    ("config/database.yml", DATABASE_YML),
    ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
    ("db/structure.sql", STRUCTURE),
    ("app/models/application_record.rb", APPLICATION_RECORD),
    ("app/models/note.rb", NOTE),
    ("app/models/raw_note.rb", RAW_NOTE),
    ("app/models/pipe_coder.rb", PIPE_CODER),
    ("app/models/pairs_coder.rb", PAIRS_CODER),
    ("app/models/upcase_coder.rb", UPCASE_CODER),
    ("app/models/pack_coder.rb", PACK_CODER),
    ("app/models/serialize_coder_probe.rb", PROBE),
];

fn overlay() -> emit_and_run::Overlay {
    FILES.iter().fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text))
}

/// `check`'s errors other than the PostgreSQL adapter notice every
/// PostgreSQL probe app carries. The `serialize` calls are claimed: they
/// run as written at class load.
fn real_errors(errors: &[String]) -> Vec<&String> {
    errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect()
}

/// The compiler side: the columns consult the declared type, and the
/// model carries its calls.
#[test]
fn serialize_with_an_app_coder_runs_at_class_load() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    let note = std::fs::read_to_string(emitted.join("app/models/note.rb")).unwrap();
    for expected in [
        r#"self.class._attribute_deserialize("tags", value)"#,
        "  serialize :tags, coder: PipeCoder, type: Array\n",
        "  serialize :body, coder: UpcaseCoder\n",
        "  pairs = PairsCoder.new(\";\")\n",
        "  serialize :meta, coder: pairs, type: Hash\n",
    ] {
        assert!(note.contains(expected), "missing {expected:?} in:\n{note}");
    }
}

/// The model files natively and emitted, same PostgreSQL.
#[test]
#[ignore = "requires pg gem, Core's pinned Rails and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn serialize_with_an_app_coder_matches_native_rails_on_postgresql() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    let (native, emitted_lines) = pg_probe::native_and_emitted(
        "serialize-coder",
        &FILES,
        &emitted,
        "rh_e2e_serialize_coder",
        "SerializeCoderProbe",
        &LOADS,
    );
    assert_eq!(native, EXPECTED, "native");
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    assert_eq!(emitted_lines, native, "emitted");
}

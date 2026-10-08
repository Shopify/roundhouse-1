//! Generated-expression validation needs source-type provenance when
//! ordinary schema typing normalizes database-specific text-like types.

use roundhouse::ingest::structure_sql::ingest_structure_sql;
use roundhouse::ingest::{ingest_migration, ingest_schema};
use roundhouse::schema::Schema;

fn schema_error(source: &str) -> String {
    ingest_schema(source.as_bytes(), "db/schema.rb")
        .expect_err("non-portable source types must not pass generated-expression validation")
        .to_string()
}

#[test]
fn schema_rb_rejects_nonportable_result_and_operand_types() {
    let operand = schema_error(
        r#"ActiveRecord::Schema[8.1].define(version: 1) do
  create_table "people", force: :cascade do |t|
    t.inet "remote_ip"
    t.string "first_name"
    t.virtual "display", type: :string, as: "remote_ip || 'x'", stored: true
  end
end
"#,
    );
    assert!(operand.contains("non-portable text semantics"), "{operand}");

    let result = schema_error(
        r#"ActiveRecord::Schema[8.1].define(version: 1) do
  create_table "people", force: :cascade do |t|
    t.string "first_name"
    t.virtual "display", type: :inet, as: "first_name || 'x'", stored: true
  end
end
"#,
    );
    assert!(
        result.contains("original generated-column result type"),
        "{result}"
    );

    let enum_operand = schema_error(
        r#"ActiveRecord::Schema[8.1].define(version: 1) do
  create_table "people", force: :cascade do |t|
    t.enum "status"
    t.virtual "label", type: :text, as: "status || 'x'", stored: true
  end
end
"#,
    );
    assert!(
        enum_operand.contains("non-portable text semantics"),
        "{enum_operand}"
    );
}

#[test]
fn schema_rb_rejects_text_limits_that_normalization_would_discard() {
    let operand = schema_error(
        r#"ActiveRecord::Schema[8.1].define(version: 1) do
  create_table "people", force: :cascade do |t|
    t.text "source_text", limit: 32
    t.virtual "display", type: :string, as: "source_text || 'x'", stored: true
  end
end
"#,
    );
    assert!(operand.contains("non-portable text semantics"), "{operand}");

    let result = schema_error(
        r#"ActiveRecord::Schema[8.1].define(version: 1) do
  create_table "people", force: :cascade do |t|
    t.string "first_name"
    t.virtual "display", type: :text, as: "first_name || 'x'", limit: 32, stored: true
  end
end
"#,
    );
    assert!(
        result.contains("original generated-column result type"),
        "{result}"
    );
}

#[test]
fn migration_folds_retain_nonportable_type_provenance() {
    let create_inet = r#"class CreatePeople < ActiveRecord::Migration[8.1]
  def change
    create_table :people do |t|
      t.inet :remote_ip
    end
  end
end
"#;
    let add_generated = r#"class AddDisplay < ActiveRecord::Migration[8.1]
  def change
    add_column :people, :display, :string, as: "remote_ip || 'x'", stored: true
  end
end
"#;
    let mut schema = Schema::default();
    ingest_migration(create_inet.as_bytes(), "001_create_people.rb", &mut schema)
        .expect("ordinary migration column");
    let inet = ingest_migration(add_generated.as_bytes(), "002_add_display.rb", &mut schema)
        .expect_err("migration folds must preserve source type evidence");
    assert!(
        inet.to_string().contains("non-portable text semantics"),
        "{inet}"
    );

    let create_enum = r#"class CreatePeople < ActiveRecord::Migration[8.1]
  def change
    create_table :people do |t|
      t.enum :status
    end
  end
end
"#;
    let add_enum_generated = r#"class AddLabel < ActiveRecord::Migration[8.1]
  def change
    add_column :people, :label, :text, as: "status || 'x'", stored: true
  end
end
"#;
    let mut schema = Schema::default();
    ingest_migration(create_enum.as_bytes(), "001_create_people.rb", &mut schema)
        .expect("ordinary enum migration column");
    let enum_error = ingest_migration(
        add_enum_generated.as_bytes(),
        "002_add_label.rb",
        &mut schema,
    )
    .expect_err("enum provenance must survive across migration files");
    assert!(
        enum_error
            .to_string()
            .contains("non-portable text semantics"),
        "{enum_error}"
    );
}

#[test]
fn structure_sql_rejects_text_like_aliases_but_accepts_unbounded_varchar() {
    for source_type in [
        "inet",
        "cidr",
        "macaddr",
        "macaddr8",
        "interval",
        "character",
    ] {
        let dump = format!(
            "CREATE TABLE public.people (source_text {source_type}, label text GENERATED ALWAYS AS (source_text || 'x') STORED);"
        );
        let error = ingest_structure_sql(dump.as_bytes(), "db/structure.sql")
            .expect_err("collapsed aliases must not become supported generated operands")
            .to_string();
        assert!(
            error.contains("non-portable text semantics"),
            "{source_type}: {error}"
        );
    }

    let enum_dump = r#"CREATE TYPE public.widget_status AS ENUM ('draft', 'published');
CREATE TABLE public.people (
  status_code public.widget_status,
  label text GENERATED ALWAYS AS (status_code || 'x') STORED
);"#;
    let enum_error = ingest_structure_sql(enum_dump.as_bytes(), "db/structure.sql")
        .expect_err("registered Postgres enums normalize to text but are not text operands")
        .to_string();
    assert!(
        enum_error.contains("non-portable text semantics"),
        "{enum_error}"
    );

    let portable = r#"CREATE TABLE public.people (
  source_text character varying,
  label text GENERATED ALWAYS AS (source_text || 'x') STORED
);"#;
    let schema = ingest_structure_sql(portable.as_bytes(), "db/structure.sql")
        .expect("unbounded varying text is a supported operand");
    let people = &schema.tables[&roundhouse::Symbol::from("people")];
    assert_eq!(people.columns.len(), 2);
}

#[test]
fn structure_sql_text_typmods_are_not_treated_as_plain_unbounded_text() {
    let dump = r#"CREATE TABLE public.people (
  source_text text(12),
  label text GENERATED ALWAYS AS (source_text || 'x') STORED
);"#;
    let error = ingest_structure_sql(dump.as_bytes(), "db/structure.sql")
        .expect_err("a discarded text typmod must not pass the generated text boundary")
        .to_string();
    assert!(error.contains("non-portable text semantics"), "{error}");
}

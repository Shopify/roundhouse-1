//! Generated columns keep their expression and storage mode in the schema,
//! reach dialect DDL, and reject forms that cannot be preserved portably.

use roundhouse::det::HashMap;
use std::path::{Path, PathBuf};

use roundhouse::Symbol;
use roundhouse::emit::shared::schema_sql::{Dialect, render_schema_statements_for};
use roundhouse::ingest::{ingest_app_from_tree, ingest_schema, survey};
use roundhouse::project::{BuildTarget, target_files};
use roundhouse::schema::{GeneratedColumnStorage, Schema, Table};

fn schema() -> Schema {
    ingest_schema(
        br#"ActiveRecord::Schema[8.1].define(version: 1) do
  create_table "people", force: :cascade do |t|
    t.string "first_name"
    t.string "last_name"
    t.virtual "display_name", type: :string, as: "first_name || ' ' || coalesce(last_name, '')", stored: true
    t.virtual "normalized_first_name", type: :string, as: "coalesce(first_name, '')", stored: true, null: false
    t.virtual "fallback_name", type: :text, as: "coalesce(first_name, '')", stored: false
  end
end
"#,
        "db/schema.rb",
    )
    .expect("generated-column schema should ingest")
}

fn table<'a>(schema: &'a Schema, name: &str) -> &'a Table {
    schema
        .tables
        .get(&Symbol::from(name))
        .unwrap_or_else(|| panic!("missing table {name}"))
}

fn column<'a>(table: &'a Table, name: &str) -> &'a roundhouse::schema::Column {
    table
        .columns
        .iter()
        .find(|column| column.name.as_str() == name)
        .unwrap_or_else(|| {
            panic!(
                "missing column {name} on {}; have {:?}",
                table.name.as_str(),
                table
                    .columns
                    .iter()
                    .map(|column| column.name.as_str())
                    .collect::<Vec<_>>()
            )
        })
}

fn generated_model_app() -> roundhouse::App {
    let tree: HashMap<PathBuf, Vec<u8>> = [
        (
            "app/models/application_record.rb",
            "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n",
        ),
        (
            "app/controllers/application_controller.rb",
            "class ApplicationController < ActionController::Base\nend\n",
        ),
        ("db/schema.rb", include_str!("support/generated_columns_schema.rb")),
        ("app/models/person.rb", include_str!("support/generated_columns_person.rb")),
        (
            "app/models/virtual_person.rb",
            include_str!("support/generated_columns_virtual_person.rb"),
        ),
        (
            "app/models/constant_person.rb",
            include_str!("support/generated_columns_constant_person.rb"),
        ),
    ]
    .iter()
    .map(|(path, content)| (PathBuf::from(path), content.as_bytes().to_vec()))
    .collect();
    let mut app = ingest_app_from_tree(tree).expect("ingest generated-column app");
    roundhouse::session::analyze_and_lower(&mut app);
    app
}

#[test]
fn generated_model_persistence_requires_a_runtime_with_insert_returning() {
    let app = generated_model_app();

    for target in [BuildTarget::Ruby, BuildTarget::Jruby, BuildTarget::Spinel] {
        target_files(&app, Path::new("."), target).unwrap_or_else(|error| {
            panic!("{target:?} runtime supports Db.exec_returning: {error}")
        });
    }

    let unsupported = [
        BuildTarget::Crystal,
        BuildTarget::Elixir,
        BuildTarget::Go,
        BuildTarget::Kotlin,
        BuildTarget::Python,
        BuildTarget::Rust,
        BuildTarget::Swift,
        BuildTarget::CSharp,
        BuildTarget::Typescript,
        BuildTarget::TypescriptWorker,
    ];
    for target in unsupported {
        let error = target_files(&app, Path::new("."), target).expect_err(
            "an SDK target without the persistence runtime must refuse generated models",
        );
        assert!(
            error.contains("generated-column model persistence"),
            "{target:?}: {error}"
        );
        assert!(error.contains(target.as_str()), "{target:?}: {error}");
        assert!(error.contains("Db.exec_returning"), "{target:?}: {error}");
        assert!(error.contains("display_name"), "{target:?}: {error}");
    }

    let roda_error = target_files(&app, Path::new("."), BuildTarget::Roda)
        .expect_err("Roda already rejects generated columns");
    assert!(
        roda_error.contains("Roda target does not support generated column"),
        "{roda_error}"
    );
}

#[test]
fn schema_rb_preserves_generated_expression_storage_and_nullability() {
    let schema = schema();
    let people = table(&schema, "people");

    let display_name = column(people, "display_name");
    let stored = display_name
        .generated
        .as_ref()
        .expect("stored generated metadata");
    assert_eq!(
        stored.expression,
        "first_name || ' ' || coalesce(last_name, '')"
    );
    assert_eq!(stored.storage, GeneratedColumnStorage::Stored);
    assert!(display_name.nullable);

    let fallback_name = column(people, "fallback_name");
    let virtual_column = fallback_name
        .generated
        .as_ref()
        .expect("virtual generated metadata");
    assert_eq!(virtual_column.expression, "coalesce(first_name, '')");
    assert_eq!(virtual_column.storage, GeneratedColumnStorage::Virtual);
    assert!(fallback_name.nullable);

    let normalized = column(people, "normalized_first_name");
    assert!(
        !normalized.nullable,
        "generated nullability remains a schema fact"
    );
    assert_eq!(
        normalized.generated.as_ref().unwrap().expression,
        "coalesce(first_name, '')"
    );
}

#[test]
fn sqlite_and_postgres_render_the_stored_expression_without_rewriting_it() {
    let schema = schema();
    let sqlite = render_schema_statements_for(&schema, Dialect::Sqlite).expect("SQLite DDL");
    assert!(
        sqlite[0].contains(
            "display_name TEXT GENERATED ALWAYS AS (first_name || ' ' || coalesce(last_name, '')) STORED"
        ),
        "stored expression should survive exactly: {sqlite:?}"
    );
    assert!(
        sqlite[0]
            .contains("fallback_name TEXT GENERATED ALWAYS AS (coalesce(first_name, '')) VIRTUAL"),
        "SQLite can render both generated storage modes: {sqlite:?}"
    );
    assert!(
        sqlite[0].contains(
            "normalized_first_name TEXT GENERATED ALWAYS AS (coalesce(first_name, '')) STORED NOT NULL"
        ),
        "generated nullability must be rendered after its storage mode: {sqlite:?}"
    );

    let postgres = render_schema_statements_for(&schema, Dialect::Postgres).expect_err(
        "Roundhouse PostgreSQL DDL renderer does not yet support virtual generated columns",
    );
    assert!(
        postgres.contains(
            "Roundhouse PostgreSQL DDL renderer does not yet support virtual generated columns"
        ),
        "the boundary should name Roundhouse's renderer, not imply PostgreSQL lacks the feature: {postgres}"
    );

    let stored_only = ingest_schema(
        br#"ActiveRecord::Schema[8.1].define(version: 1) do
  create_table "stored_people", force: :cascade do |t|
    t.string "first_name"
    t.string "last_name"
    t.virtual "display_name", type: :string, as: "first_name || ' ' || coalesce(last_name, '')", stored: true
  end
end
"#,
        "db/schema.rb",
    )
    .expect("stored-only schema should ingest");
    let postgres =
        render_schema_statements_for(&stored_only, Dialect::Postgres).expect("Postgres stored DDL");
    assert!(
        postgres[0].contains(
            "\"display_name\" character varying GENERATED ALWAYS AS (first_name || ' ' || coalesce(last_name, '')) STORED"
        ),
        "Postgres DDL should retain the expression: {postgres:?}"
    );
}

#[test]
fn generated_columns_coexist_with_a_custom_string_primary_key() {
    let schema = ingest_schema(
        br#"ActiveRecord::Schema[8.1].define(version: 1) do
  create_table "custom_people", primary_key: "person_key", id: :string, force: :cascade do |t|
    t.string "first_name"
    t.virtual "label", type: :string, as: "first_name || '-custom'", stored: true
  end
end
"#,
        "db/schema.rb",
    )
    .expect("custom string key and generated column should ingest");
    let custom_people = table(&schema, "custom_people");
    let key = column(custom_people, "person_key");
    assert!(key.primary_key);
    assert!(key.generated.is_none());
    let label = column(custom_people, "label");
    assert!(!label.primary_key);
    assert_eq!(
        label.generated.as_ref().unwrap().expression,
        "first_name || '-custom'"
    );

    let sqlite = render_schema_statements_for(&schema, Dialect::Sqlite).expect("SQLite DDL");
    assert!(
        sqlite[0].contains("person_key TEXT PRIMARY KEY NOT NULL"),
        "{sqlite:?}"
    );
    assert!(
        sqlite[0].contains("label TEXT GENERATED ALWAYS AS (first_name || '-custom') STORED"),
        "{sqlite:?}"
    );
    let postgres = render_schema_statements_for(&schema, Dialect::Postgres).expect("Postgres DDL");
    assert!(
        postgres[0].contains("\"person_key\" character varying PRIMARY KEY NOT NULL"),
        "{postgres:?}"
    );
    assert!(
        postgres[0].contains(
            "\"label\" character varying GENERATED ALWAYS AS (first_name || '-custom') STORED"
        ),
        "{postgres:?}"
    );
}

#[test]
fn unsupported_expressions_and_generated_column_dsl_fail_strict_ingest() {
    let unsupported = [
        (
            "first_name::text",
            "PostgreSQL casts are not rewritten into a different dialect",
        ),
        (
            "payload ->> 'name'",
            "PostgreSQL JSON operators are not rewritten into SQLite JSON1",
        ),
        (
            "lower(first_name)",
            "functions outside concat/coalesce are not in the portable subset",
        ),
        (
            "first_name || 'x' trailing",
            "trailing expression tokens must not be discarded",
        ),
        (
            "first_name || 'x\\0y'",
            "NUL cannot be embedded in a generated SQL expression",
        ),
    ];

    for (expression, reason) in unsupported {
        let source = format!(
            r#"ActiveRecord::Schema[8.1].define(version: 1) do
  create_table "people", force: :cascade do |t|
    t.string "first_name"
    t.text "payload"
    t.virtual "display_name", type: :string, as: "{expression}", stored: true
  end
end
"#
        );
        let error = ingest_schema(source.as_bytes(), "db/schema.rb")
            .expect_err("unsupported generated expressions must not become writable columns")
            .to_string();
        assert!(
            error.contains("display_name") && error.contains("generated"),
            "{reason}: {error}"
        );
    }

    let missing_options = ingest_schema(
        br#"ActiveRecord::Schema[8.1].define(version: 1) do
  create_table "people", force: :cascade do |t|
    t.virtual "display_name"
  end
end
"#,
        "db/schema.rb",
    )
    .expect_err("t.virtual without a result type and expression must fail explicitly")
    .to_string();
    assert!(
        missing_options.contains("generated column type is missing")
            || missing_options.contains("generated expression is missing"),
        "{missing_options}"
    );
}

#[test]
fn generated_expression_keyword_columns_must_be_quoted() {
    for keyword in ["ANY", "USER", "SOME"] {
        let column_name = keyword.to_ascii_lowercase();
        let unquoted = format!(
            r#"ActiveRecord::Schema[8.1].define(version: 1) do
  create_table "people", force: :cascade do |t|
    t.string "{column_name}"
    t.virtual "computed", type: :string, as: "{keyword}", stored: true
  end
end
"#
        );
        let error = ingest_schema(unquoted.as_bytes(), "db/schema.rb")
            .expect_err("bare SQL keywords must not be mistaken for portable column references")
            .to_string();
        assert!(
            error.contains("unquoted SQL keyword") && error.contains(keyword),
            "{keyword}: {error}"
        );

        let quoted = format!(
            r#"ActiveRecord::Schema[8.1].define(version: 1) do
  create_table "people", force: :cascade do |t|
    t.string "{column_name}"
    t.virtual "computed", type: :string, as: '"{column_name}"', stored: true
  end
end
"#
        );
        let schema = ingest_schema(quoted.as_bytes(), "db/schema.rb")
            .expect("a double-quoted SQL keyword is an identifier");
        let computed = column(table(&schema, "people"), "computed");
        assert_eq!(
            computed.generated.as_ref().unwrap().expression,
            format!("\"{column_name}\"")
        );

        let sqlite = render_schema_statements_for(&schema, Dialect::Sqlite).expect("SQLite DDL");
        assert!(
            sqlite[0].contains(&format!(
                "computed TEXT GENERATED ALWAYS AS (\"{column_name}\") STORED"
            )),
            "{keyword}: {sqlite:?}"
        );
        let postgres =
            render_schema_statements_for(&schema, Dialect::Postgres).expect("Postgres DDL");
        assert!(
            postgres[0].contains(&format!(
                "\"computed\" character varying GENERATED ALWAYS AS (\"{column_name}\") STORED"
            )),
            "{keyword}: {postgres:?}"
        );
    }
}

#[test]
fn unicode_literals_are_preserved_and_invalid_source_encoding_is_rejected() {
    let source = r#"ActiveRecord::Schema[8.1].define(version: 1) do
  create_table "people", force: :cascade do |t|
    t.string "first_name"
    t.virtual "localized_name", type: :string, as: "coalesce(first_name, '未設定')", stored: true
  end
end
"#
    .as_bytes();
    let schema = ingest_schema(source, "db/schema.rb").expect("Unicode literal is valid SQL text");
    assert_eq!(
        column(table(&schema, "people"), "localized_name")
            .generated
            .as_ref()
            .unwrap()
            .expression,
        "coalesce(first_name, '未設定')"
    );

    let mut invalid_utf8 = br#"ActiveRecord::Schema[8.1].define(version: 1) do
  create_table "people", force: :cascade do |t|
    t.string "first_name"
    t.virtual "localized_name", type: :string, as: "coalesce(first_name, '"#
        .to_vec();
    invalid_utf8.push(0xff);
    invalid_utf8.extend_from_slice(
        br#"')", stored: true
  end
end
"#,
    );
    let parsed = std::panic::catch_unwind(|| ingest_schema(&invalid_utf8, "db/schema.rb"));
    let result = parsed.expect("invalid UTF-8 in an expression must not panic ingest");
    assert!(
        result.is_err(),
        "invalid UTF-8 must be an explicit ingest error"
    );
}

#[test]
fn structure_sql_keeps_supported_generated_columns_and_rejects_casts() {
    let source = br#"CREATE TABLE public.people (
    id bigint NOT NULL,
    first_name text,
    last_name text,
    display_name text GENERATED ALWAYS AS (first_name || ' ' || coalesce(last_name, '')) STORED
);
ALTER TABLE ONLY public.people ADD CONSTRAINT people_pkey PRIMARY KEY (id);
"#;
    let schema =
        roundhouse::ingest::structure_sql::ingest_structure_sql(source, "db/structure.sql")
            .expect("portable generated expression in structure.sql");
    let people = table(&schema, "people");
    let generated = column(people, "display_name")
        .generated
        .as_ref()
        .expect("generated column");
    assert_eq!(
        generated.expression,
        "first_name || ' ' || coalesce(last_name, '')"
    );
    assert_eq!(generated.storage, GeneratedColumnStorage::Stored);
    let postgres = render_schema_statements_for(&schema, Dialect::Postgres).expect("Postgres DDL");
    assert!(
        postgres[0]
            .contains("GENERATED ALWAYS AS (first_name || ' ' || coalesce(last_name, '')) STORED")
    );

    let unicode = r#"CREATE TABLE public.people (
    id bigint NOT NULL,
    first_name text,
    display_name text GENERATED ALWAYS AS (coalesce(first_name, '未設定')) STORED
);
ALTER TABLE ONLY public.people ADD CONSTRAINT people_pkey PRIMARY KEY (id);
"#
    .as_bytes();
    let schema =
        roundhouse::ingest::structure_sql::ingest_structure_sql(unicode, "db/structure.sql")
            .expect("Unicode text literal should survive structure.sql ingest");
    assert_eq!(
        column(table(&schema, "people"), "display_name")
            .generated
            .as_ref()
            .unwrap()
            .expression,
        "coalesce(first_name, '未設定')"
    );

    let casted = br#"CREATE TABLE public.people (
    id bigint NOT NULL,
    first_name text,
    display_name text GENERATED ALWAYS AS (first_name::text) STORED
);
ALTER TABLE ONLY public.people ADD CONSTRAINT people_pkey PRIMARY KEY (id);
"#;
    let error = roundhouse::ingest::structure_sql::ingest_structure_sql(casted, "db/structure.sql")
        .expect_err("PG-specific casts remain an explicit unsupported boundary")
        .to_string();
    assert!(
        error.contains("generated column dropped: people.display_name"),
        "{error}"
    );

    let mut invalid_utf8 = br#"CREATE TABLE public.people (
    id bigint NOT NULL,
    first_name text,
    display_name text GENERATED ALWAYS AS (coalesce(first_name, '"#
        .to_vec();
    invalid_utf8.push(0xff);
    invalid_utf8.extend_from_slice(
        br#"')) STORED
);
ALTER TABLE ONLY public.people ADD CONSTRAINT people_pkey PRIMARY KEY (id);
"#,
    );
    let error =
        roundhouse::ingest::structure_sql::ingest_structure_sql(&invalid_utf8, "db/structure.sql")
            .expect_err("lossy invalid UTF-8 must not pass as a generated literal")
            .to_string();
    assert!(
        error.contains("generated column dropped: people.display_name"),
        "{error}"
    );
}

#[test]
fn survey_mode_ledgers_unsupported_generated_expressions() {
    survey::activate();
    let result = ingest_schema(
        br#"ActiveRecord::Schema[8.1].define(version: 1) do
  create_table "people", force: :cascade do |t|
    t.string "first_name"
    t.virtual "display_name", type: :string, as: "lower(first_name)", stored: true
  end
end
"#,
        "db/schema.rb",
    );
    let gaps = survey::drain();
    let schema = result.expect("survey mode should retain the rest of the schema");
    assert!(
        table(&schema, "people")
            .columns
            .iter()
            .all(|column| column.name.as_str() != "display_name"),
        "unsupported generated columns should be dropped in survey mode"
    );
    assert_eq!(
        gaps.len(),
        1,
        "unsupported generated column must be ledgered: {gaps:?}"
    );
    assert!(
        gaps[0]
            .to_string()
            .contains("generated column dropped: people.display_name")
    );
}

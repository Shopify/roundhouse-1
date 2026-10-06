//! Real PostgreSQL driver contracts. Requires an explicit disposable-server
//! admin URL; the Ruby test creates and drops only its own named databases.
#[test]
#[ignore = "requires pg gem and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn postgres_driver_preserves_values_database_identity_and_leases() {
    assert!(std::env::var_os("ROUNDHOUSE_PG_TEST_URL").is_some(), "set ROUNDHOUSE_PG_TEST_URL");
    let result = std::process::Command::new("ruby")
        .arg("tests/db_pg_runtime.rb")
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("ruby");
    assert!(result.status.success(), "stdout={}\nstderr={}",
        String::from_utf8_lossy(&result.stdout), String::from_utf8_lossy(&result.stderr));
}

#[test]
#[ignore = "requires pg gem and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn shared_lowering_executes_postgresql_sql_in_emitted_ruby() {
    use roundhouse::expr::{Expr, ExprNode};
    use roundhouse::ident::{ClassId, Symbol, TableRef, VarId};
    use roundhouse::lower::arel::*;
    use roundhouse::schema::{Column, ColumnType, Schema, Table};
    use roundhouse::span::Span;
    use roundhouse::ty::Ty;

    let mut schema = Schema::default();
    schema.tables.insert(Symbol::from("entries"), Table {
        name: Symbol::from("entries"),
        columns: vec![
            Column { name: Symbol::from("entry_key"), col_type: ColumnType::Integer,
                nullable: false, default: None, primary_key: true },
            Column { name: Symbol::from("Title"), col_type: ColumnType::Text,
                nullable: false, default: None, primary_key: false },
            Column { name: Symbol::from("enabled"), col_type: ColumnType::Boolean,
                nullable: false, default: None, primary_key: false },
        ],
        indexes: vec![], foreign_keys: vec![], virtual_module: None,
    });
    let owner = ClassId(Symbol::from("Entry"));
    let title_var = Expr::new(Span::synthetic(), ExprNode::Var { name: Symbol::from("title"), id: VarId(0) });
    let table = TableRef(Symbol::from("entries"));
    let col = ColRef { table: table.clone(), column: Symbol::from("Title") };
    let runtime_title = Value::Runtime { expr: title_var, ty: ValueType::Str };
    let insert = ArelOp::Insert(Insert {
        table: table.clone(), assignments: vec![
            Assignment { column: Symbol::from("Title"), value: runtime_title.clone() },
            Assignment { column: Symbol::from("enabled"), value: Value::LiteralBool(true) },
        ], returns_rowid: true,
    });
    let count = ArelOp::Select(Select {
        table: table.clone(), columns: ColumnSpec::Count, single_record: false,
        conditions: Some(Predicate::Eq(col.clone(), runtime_title)),
        orders: vec![], limit: None, joins: vec![], preloads: vec![],
    });
    let update = ArelOp::Update(Update {
        table: table.clone(), assignments: vec![Assignment {
            column: Symbol::from("enabled"), value: Value::LiteralBool(false),
        }], conditions: Some(Predicate::Eq(col, Value::LiteralStr("l'été \\ hi".into()))),
    });
    let truncate = ArelOp::Delete(Delete { table, conditions: None });
    let methods = [("pg_insert(title)", insert), ("pg_count(title)", count),
        ("pg_update", update), ("pg_truncate", truncate)]
        .into_iter().map(|(name, op)| {
            let mut expr = PostgresVisitor.visit(&op, &schema, &owner);
            // The lowered result is typed by the ordinary pipeline. SQL
            // primitive emit is direct; no Rails runtime is loaded here.
            expr.ty = Some(Ty::Int);
            format!("def {name}\n{}\nend\n", roundhouse::emit::ruby::emit_expr(&expr))
        }).collect::<String>();
    let ddl = roundhouse::emit::shared::schema_sql::render_schema_statements_for(
        &schema, roundhouse::emit::shared::schema_sql::Dialect::Postgres,
    ).unwrap().into_iter().map(|sql| format!("Db.exec({})\n", serde_json::to_string(&sql).unwrap()))
        .collect::<String>();
    let script = format!(r#"
class PostgreSQLRuntimeTest
  {methods}
  def test_shared_lowered_sql
    {ddl}
    assert_equal 1, pg_insert("l'été \\ hi")
    assert_equal 2, pg_insert("other")
    assert_equal 1, pg_count("l'été \\ hi")
    assert_equal 0, pg_count("absent")
    pg_update
    assert_equal false, scalar('SELECT enabled FROM entries WHERE entry_key=1')
    Db.exec("BEGIN")
    pg_truncate
    assert_equal 0, pg_count("other")
    assert_equal 1, pg_insert("rolled back")
    Db.exec("ROLLBACK")
    assert_equal 1, pg_count("other")
    pg_truncate
    assert_equal 1, pg_insert("restart")
  end
end
"#);
    let result = std::process::Command::new("ruby")
        .args(["-r", "./tests/db_pg_runtime.rb", "-e", &script, "--", "--name", "test_shared_lowered_sql"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output().expect("ruby");
    assert!(result.status.success(), "emitted code={script}\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&result.stdout), String::from_utf8_lossy(&result.stderr));
}

fn postgres_model_app() -> roundhouse::app::App {
    use roundhouse::database::{Database, DatabaseAdapter};
    use roundhouse::ident::Symbol;
    use roundhouse::schema::{Column, ColumnType, Schema, Table};
    let mut app = roundhouse::ingest::ingest_app(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/tiny-blog"),
    ).expect("ingest fixture");
    let mut schema = Schema::default();
    schema.tables.insert(Symbol::from("entries"), Table {
        name: Symbol::from("entries"),
        columns: vec![
            Column { name: Symbol::from("id"), col_type: ColumnType::Integer,
                nullable: false, default: None, primary_key: true },
            Column { name: Symbol::from("Title"), col_type: ColumnType::Text,
                nullable: false, default: None, primary_key: false },
            Column { name: Symbol::from("enabled"), col_type: ColumnType::Boolean,
                nullable: false, default: None, primary_key: false },
        ], indexes: vec![], foreign_keys: vec![], virtual_module: None,
    });
    let model = roundhouse::ingest::model::ingest_model(
        b"class Entry < ActiveRecord::Base\n def self.named(title)\n where(Title: title).count\n end\n def matching\n Entry.where(Title: @Title).count\n end\nend\n",
        "app/models/entry.rb", &schema, &Default::default(),
    ).expect("ingest model").expect("model");
    app.models = vec![model];
    app.schema = schema;
    app.controllers.clear();
    app.databases = vec![Database {
        name: "primary".into(), adapter: DatabaseAdapter::Postgresql,
        primary: true, replica: false, database_tasks: true, schema_source: None,
        schema: None, schema_search_path: Some("runtime_test,public".into()),
        pragmas: vec![], migrations_paths: vec![],
    }];
    app
}

#[test]
fn model_sql_uses_owning_database_dialect() {
    let mut app = postgres_model_app();
    let files = roundhouse::emit::ruby::emit_lowered_models(&app);
    let pg = &files.iter().find(|f| f.path.to_str() == Some("app/models/entry.rb")).unwrap().content;
    assert!(pg.contains("insert_returning_int"), "{pg}");
    assert!(pg.contains("RETURNING"), "{pg}");
    assert!(pg.contains("$1"), "{pg}");
    assert!(pg.contains("TRUNCATE TABLE"), "{pg}");
    assert!(pg.contains("RESTART IDENTITY"), "{pg}");
    assert!(!pg.contains("last_insert_rowid"), "{pg}");
    assert!(!pg.contains("sqlite_sequence"), "{pg}");
    app.databases.clear();
    let files = roundhouse::emit::ruby::emit_lowered_models(&app);
    let sqlite = &files.iter().find(|f| f.path.to_str() == Some("app/models/entry.rb")).unwrap().content;
    assert!(sqlite.contains("last_insert_rowid"), "{sqlite}");
    assert!(sqlite.contains("sqlite_sequence"), "{sqlite}");
    assert!(!sqlite.contains("insert_returning_int"), "{sqlite}");
}

#[test]
#[ignore = "requires pg gem and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn emitted_model_crud_executes_on_postgresql() {
    let app = postgres_model_app();
    let files = roundhouse::emit::ruby::emit_lowered_models(&app);
    struct ProjectDir(std::path::PathBuf);
    impl Drop for ProjectDir {
        fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
    }
    let project = ProjectDir(std::env::temp_dir().join(format!(
        "roundhouse-pg-model-{}", std::process::id())));
    std::fs::create_dir(&project.0).expect("project directory");
    for file in files {
        let path = project.0.join(file.path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, file.content).unwrap();
    }
    std::os::unix::fs::symlink(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("runtime/ruby"),
        project.0.join("runtime"),
    ).unwrap();
    let model_path = serde_json::to_string(&project.0.join("app/models/entry.rb")).unwrap();
    let ddl = roundhouse::emit::shared::schema_sql::render_schema_statements_for(
        &app.schema, roundhouse::emit::shared::schema_sql::Dialect::Postgres,
    ).unwrap().into_iter().map(|sql| format!("Db.exec({})\n", serde_json::to_string(&sql).unwrap()))
        .collect::<String>();
    // The actual emitted model uses Roundhouse's shared framework, never Rails.
    // Project bootstrap/packaging is not certified by this isolated model gate.
    let script = format!(r#"
require_relative "runtime/ruby/active_support_ext"
require_relative "runtime/spinel/scaffold/ruby_overlay/runtime/active_support_time_parsing"
require {model_path}
class PostgreSQLRuntimeTest
  def test_emitted_model_crud
    {ddl}
    first = Entry.create!(Title: "l'été \\ hi", enabled: true)
    second = Entry.create!(Title: "false", enabled: false)
    assert_equal 1, first.id
    assert_equal 2, second.id
    assert_equal 2, Entry.count
    assert Entry.exists?(first.id)
    loaded = Entry.find(first.id)
    assert_equal "l'été \\ hi", loaded.Title
    assert_equal true, loaded.enabled
    assert_equal 1, loaded.matching
    loaded.Title = "absent"
    assert_equal 0, loaded.matching
    loaded.reload
    assert_equal false, Entry.find(second.id).enabled
    loaded.update!(Title: "updated", enabled: false)
    first.reload
    assert_equal "updated", first.Title
    assert_equal false, first.enabled
    Db.exec("BEGIN")
    Entry._adapter_truncate
    assert_equal 0, Entry.count
    assert_equal 1, Entry.create!(Title: "rollback", enabled: true).id
    Db.exec("ROLLBACK")
    assert_equal 2, Entry.count
    loaded.destroy!
    assert_equal 1, Entry.count
    refute Entry.exists?(loaded.id)
    Entry.delete_all
    assert_equal 0, Entry.count
    assert_equal 3, Entry.create!(Title: "no reset", enabled: true).id
    Entry._adapter_truncate
    assert_equal 1, Entry.create!(Title: "reset", enabled: true).id
  end
end
"#);
    let result = std::process::Command::new("ruby")
        .args(["-r", "./tests/db_pg_runtime.rb", "-e", &script, "--", "--name", "test_emitted_model_crud"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output().expect("ruby");
    assert!(result.status.success(), "emitted code={script}\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&result.stdout), String::from_utf8_lossy(&result.stderr));
}

//! Real PostgreSQL driver contracts. Requires an explicit disposable-server
//! admin URL; the Ruby test creates and drops only its own named databases.
#[test]
fn notification_runtime_delivers_events_synchronously_and_cleans_up() {
    let result = std::process::Command::new("ruby")
        .args(["tests/notifications_runtime.rb", "emitted"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("ruby");
    assert!(result.status.success(), "stdout={}\nstderr={}",
        String::from_utf8_lossy(&result.stdout), String::from_utf8_lossy(&result.stderr));
}

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
    let script = format!(r##"
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
"##);
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
    let files = roundhouse::project::target_files(
        &app,
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/tiny-blog"),
        roundhouse::project::BuildTarget::Ruby,
    ).expect("package PostgreSQL Ruby project");
    struct ProjectDir(std::path::PathBuf);
    impl Drop for ProjectDir {
        fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
    }
    let project = ProjectDir(std::env::temp_dir().join(format!(
        "roundhouse-pg-model-{}", std::process::id())));
    std::fs::create_dir(&project.0).expect("project directory");
    for (name, content) in files {
        let path = project.0.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
    let model_path = serde_json::to_string(&project.0.join("app/models/entry.rb")).unwrap();
    let main_path = serde_json::to_string(&project.0.join("main.rb")).unwrap();
    let helper_path = serde_json::to_string(&project.0.join("test/test_helper.rb")).unwrap();
    let runtime_path = serde_json::to_string(&project.0.join("runtime/db.rb")).unwrap();
    let support_path = serde_json::to_string(&project.0.join("runtime/active_support_ext.rb")).unwrap();
    let schema_path = serde_json::to_string(&project.0.join("config/schema.rb")).unwrap();
    // Execute the packaged driver, schema, shared framework and model. The
    // authored Core bootstrap is a separate integration still being repaired.
    let script = format!(r##"
require {runtime_path}
require {support_path}
require {schema_path}
require {model_path}
ENV["ROUNDHOUSE_DATABASE_URL"] = DB_OPTIONS.merge(dbname: DATABASE_NAMES[0]).compact.map {{ |k,v| "#{{k}}=#{{v}}" }}.join(" ")
bootstrap_connection = PG.connect(DB_OPTIONS.merge(dbname: DATABASE_NAMES[0]))
bootstrap_connection.exec("CREATE SCHEMA runtime_test").clear
bootstrap_connection.finish
require {helper_path}
Db.close
class PostgreSQLRuntimeTest
  def test_emitted_model_crud
    require {main_path}
    Db.close
    ActiveRecord.adapter = nil
    Main.configure_default_adapter!
    Schema.statements.each {{ |sql| Db.exec(sql) }}
    assert_equal "PostgreSQL", ActiveRecord::Base.connection.adapter_name
    assert_equal "TRUE", ActiveRecord::Base.connection.quote(true)
    assert_equal "FALSE", ActiveRecord::Base.connection.quote(false)
    assert_equal "runtime_test,public", scalar("SHOW search_path")
    assert_equal DATABASE_NAMES[0], scalar("SELECT current_database()")
    assert_equal [{{ "number" => 1 }}], ActiveRecord::Base.connection.execute('SELECT 1 AS number').to_a
    error = assert_raises(ActiveRecord::StatementInvalid) {{ ActiveRecord::Base.connection.execute('SELECT * FROM no_such_table') }}
    assert_equal 'SELECT * FROM no_such_table', error.sql
    assert_equal [], error.binds
    events = []
    seen_during_block = false
    listener = ActiveSupport::Notifications.subscribe("sql.active_record") {{ |event| events << event }}
    begin
      ActiveSupport::Notifications.subscribed(lambda {{ |name, start, finish, id, payload|
        seen_during_block = true
        assert_equal "sql.active_record", name
        assert finish >= start
        refute_empty id
        assert_equal 'SELECT 7 AS notified', payload[:sql]
      }}, "sql.active_record") do
        assert_equal [{{ "notified" => 7 }}], ActiveRecord::Base.connection.execute('SELECT 7 AS notified').to_a
        assert seen_during_block, "SQL callbacks must run before the query returns"
      end
      assert_equal 1, events.length
      assert_equal 'SELECT 7 AS notified', events.last.payload[:sql]
      assert_operator events.last.duration, :>=, 0
      failed = assert_raises(ActiveRecord::StatementInvalid) {{ ActiveRecord::Base.connection.execute('SELECT * FROM missing_notified_table') }}
      assert_equal 2, events.length
      assert_same failed, events.last.payload[:exception_object]
      assert_equal [failed.class.name, failed.message], events.last.payload[:exception]
      assert_equal failed.sql, events.last.payload[:sql]
      handle = Db.prepare('SELECT $1::integer / $2::integer')
      Db.bind_int(handle, 1, 5)
      Db.bind_int(handle, 2, 0)
      bound_error = assert_raises(ActiveRecord::StatementInvalid) {{ Db.step?(handle) }}
      Db.finalize(handle)
      assert_equal [5, 0], bound_error.binds
      assert_same bound_error, events.last.payload[:exception_object]
      assert_equal [5, 0], events.last.payload[:binds]
    ensure
      ActiveSupport::Notifications.unsubscribe(listener)
    end
    count_after_unsubscribe = events.length
    ActiveRecord::Base.connection.execute('SELECT 8')
    assert_equal count_after_unsubscribe, events.length
    first = Entry.create!(Title: "l'été \\ hi", enabled: true)
    second = Entry.create!(Title: "false", enabled: false)
    assert_equal 1, first.id
    assert_equal 2, second.id
    assert_equal 2, Entry.count
    assert_equal 1, Entry.named("l'été \\ hi")
    assert_equal 0, Entry.named("absent")
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
    connection = ActiveRecord::Base.connection
    connection.begin_isolated_db_transaction(:serializable)
    assert_equal "serializable", scalar("SHOW transaction_isolation")
    Entry.create!(Title: "discard isolated", enabled: true)
    connection.exec_rollback_db_transaction
    assert_equal 1, Entry.count
    assert_raises(ActiveRecord::TransactionIsolationError) {{ connection.begin_isolated_db_transaction(:invalid) }}
  end
end
"##);
    let result = std::process::Command::new("ruby")
        .args(["-r", "./tests/db_pg_runtime.rb", "-e", &script, "--", "--name", "test_emitted_model_crud"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output().expect("ruby");
    println!("packaged model: {}", String::from_utf8_lossy(&result.stdout).trim());
    assert!(result.status.success(), "emitted code={script}\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&result.stdout), String::from_utf8_lossy(&result.stderr));
}

#[test]
fn postgres_project_packages_matching_driver_schema_and_gem() {
    use roundhouse::project::{target_files, BuildTarget};
    let app = postgres_model_app();
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/tiny-blog");
    let files = target_files(&app, &fixture, BuildTarget::Ruby).unwrap();
    let source = |path: &str| files.iter().find(|(p, _)| p == path).unwrap().1.as_str();
    assert!(source("runtime/db.rb").contains("require \"pg\""));
    assert!(!source("runtime/db.rb").contains("require \"sqlite3\""));
    assert!(source("Gemfile").contains("gem \"pg\""));
    assert!(source("test/test_helper.rb").contains("DatabaseConfiguration.configure!"));
    assert!(!source("test/test_helper.rb").contains("Db.configure(_test_db_path)"));
    assert!(!source("main.rb").contains("SqliteAdapter.configure"));
    assert!(source("main.rb").contains("DatabaseConfiguration.configure!"));
    assert!(!files.iter().any(|(p, _)| p == "runtime/active_record_sqlite3_adapter.rb" || p == "runtime/sqlite_adapter.rb"));
    assert!(!source("Gemfile").contains("gem \"sqlite3\""));
    assert!(!files.iter().any(|(p, _)| p == "Gemfile.lock"));
    assert!(source("config/schema.rb").contains("serial PRIMARY KEY"));
    assert!(source("config/schema.rb").contains("boolean NOT NULL"));
    assert_eq!(files.iter().filter(|(p, _)| p == "runtime/db.rb").count(), 1);
    assert_eq!(files.iter().filter(|(p, _)| p == "config/schema.rb").count(), 1);
    for target in [BuildTarget::Jruby, BuildTarget::Spinel, BuildTarget::Rust] {
        let error = target_files(&app, &fixture, target).unwrap_err();
        assert!(error.contains("postgresql") && error.contains("not substituted"), "{error}");
    }
}

#[test]
fn postgres_schema_error_is_not_replaced_by_sqlite_or_empty_ddl() {
    let mut app = postgres_model_app();
    app.schema.tables.get_mut(&roundhouse::ident::Symbol::from("entries")).unwrap()
        .virtual_module = Some(roundhouse::schema::VirtualModule {
            module: "fts5".into(), args: vec!["Title".into()],
        });
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/tiny-blog");
    let error = roundhouse::project::target_files(&app, &fixture,
        roundhouse::project::BuildTarget::Ruby).unwrap_err();
    assert!(error.contains("PostgreSQL schema") && error.contains("virtual table"), "{error}");
}

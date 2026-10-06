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

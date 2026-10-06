# Real PostgreSQL contract tests. Databases are created by this test process
# and dropped afterwards; no app database is used or truncated.
require "minitest/autorun"
require "timeout"
require_relative "../runtime/spinel/db_pg_cruby"

admin_url = ENV.fetch("ROUNDHOUSE_PG_TEST_URL")
ADMIN = PG.connect(admin_url)
DATABASE_NAMES = ["roundhouse_pg_runtime_#{Process.pid}", "roundhouse_pg_runtime_archive_#{Process.pid}"].freeze
DATABASE_NAMES.each { |name| ADMIN.exec("CREATE DATABASE #{ADMIN.quote_ident(name)}").clear }
DB_OPTIONS = ADMIN.conninfo_hash.slice(:host, :port, :user, :password).freeze
Minitest.after_run do
  Db.close
  DATABASE_NAMES.each { |name| ADMIN.exec("DROP DATABASE #{ADMIN.quote_ident(name)}").clear }
  ADMIN.finish
end

class PostgreSQLRuntimeTest < Minitest::Test
  def setup
    Db.configure(DB_OPTIONS.merge(dbname: DATABASE_NAMES[0]), pool_size: 4, search_path: "runtime_test,public")
    Db.configure_database("archive", DB_OPTIONS.merge(dbname: DATABASE_NAMES[1]), pool_size: 2, search_path: "runtime_test,public")
    ["primary", "archive"].each do |name|
      Db.exec_on(name, "DROP SCHEMA IF EXISTS runtime_test CASCADE")
      Db.exec_on(name, "CREATE SCHEMA runtime_test")
      Db.exec_on(name, "CREATE TABLE items (id bigserial PRIMARY KEY, label text, enabled boolean, amount numeric(30,10), payload bytea, day date)")
    end
  end

  def teardown
    Db.close
  end

  def scalar(sql, cacheable: false)
    statement = Db.prepare(sql, cacheable: cacheable)
    assert Db.step?(statement)
    Db.column_value(statement, 0)
  ensure
    Db.finalize(statement)
  end

  def test_typed_values_null_escaping_and_real_returning
    assert_equal 1, Db.insert_returning_int("INSERT INTO items(label,enabled,amount,payload,day) VALUES (#{Db.escape_string("l'été \\ hi")},TRUE,12345678901234567890.1234567890,#{Db.escape_string("a\0\xff".b)},'2026-10-06') RETURNING id")
    statement = Db.prepare("SELECT label,enabled,amount,payload,day FROM items")
    assert Db.step?(statement)
    assert_equal "l'été \\ hi", Db.column_text(statement, 0)
    assert_equal true, Db.column_bool(statement, 1)
    assert_equal BigDecimal("12345678901234567890.1234567890"), Db.column_value(statement, 2)
    assert_equal "a\0\xff".b, Db.column_value(statement, 3)
    assert_equal Date.new(2026,10,6), Db.column_value(statement, 4)
    refute Db.step?(statement)
    Db.finalize(statement)
    Db.exec("INSERT INTO items(label,enabled) VALUES (NULL,FALSE)")
    statement = Db.prepare("SELECT label,enabled,amount FROM items WHERE id=2")
    assert Db.step?(statement)
    assert_nil Db.column_text_opt(statement, 0)
    assert_equal false, Db.column_bool_opt(statement, 1)
    assert_nil Db.column_value(statement, 2)
    Db.finalize(statement)
  end

  def test_trigger_sequence_cannot_replace_returned_identity
    Db.exec("CREATE TABLE audit (id bigserial PRIMARY KEY)")
    Db.exec("SELECT setval('audit_id_seq',900)")
    Db.exec("CREATE FUNCTION record_insert() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN INSERT INTO audit DEFAULT VALUES; RETURN NEW; END $$")
    Db.exec("CREATE TRIGGER record_insert AFTER INSERT ON items FOR EACH ROW EXECUTE FUNCTION record_insert()")
    assert_equal 1, Db.insert_returning_int("INSERT INTO items DEFAULT VALUES RETURNING id")
    assert_equal 901, scalar("SELECT id FROM audit")
    assert_raises(NotImplementedError) { Db.last_insert_rowid }
  end

  def test_database_identity_search_path_and_rollback
    assert_equal "runtime_test,public", scalar("SHOW search_path")
    Db.exec("BEGIN")
    Db.exec("INSERT INTO items(label) VALUES ('discard')")
    assert_equal 1, scalar("SELECT COUNT(*) FROM items")
    Db.exec("ROLLBACK")
    assert_equal 0, scalar("SELECT COUNT(*) FROM items")
    Db.exec_on("archive", "INSERT INTO items(label) VALUES ('archive')")
    assert_equal 0, scalar("SELECT COUNT(*) FROM items")
    Db.with_database("archive") { assert_equal "archive", scalar("SELECT label FROM items") }
    assert_raises(ArgumentError) { Db.exec_on("missing", "SELECT 1") }
    assert_equal DATABASE_NAMES[0], scalar("SELECT current_database()")
  end

  def test_postgres_placeholders_and_nested_identical_handles
    first = Db.prepare("SELECT $1::int, $2::boolean, $3::text")
    second = Db.prepare("SELECT $1::int, $2::boolean, $3::text")
    Db.bind_int(first,1,42); Db.bind_bool(first,2,false); Db.bind_text(first,3,"one")
    Db.bind_int(second,1,91); Db.bind_bool(second,2,true); Db.bind_text(second,3,"two")
    assert Db.step?(first)
    assert Db.step?(second)
    assert_equal 42, Db.column_int(first,0)
    assert_equal 91, Db.column_int(second,0)
    assert_equal false, Db.column_bool(first,1)
    assert_equal true, Db.column_bool(second,1)
    Db.finalize(second)
    assert_equal "one", Db.column_text(first,2)
    Db.finalize(first)
  end

  def test_query_cache_is_per_database_and_invalidates_on_write
    Db.query_cache_begin
    log = Db.capture_sql do
      assert_equal 0, scalar("SELECT COUNT(*) FROM items", cacheable: true)
      assert_equal 0, scalar("SELECT COUNT(*) FROM items", cacheable: true)
      Db.exec("INSERT INTO items DEFAULT VALUES")
      assert_equal 1, scalar("SELECT COUNT(*) FROM items", cacheable: true)
      Db.with_database("archive") { assert_equal 0, scalar("SELECT COUNT(*) FROM items", cacheable: true) }
    end
    assert_equal 3, log.count { |sql| sql == "SELECT COUNT(*) FROM items" }
  ensure
    Db.query_cache_end
  end

  def test_truncate_restarts_identity_and_rolls_back
    Db.exec("INSERT INTO items DEFAULT VALUES")
    Db.exec("BEGIN")
    Db.exec("TRUNCATE TABLE items RESTART IDENTITY")
    assert_equal 1, Db.insert_returning_int("INSERT INTO items DEFAULT VALUES RETURNING id")
    Db.exec("ROLLBACK")
    assert_equal 2, Db.insert_returning_int("INSERT INTO items DEFAULT VALUES RETURNING id")
  end

  def test_thread_lease_does_not_inherit_parent_transaction
    Db.with_connection do
      Db.exec("BEGIN")
      Db.exec("INSERT INTO items(label) VALUES ('uncommitted')")
      parent_pid = scalar("SELECT pg_backend_pid()")
      result = Thread.new do
        Db.with_connection do
          [scalar("SELECT pg_backend_pid()"), scalar("SELECT COUNT(*) FROM items")]
        end
      end.value
      refute_equal parent_pid, result[0]
      assert_equal 0, result[1]
      Db.exec("ROLLBACK")
    end
  end

  def test_fiber_lease_does_not_inherit_parent_and_rejects_foreign_handle
    Db.with_connection do
      parent_pid = scalar("SELECT pg_backend_pid()")
      statement = Db.prepare("SELECT 1")
      Fiber.new do
        Db.with_connection do
          refute_equal parent_pid, scalar("SELECT pg_backend_pid()")
          assert_raises(RuntimeError) { Db.step?(statement) }
        end
      end.resume
      assert Db.step?(statement)
      Db.finalize(statement)
    end
  end

  def test_failed_transaction_is_cleaned_on_release
    Db.with_connection do
      Db.exec("BEGIN")
      Db.exec("INSERT INTO items DEFAULT VALUES")
      assert_raises(PG::UndefinedTable) { Db.exec("SELECT * FROM absent") }
    end
    Db.with_connection do
      assert_equal 0, scalar("SELECT COUNT(*) FROM items")
    end
  end

  def test_lease_restored_after_nested_database_exception
    Db.with_connection do
      connection = Db.current_dbh
      assert_raises(RuntimeError) do
        Db.with_database("archive") { Db.with_connection { raise "probe" } }
      end
      assert_same connection, Db.current_dbh
      Db.with_connection { assert_same connection, Db.current_dbh }
    end
    refute Db.in_lease?
  end

  def test_handle_cannot_follow_a_reused_connection_into_a_new_lease
    statement = nil
    Db.with_connection { statement = Db.prepare("SELECT 1") }
    Db.with_connection do
      assert_raises(RuntimeError) { Db.step?(statement) }
    end
  end

  def test_exception_releases_connection_and_rolls_back_successful_open_transaction
    assert_raises(RuntimeError) do
      Db.with_connection do
        Db.exec("BEGIN")
        Db.exec("INSERT INTO items DEFAULT VALUES")
        raise "abort request"
      end
    end
    refute Db.in_lease?
    Db.with_connection { assert_equal 0, scalar("SELECT COUNT(*) FROM items") }
    Db.with_connection do
      Db.exec("BEGIN")
      Db.exec("INSERT INTO items DEFAULT VALUES")
    end
    Db.with_connection { assert_equal 0, scalar("SELECT COUNT(*) FROM items") }
  end

  def test_reconnect_after_fork_preserves_schema_identity
    reader, writer = IO.pipe
    pid = fork do
      reader.close
      begin
        writer.write("#{scalar('SHOW search_path')}|#{scalar('SELECT current_database()')}")
        Db.close
      rescue => error
        writer.write("ERROR:#{error.class}:#{error.message}")
      ensure
        writer.close
      end
      exit! 0
    end
    writer.close
    child = reader.read
    Process.wait(pid)
    reader.close
    assert_equal "runtime_test,public|#{DATABASE_NAMES[0]}", child
    assert_equal DATABASE_NAMES[0], scalar("SELECT current_database()")
    Db.with_database("archive") { assert_equal DATABASE_NAMES[1], scalar("SELECT current_database()") }
  end

  def test_arbitrary_sql_is_not_replayed_and_release_invalidates_cache
    Db.query_cache_begin
    Db.with_connection do
      assert_equal 1, scalar("SELECT nextval('items_id_seq')")
      assert_equal 2, scalar("SELECT nextval('items_id_seq')")
      Db.exec("BEGIN")
      Db.exec("INSERT INTO items DEFAULT VALUES")
      assert_equal 1, scalar("SELECT COUNT(*) FROM items", cacheable: true)
    end
    Db.with_connection do
      assert_equal 0, scalar("SELECT COUNT(*) FROM items", cacheable: true)
    end
  ensure
    Db.query_cache_end
  end

  def test_repeatable_read_and_savepoint_rollback_use_the_leased_connection
    Db.with_connection do
      Db.exec("BEGIN ISOLATION LEVEL REPEATABLE READ")
      assert_equal 0, scalar("SELECT COUNT(*) FROM items")
      Thread.new { Db.with_connection { Db.exec("INSERT INTO items(label) VALUES ('other transaction')") } }.value
      assert_equal 0, scalar("SELECT COUNT(*) FROM items")
      Db.exec("SAVEPOINT inner_work")
      Db.exec("INSERT INTO items(label) VALUES ('discard savepoint')")
      assert_equal 1, scalar("SELECT COUNT(*) FROM items")
      Db.exec("ROLLBACK TO SAVEPOINT inner_work")
      assert_equal 0, scalar("SELECT COUNT(*) FROM items")
      Db.exec("COMMIT")
      assert_equal 1, scalar("SELECT COUNT(*) FROM items")
    end
  end
end

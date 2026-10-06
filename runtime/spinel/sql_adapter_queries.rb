# Shared query/row facade. Backend modules supply identifier quoting,
# generated-insert identity and truncate semantics; Db owns driver I/O.
module SqlAdapterQueries
  def configure(database_path)
    Db.configure(database_path)
  end

  def execute_ddl(sql)
    Db.exec(sql)
  end

  # Rows affected by the last INSERT/UPDATE/DELETE — Rails'
  # `delete_all`/`update_all` return this count.
  def changes
    Db.changes
  end

  def all(table)
    select_rows("SELECT * FROM #{quote_identifier(table)}")
  end

  def find(table, id)
    rows = select_rows("SELECT * FROM #{quote_identifier(table)} WHERE id = #{Db.escape_int(id)} LIMIT 1")
    rows.empty? ? nil : rows[0]
  end

  def where(table, conditions)
    sql = "SELECT * FROM #{quote_identifier(table)}"
    sql += " WHERE #{build_where(conditions)}" unless conditions.empty?
    select_rows(sql)
  end

  def count(table)
    stmt = Db.prepare("SELECT COUNT(*) FROM #{quote_identifier(table)}")
    n = Db.step?(stmt) ? Db.column_int(stmt, 0) : 0
    Db.finalize(stmt)
    n
  end

  def exists?(table, id)
    stmt = Db.prepare("SELECT 1 FROM #{quote_identifier(table)} WHERE id = #{Db.escape_int(id)} LIMIT 1")
    found = Db.step?(stmt)
    Db.finalize(stmt)
    found
  end

  def insert(table, attrs)
    cols = attrs.keys
    values = cols.map { |k| escape_value(attrs[k]) }
    sql = if cols.empty?
      "INSERT INTO #{quote_identifier(table)} DEFAULT VALUES"
    else
      names = cols.map { |k| quote_identifier(k) }
      "INSERT INTO #{quote_identifier(table)} (#{names.join(", ")}) VALUES (#{values.join(", ")})"
    end
    insert_result(sql)
  end

  def update(table, id, attrs)
    assigns = attrs.keys.map { |k| "#{quote_identifier(k)} = #{escape_value(attrs[k])}" }
    sql = "UPDATE #{quote_identifier(table)} SET #{assigns.join(", ")} WHERE id = #{Db.escape_int(id)}"
    Db.exec(sql)
  end

  def delete(table, id)
    Db.exec("DELETE FROM #{quote_identifier(table)} WHERE id = #{Db.escape_int(id)}")
  end

  # `Model.delete_all` — bulk row delete with ActiveRecord semantics:
  # rows go, the autoincrement counter stays (unlike `truncate`).
  def delete_all(table)
    Db.exec("DELETE FROM #{quote_identifier(table)}")
  end

  # Internal helpers — walk a prepared statement and build a row hash
  # keyed by column name. Values read through `Db.column_value` — the
  # driver's NATIVE types (Integer/Float/String, nil for NULL), which
  # is what ActiveRecord hands the app: `group_by(&:fk)[nil]` finds
  # root rows, integer columns compare as integers, and `pluck(:fk)`
  # returns keys that match the ones model attributes carry. All three
  # shims answer this now, the FFI one by storage-class dispatch. Each
  # row is its own Hash so the caller gets a stable copy after
  # `finalize`.
  def select_rows(sql)
    stmt = Db.prepare(sql)
    rows = []
    ncols = Db.column_count(stmt)
    # Column names can't change between rows — resolve them once, not
    # rows×cols times (each column_name is a shim call, and hydrating
    # a 1000-row result was paying 24,000 of them).
    names = []
    i = 0
    while i < ncols
      names.push(Db.column_name(stmt, i))
      i += 1
    end
    while Db.step?(stmt)
      row = {}
      i = 0
      while i < ncols
        row[names[i]] = Db.column_value(stmt, i)
        i += 1
      end
      rows << row
    end
    rows
  ensure
    Db.finalize(stmt) if stmt
  end

  def build_where(conditions)
    conditions.map { |k, v| v.nil? ? "#{quote_identifier(k)} IS NULL" : "#{quote_identifier(k)} = #{escape_value(v)}" }.join(" AND ")
  end

  # A bind value rendered as SQL text.
  #
  # A Time goes through `ActiveSupport.format_db_time`, the SAME writer
  # every temporal COLUMN is stored with, and that is the whole point:
  # comparison here is string-vs-string, so a bind written in any other
  # format silently answers the wrong rows rather than erroring.
  # `Time#to_s` is that other format — it renders local time with a
  # zone offset (`2026-08-22 15:30:00 -0400`) where the column holds
  # UTC with microseconds (`2026-08-22 19:30:00.123456`). Both start
  # with the same date, so the comparison reaches the HOUR and answers
  # on the zone difference: campfire's `where("created_at < ?",
  # message.created_at)` matched NOTHING and its `>` twin matched
  # EVERYTHING, which is a message list paginating backwards into an
  # empty page and forwards into the whole room.
  def escape_value(v)
    if v.is_a?(Integer)
      Db.escape_int(v)
    elsif v == true
      Db.escape_bool(true)
    elsif v == false
      Db.escape_bool(false)
    elsif v.nil?
      "NULL"
    elsif v.is_a?(Time)
      Db.escape_string(ActiveSupport.format_db_time(v).to_s)
    elsif defined?(Date) && v.is_a?(Date)
      # Date-only predicates compare against YYYY-MM-DD text, never a
      # zoned timestamp. Date + format_db_date load only with the Date
      # package (matz/spinel#7334).
      Db.escape_string(ActiveSupport.format_db_date(v).to_s)
    else
      Db.escape_string(v.to_s)
    end
  end
end

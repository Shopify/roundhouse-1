# SQLite backend hooks for the shared query facade.
require_relative "sql_adapter_queries"

module SqliteAdapter
  extend SqlAdapterQueries

  def self.quote_identifier(name)
    name.to_s
  end

  def self.insert_result(sql)
    Db.exec(sql)
    Db.last_insert_rowid
  end

  def self.truncate(table)
    Db.exec("DELETE FROM #{table}")
    Db.exec("DELETE FROM sqlite_sequence WHERE name = #{Db.escape_string(table)}")
  end
end

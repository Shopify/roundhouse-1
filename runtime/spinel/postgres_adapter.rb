# PostgreSQL hooks over the same row/query facade as SQLite.
require_relative "sql_adapter_queries"

module PostgresAdapter
  extend SqlAdapterQueries

  def self.quote_identifier(name)
    '"' + name.to_s.gsub('"', '""') + '"'
  end

  def self.insert_result(sql)
    Db.insert_returning_int(sql + ' RETURNING "id"')
  end

  def self.truncate(table)
    Db.exec("TRUNCATE TABLE #{quote_identifier(table)} RESTART IDENTITY")
  end
end

# PostgreSQL implementation of the emitted Db primitive, using the pg gem.
# SQL arrives in PostgreSQL dialect from shared lowering. This driver never
# rewrites SQL, calls Rails, or substitutes another database adapter.
require "pg"
require "bigdecimal"
require "date"

module Db
  Pool = Struct.new(:config, :size, :connections, :free, :mutex, :cv, :pid)
  @databases = {}
  @primary = "primary"
  @owner_pid = Process.pid
  STMT_CACHE_CAP = 4096

  def self.configure(config, pool_size: ENV.fetch("RAILS_MAX_THREADS", "3").to_i, search_path: nil)
    configure_database("primary", config, pool_size: pool_size, search_path: search_path)
  end

  # Endpoint configuration is supplied separately from the schema manifest:
  # manifests contain identity and hashes, never credentials.
  def self.configure_database(name, config, pool_size: 3, search_path: nil)
    raise ArgumentError, "pool_size must be positive" unless pool_size.positive?
    raise ArgumentError, "database already configured: #{name}" if @databases.key?(name.to_s)
    options = if config.is_a?(Hash)
      config.dup
    else
      # Use libpq's parser for both connection strings and PostgreSQL URIs.
      # Passing a whole connection string as a dbname loses its endpoint.
      PG::Connection.conninfo_parse(config.to_s).each_with_object({}) do |option, out|
        out[option[:keyword].to_sym] = option[:val] if option[:val]
      end
    end
    options = options.transform_keys(&:to_sym)
    options[:options] = options.fetch(:options, "")
    pool = Pool.new(options.freeze, pool_size, [], [], Mutex.new, ConditionVariable.new, Process.pid)
    begin
      pool_size.times do
        conn = PG.connect(options)
        pool.connections << conn
        conn.instance_variable_set(:@rh_pg_search_path, search_path)
        conn.exec_params("SELECT pg_catalog.set_config('search_path', $1, false)", [search_path]).clear if search_path
        conn.exec("SET TIME ZONE 'UTC'").clear
        conn.exec("SET standard_conforming_strings = on").clear
        conn.type_map_for_results = PG::BasicTypeMapForResults.new(conn)
        conn.instance_variable_set(:@rh_pg_changes, 0)
        conn.instance_variable_set(:@rh_pg_cache, {})
        pool.free << conn
      end
    rescue Exception
      pool.connections.each { |c| c.finish unless c.finished? }
      raise
    end
    @databases[name.to_s] = pool
    nil
  end

  # Thread#[] is fiber-local, unlike inheritable Fiber storage. Neither a
  # newly spawned thread nor a sibling fiber inherits a parent's lease.
  def self.context
    Thread.current[:roundhouse_pg_db] ||= { name: @primary, leases: {}, cache: nil, capture: nil }
  end

  def self.database_pool
    adopt_after_fork
    name = context[:name]
    pool = @databases.fetch(name) { raise ArgumentError, "unconfigured database: #{name}" }
    pool
  end

  def self.adopt_after_fork
    return if @owner_pid == Process.pid
    selected = context[:name]
    configurations = @databases.map do |name, pool|
      search_path = pool.connections.first.instance_variable_get(:@rh_pg_search_path)
      pool.connections.each do |conn|
        # Redirect only the child's descriptor before libpq frees it. This
        # is Rails' PostgreSQL discard! strategy: a plain finish would send
        # Terminate over the shared socket and disconnect the parent.
        conn.socket_io.reopen(IO::NULL)
        conn.finish
      end
      [name, pool.config, pool.size, search_path]
    end
    @databases = {}
    @owner_pid = Process.pid
    Thread.current[:roundhouse_pg_db] = { name: selected, leases: {}, cache: nil, capture: nil }
    configurations.each do |name, config, size, path|
      configure_database(name, config, pool_size: size, search_path: path)
    end
  end

  def self.current_dbh
    pool = database_pool
    context[:leases][context[:name]] ||= pool.mutex.synchronize do
      pool.cv.wait(pool.mutex) while pool.free.empty?
      conn = pool.free.shift
      conn.instance_variable_set(:@rh_pg_lease, Object.new)
      conn
    end
  end

  def self.in_lease?
    database_pool
    context[:leases].key?(context[:name])
  end

  def self.with_database(name)
    previous = context[:name]
    context[:name] = name.to_s
    database_pool # reject an unknown identity even when the block does no SQL
    yield
  ensure
    context[:name] = previous
  end

  def self.with_connection
    return yield if in_lease?
    current_dbh
    begin
      yield
    ensure
      release_connection
    end
  end

  def self.release_connection
    name = context[:name]
    conn = context[:leases].delete(name)
    return unless conn
    pool = @databases.fetch(name)
    begin
      conn.exec("ROLLBACK").clear if conn.transaction_status != PG::PQTRANS_IDLE
    ensure
      context[:cache]&.clear
      conn.instance_variable_set(:@rh_pg_lease, nil)
      pool.mutex.synchronize do
        pool.free << conn
        pool.cv.signal
      end
    end
  end

  def self.exec_on(name, sql)
    with_database(name) { with_connection { exec(sql) } }
  end

  def self.close
    adopt_after_fork
    context[:leases].keys.each { |name| with_database(name) { release_connection } }
    @databases.each_value do |pool|
      pool.mutex.synchronize do
        raise "cannot close a leased PostgreSQL pool" unless pool.free.size == pool.size
      end
    end
    @databases.each_value { |pool| pool.connections.each { |conn| conn.finish } }
    @databases.clear
    Thread.current[:roundhouse_pg_db] = nil
    nil
  end

  def self.exec(sql)
    record_query(sql)
    context[:cache]&.clear
    conn = current_dbh
    result = conn.exec(sql)
    conn.instance_variable_set(:@rh_pg_changes, result.cmd_tuples)
    nil
  rescue PG::Error => error
    raise translate(error, sql)
  ensure
    result&.clear
  end

  # The lowerer appends a typed RETURNING key, so trigger side effects or
  # unrelated sequences cannot affect the returned insert identity.
  def self.insert_returning_int(sql)
    record_query(sql)
    context[:cache]&.clear
    conn = current_dbh
    result = conn.exec(sql)
    raise "INSERT RETURNING must produce one key" unless result.ntuples == 1 && result.nfields == 1
    conn.instance_variable_set(:@rh_pg_changes, result.cmd_tuples)
    Integer(result.getvalue(0, 0))
  rescue PG::Error => error
    raise translate(error, sql)
  ensure
    result&.clear
  end

  def self.last_insert_rowid
    raise NotImplementedError, "PostgreSQL inserts require an explicit RETURNING key"
  end

  def self.changes
    current_dbh.instance_variable_get(:@rh_pg_changes)
  end

  def self.translate(error, sql)
    return error unless defined?(ActiveRecord::StatementInvalid)
    state = error.result&.error_field(PG::Result::PG_DIAG_SQLSTATE)
    class_name = {
      "23505" => :RecordNotUnique, "23502" => :NotNullViolation,
      "23503" => :InvalidForeignKey, "23514" => :CheckViolation,
      "40001" => :SerializationFailure, "40P01" => :Deadlocked,
      "55P03" => :LockWaitTimeout, "57014" => :QueryCanceled
    }.fetch(state, :StatementInvalid)
    klass = ActiveRecord.const_defined?(class_name) ? ActiveRecord.const_get(class_name) : ActiveRecord::StatementInvalid
    klass.new("#{error.class}: #{error.message}", sql: sql, binds: [])
  end

  def self.unique_violation?(message)
    message.to_s.include?("PG::UniqueViolation")
  end

  # Each handle owns its result, even for nested identical SQL. Server-side
  # prepared statements belong to the connection and have a bounded cache.
  def self.prepare(sql, cacheable: false)
    conn = current_dbh
    { conn: conn, sql: sql, binds: [], result: nil, row: nil, pos: 0, closed: false,
      owner: [Thread.current, Fiber.current], database: context[:name],
      lease: conn.instance_variable_get(:@rh_pg_lease), cacheable: cacheable }
  end

  def self.validate_handle(handle)
    raise "finalized PostgreSQL statement" if handle[:closed]
    raise "PostgreSQL statement belongs to another thread or fiber" unless handle[:owner] == [Thread.current, Fiber.current]
    raise "PostgreSQL statement belongs to another database lease" unless handle[:database] == context[:name] && handle[:conn].equal?(current_dbh)
    raise "PostgreSQL statement outlived its lease" unless handle[:lease].equal?(handle[:conn].instance_variable_get(:@rh_pg_lease))
  end

  def self.execute_handle(handle)
    validate_handle(handle)
    return if handle[:result]
    sql, conn, binds = handle.values_at(:sql, :conn, :binds)
    key = [context[:name], sql]
    # Raw SQL may lock rows or call volatile functions. Cacheability is a
    # caller decision; the default never replays arbitrary SQL. Lowered SQL
    # can opt in once its read semantics have been proven.
    if handle[:cacheable] && binds.empty? && (cached = context[:cache]&.[](key))
      handle[:rows], handle[:names] = cached
      handle[:result] = :cached
      return
    end
    record_query(sql)
    if binds.empty?
      result = conn.exec(sql)
    else
      cache = conn.instance_variable_get(:@rh_pg_cache)
      name = cache.delete(sql)
      unless name
        if cache.size >= STMT_CACHE_CAP
          _, oldest = cache.shift
          conn.exec("DEALLOCATE #{oldest}").clear
        end
        serial = conn.instance_variable_get(:@rh_pg_serial).to_i + 1
        conn.instance_variable_set(:@rh_pg_serial, serial)
        name = "rh_#{serial}"
        conn.prepare(name, sql).clear
      end
      cache[sql] = name
      result = conn.exec_prepared(name, binds)
    end
    handle[:result] = result
    handle[:names] = result.fields
    conn.instance_variable_set(:@rh_pg_changes, result.cmd_tuples)
    # Only SELECT results can be replayed. Writes (including RETURNING)
    # invalidate cache; this is decided by PG's command tag, not SQL text.
    if result.cmd_status.start_with?("SELECT")
      if handle[:cacheable] && binds.empty? && context[:cache] && result.ntuples <= 16
        handle[:rows] = result.values
        context[:cache][key] = [handle[:rows], handle[:names]]
      end
    else
      context[:cache]&.clear
    end
  rescue PG::Error => error
    raise translate(error, sql)
  end

  def self.step?(handle)
    execute_handle(handle)
    if handle[:rows]
      row = handle[:rows][handle[:pos]]
    elsif handle[:pos] < handle[:result].ntuples
      row = handle[:result].tuple_values(handle[:pos])
    end
    handle[:pos] += 1 if row
    handle[:row] = row
    !row.nil?
  end

  def self.column_value(handle, index)
    validate_handle(handle)
    handle[:row].fetch(index)
  end

  def self.column_int(handle, index)
    column_value(handle, index).to_i
  end

  def self.column_float(handle, index)
    column_value(handle, index).to_f
  end

  def self.column_text(handle, index)
    value = column_value(handle, index)
    value.nil? ? "" : value.to_s
  end

  def self.column_bool(handle, index)
    column_value(handle, index) == true
  end

  def self.column_int_opt(handle, index)
    value = column_value(handle, index)
    value.nil? ? nil : value.to_i
  end

  def self.column_float_opt(handle, index)
    value = column_value(handle, index)
    value.nil? ? nil : value.to_f
  end

  def self.column_text_opt(handle, index)
    value = column_value(handle, index)
    value.nil? ? nil : value.to_s
  end

  def self.column_bool_opt(handle, index)
    column_value(handle, index)
  end

  def self.column_count(handle)
    execute_handle(handle)
    handle[:names].length
  end

  def self.column_name(handle, index)
    execute_handle(handle)
    handle[:names].fetch(index)
  end

  def self.finalize(handle)
    return if handle.nil? || handle[:closed]
    validate_handle(handle)
    handle[:result].clear if handle[:result].is_a?(PG::Result)
    handle[:closed] = true
    handle[:row] = nil
    nil
  end

  def self.bind_int(handle, index, value)
    validate_handle(handle)
    raise "cannot bind an executed statement" if handle[:result]
    handle[:binds][index - 1] = value
  end

  def self.bind_text(handle, index, value)
    bind_int(handle, index, value)
  end

  def self.bind_bool(handle, index, value)
    bind_int(handle, index, value.nil? ? nil : (value ? "t" : "f"))
  end

  def self.query_cache_begin
    context[:cache] = {}
  end

  def self.query_cache_end
    context[:cache] = nil
  end

  def self.capture_sql
    previous = context[:capture]
    log = []
    context[:capture] = log
    begin
      yield
    ensure
      context[:capture] = previous
    end
    log
  end

  def self.record_query(sql)
    context[:capture]&.push(sql)
  end

  def self.escape_string(value)
    string = value.to_s
    # Binary storage is bytea; text containing NUL is invalid PostgreSQL
    # text and must fail, rather than silently convert to a different type.
    return "'\\x#{string.unpack1('H*')}'::bytea" if string.encoding == Encoding::BINARY
    "'#{current_dbh.escape_string(string)}'"
  end

  def self.escape_int(value)
    value.to_i.to_s
  end

  def self.escape_bool(value)
    value ? "TRUE" : "FALSE"
  end

  def self.escape_string_opt(value)
    value.nil? ? "NULL" : escape_string(value)
  end

  def self.escape_int_opt(value)
    value.nil? ? "NULL" : escape_int(value)
  end

  def self.escape_float_opt(value)
    value.nil? ? "NULL" : value.to_f.to_s
  end

  def self.escape_bool_opt(value)
    value.nil? ? "NULL" : escape_bool(value)
  end

  def self.escape_int_list(values)
    values.empty? ? "NULL" : values.map { |value| Integer(value).to_s }.join(", ")
  end
end

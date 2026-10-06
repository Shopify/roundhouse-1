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
  @adapter_class = nil
  STMT_CACHE_CAP = 4096
  # The query PG::BasicTypeMapForResults runs on a new connection, as the
  # label of that configuration step.
  TYPE_MAP_QUERY = "SELECT t.oid, t.typname, t.typelem, t.typdelim, ti.proname AS typinput " \
    "FROM pg_type as t JOIN pg_proc as ti ON ti.oid = t.typinput"

  # The connection lifecycle of Rails 8.1.4's PostgreSQLAdapter
  # (abstract_adapter.rb, postgresql_adapter.rb), one object per pooled
  # PostgreSQL session. It is the seam a prepended adapter module wraps:
  # semian's ActiveRecordAdapter overrides `connect` (each PG.connect),
  # `raw_execute` (each statement) and `active?` (each ping), exactly the
  # three members it overrides on Rails' adapter, with Rails' call graph:
  #
  #   connect!  -> verify! -> active?, then reconnect! unless active
  #   reconnect! -> reconnect (reset, else connect) -> configure_connection,
  #                 retrying a connection error `connection_retries` times
  #                 (default 1) with `sleep 0.1 * attempt`
  #   configure_connection -> one raw_execute per session statement
  #   raw_execute -> sql.active_record event -> with_raw_connection
  #   with_raw_connection -> connect!/verify! when the session was lost and
  #                 no transaction is open, then the statement; a failure
  #                 marks the session unverified
  #
  # Not modeled: lazy BEGIN (the runtime's BEGIN is its own statement),
  # verification on pool checkout or after `verify_timeout`, query retry
  # (`allow_retry`), Rails' transaction restoration on reconnect, and the
  # dirty mark Rails' public `raw_connection` sets (which stops implicit
  # reconnects until checkout).
  module ConnectionLifecycle
    # Statements after which no transaction is open, whatever their
    # outcome (Rails pops the transaction when COMMIT/ROLLBACK fails).
    TRANSACTION_END = /\A\s*(?:COMMIT|END|ROLLBACK|ABORT)(?:\s+(?:WORK|TRANSACTION))?\s*;?\s*\z/i

    attr_reader :raw_connection
    # Set by Db before connect!; applied by configure_connection.
    attr_accessor :search_path

    def initialize(config = {})
      @config = config
      # Rails' PostgreSQLAdapter#initialize: Active Record's names mapped
      # to libpq's, then only libpq's parameters forwarded to PG.connect,
      # so a resolved database.yml entry (adapter:, pool:, semian:, ...)
      # can be passed whole.
      conn_params = config.compact
      conn_params[:user] = conn_params.delete(:username) if conn_params[:username]
      conn_params[:dbname] = conn_params.delete(:database) if conn_params[:database]
      valid_conn_param_keys = PG::Connection.conndefaults_hash.keys + [:requiressl]
      conn_params.select! { |key, _| valid_conn_param_keys.include?(key) }
      @connection_parameters = conn_params
      @raw_connection = nil
      @verified = false
      @transaction_open = false
      @search_path = nil
      @rh_pg_changes = 0
      @rh_pg_lease = nil
    end

    def connected?
      !@raw_connection.nil?
    end

    def connect!
      verify!
      self
    end

    def verify!
      reconnect! unless active?
      @verified = true
    end

    def active?
      return false unless @raw_connection
      @raw_connection.query(";").clear
      @verified = true
      true
    rescue PG::Error
      false
    end

    def reconnect!
      retries_available = connection_retries
      deadline = retry_deadline && Process.clock_gettime(Process::CLOCK_MONOTONIC) + retry_deadline
      attempt_configure_connection do
        begin
          reconnect
          @verified = true
          @transaction_open = false
          configure_connection
        rescue => original_exception
          translated_exception = Db.translate_exception_class(original_exception, nil, [])
          retry_deadline_exceeded = deadline && deadline < Process.clock_gettime(Process::CLOCK_MONOTONIC)
          if !retry_deadline_exceeded && retries_available > 0
            retries_available -= 1
            if retryable_connection_error?(translated_exception)
              backoff(connection_retries - retries_available)
              retry
            end
          end
          raise translated_exception
        end
      end
      self
    end

    def disconnect!
      @verified = false
      @transaction_open = false
      begin
        @raw_connection&.close
      rescue StandardError
        nil
      end
      @raw_connection = nil
    end

    # Rails' discard!: after fork, redirect only the child's descriptor
    # before libpq frees it, so no Terminate reaches the parent's socket.
    def discard!
      conn = @raw_connection
      @raw_connection = nil
      @verified = false
      return if conn.nil? || conn.finished?
      conn.socket_io.reopen(IO::NULL)
      conn.finish
    end

    # Lowest-level statement execution: one `sql.active_record` event
    # around a verified session, yielding the PG::Connection. Driver errors
    # leave as their Active Record translation.
    def raw_execute(sql, name = "SQL", binds = [])
      Db.instrument_sql(sql, binds, name: name) do
        with_raw_connection(sql, binds) { |conn| yield conn }
      end
    end

    # Rails' valid_raw_connection: the session, reconnecting a lost one.
    def valid_raw_connection
      with_raw_connection { |conn| conn }
    end

    # Rails' transaction_manager.restorable?: nothing is lost by replacing
    # the session when no transaction is open on it.
    def reconnect_can_restore_state?
      observe_transaction_state
      !@transaction_open
    end

    private

    def with_raw_connection(sql = nil, binds = [])
      connect! if @raw_connection.nil? && reconnect_can_restore_state?
      verify! if !@verified && reconnect_can_restore_state?
      observe_transaction_state
      begin
        yield @raw_connection
      rescue => original_exception
        translated_exception = Db.translate_exception_class(original_exception, sql, binds)
        @verified = false unless retryable_query_error?(translated_exception)
        raise translated_exception
      rescue Exception
        @verified = false
        raise
      ensure
        @transaction_open = false if sql && TRANSACTION_END.match?(sql)
      end
    end

    # libpq tracks the transaction state locally, so it is current until a
    # failed exchange marks the session bad; then the last observation is
    # what the session held.
    def observe_transaction_state
      conn = @raw_connection
      return if conn.nil? || conn.finished? || conn.status != PG::CONNECTION_OK
      status = conn.transaction_status
      @transaction_open = status == PG::PQTRANS_INTRANS || status == PG::PQTRANS_INERROR || status == PG::PQTRANS_ACTIVE
    end

    def connection_retries
      (@config[:connection_retries] || 1).to_i
    end

    def retry_deadline
      @config[:retry_deadline] ? @config[:retry_deadline].to_f : nil
    end

    def retryable_connection_error?(exception)
      return false unless defined?(ActiveRecord::ConnectionNotEstablished)
      exception.is_a?(ActiveRecord::ConnectionNotEstablished) ||
        (defined?(ActiveRecord::ConnectionFailed) && exception.is_a?(ActiveRecord::ConnectionFailed))
    end

    # Rails' PostgreSQLAdapter override over AbstractAdapter's. It reads
    # the session's transaction status first, so on a session the client
    # itself finished that read raises PG::ConnectionBad out of
    # with_raw_connection's rescue (cause: the statement's error),
    # untranslated and leaving the session verified, as in Rails.
    def retryable_query_error?(exception)
      @raw_connection&.transaction_status != ::PG::PQTRANS_INERROR &&
        retryable_query_error_class?(exception)
    end

    def retryable_query_error_class?(exception)
      (defined?(ActiveRecord::Deadlocked) && exception.is_a?(ActiveRecord::Deadlocked)) ||
        (defined?(ActiveRecord::LockWaitTimeout) && exception.is_a?(ActiveRecord::LockWaitTimeout))
    end

    def backoff(counter)
      sleep 0.1 * counter
    end

    def reconnect
      begin
        @raw_connection&.reset
      rescue PG::ConnectionBad
        @raw_connection = nil
      end
      connect unless @raw_connection
    end

    def connect
      @raw_connection = Db.new_client(@connection_parameters)
    end

    def attempt_configure_connection
      yield
    rescue Exception
      disconnect!
      raise
    end

    # Rails 8.1.4's PostgreSQLAdapter#configure_connection, from the
    # adapter's configuration (a resolved database.yml entry), in Rails'
    # order and each setting a statement of its own as its internal_execute.
    # A reset session has lost its prepared statements. Rails' own type
    # decoders are not ported: the driver keeps PG::BasicTypeMapForResults.
    def configure_connection
      @raw_connection.instance_variable_set(:@rh_pg_cache, {})
      check_version
      @raw_connection.set_client_encoding(@config[:encoding]) if @config[:encoding]
      session_statement("SET client_min_messages TO '#{@config[:min_messages] || "warning"}'")
      if (path = @search_path)
        # Db's own search path (the schema manifest's), a value.
        sql = "SELECT pg_catalog.set_config('search_path', $1, false)"
        raw_execute(sql, "SCHEMA", [path]) { |conn| conn.exec_params(sql, [path]).clear }
      elsif (path = @config[:schema_search_path] || @config[:schema_order])
        # Rails' schema_search_path=: the configured list as SQL.
        session_statement("SET search_path TO #{path}")
      end
      session_statement("SET standard_conforming_strings = on")
      session_statement("SET intervalstyle = iso_8601")
      variables = session_variables
      variables.each do |name, value|
        if value == ":default" || value == :default
          session_statement("SET SESSION #{name} TO DEFAULT")
        elsif !value.nil?
          session_statement("SET SESSION #{name} TO #{quote_session_value(value)}")
        end
      end
      raw_execute(TYPE_MAP_QUERY, "SCHEMA") do |conn|
        conn.type_map_for_results = PG::BasicTypeMapForResults.new(conn)
        nil
      end
      reconfigure_connection_timezone(variables)
    end

    # Rails' check_version.
    def check_version
      version = @raw_connection.server_version
      if version < 9_05_00
        raise "Your version of PostgreSQL (#{version}) is too old. Active Record supports PostgreSQL >= 9.5."
      end
    end

    def session_variables
      (@config[:variables] || {}).to_h.transform_keys(&:to_s)
    end

    # Rails' reconfigure_connection_timezone, which add_pg_decoders runs
    # last: unless `variables` sets the time zone, UTC under Active
    # Record's default `default_timezone` (:utc), else the server default.
    def reconfigure_connection_timezone(variables = session_variables)
      return if variables["timezone"]
      utc = !(defined?(ActiveRecord) && ActiveRecord.respond_to?(:default_timezone)) || ActiveRecord.default_timezone == :utc
      session_statement(utc ? "SET SESSION timezone TO 'UTC'" : "SET SESSION timezone TO DEFAULT")
    end

    def session_statement(sql)
      raw_execute(sql, "SCHEMA") { |conn| conn.exec(sql).clear }
    end

    # Rails' PostgreSQL quote for the value classes a configuration holds.
    def quote_session_value(value)
      case value
      when String, Symbol then "'#{@raw_connection.escape_string(value.to_s)}'"
      when true then "TRUE"
      when false then "FALSE"
      when BigDecimal then value.to_s("F")
      when Numeric then value.finite? ? value.to_s : "'#{value}'"
      else raise TypeError, "can't quote #{value.class.name}"
      end
    end
  end

  # The adapter when no Active Record adapter class is installed (the
  # ActiveRecord overlay installs ConnectionAdapters::PostgreSQLAdapter).
  class Adapter
    include ConnectionLifecycle
  end

  def self.adapter_class
    @adapter_class || Adapter
  end

  def self.adapter_class=(klass)
    @adapter_class = klass
  end

  def self.configured_databases
    @databases.keys
  end

  def self.configure(config, pool_size: nil, search_path: nil, semian: nil)
    configure_database("primary", config, pool_size: pool_size, search_path: search_path, semian: semian,
      default_pool_size: ENV.fetch("RAILS_MAX_THREADS", "3").to_i)
  end

  # Endpoint configuration is supplied separately from the schema manifest:
  # manifests contain identity and hashes, never credentials. A Hash is a
  # resolved database.yml entry (symbol or string keys, Active Record or
  # libpq names); its adapter keys (`semian`, `connection_retries`,
  # `retry_deadline`, `schema_search_path`) are the adapter's and never
  # reach libpq. The `semian:` keyword supplies that entry for a
  # connection string.
  #
  # The pool holds `pool_size:` adapters when given; otherwise the entry's
  # `max_connections` or `pool` as Rails 8.1.4's HashConfig#max_connections
  # reads them (both set and different, or `pool` with `min_connections`,
  # is Rails' ambiguity error); otherwise Rails' 5 for a Hash entry and
  # `default_pool_size` for a connection string. Every adapter connects
  # here, so the size must be positive.
  def self.configure_database(name, config, pool_size: nil, search_path: nil, semian: nil, default_pool_size: 3)
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
    unless semian.nil?
      raise ArgumentError, "semian configured twice for database #{name}" if options.key?(:semian)
      options[:semian] = semian
    end
    options[:options] = options.fetch(:options, "")
    pool_size ||= configured_max_connections(options) || (config.is_a?(Hash) ? RAILS_DEFAULT_POOL : default_pool_size)
    raise ArgumentError, "pool_size must be positive" unless pool_size.positive?
    pool = Pool.new(options.freeze, pool_size, [], [], Mutex.new, ConditionVariable.new, Process.pid)
    begin
      pool_size.times do
        adapter = adapter_class.new(options)
        adapter.search_path = search_path
        pool.connections << adapter
        adapter.connect!
        pool.free << adapter
      end
    rescue Exception
      pool.connections.each(&:disconnect!)
      raise
    end
    @databases[name.to_s] = pool
    nil
  end

  # Rails 8.1.4 HashConfig#max_connections: `max_connections`, else `pool`,
  # as an Integer; nil when neither is set (the caller's default applies).
  # A negative value (Rails' unbounded pool) cannot be held by an eagerly
  # connected pool, so it is refused.
  RAILS_DEFAULT_POOL = 5
  def self.configured_max_connections(options)
    if options[:pool] && options[:max_connections] && options[:pool].to_i != options[:max_connections].to_i
      raise "Ambiguous configuration: 'pool' (#{options[:pool].to_i}) and 'max_connections' " \
        "(#{options[:max_connections].to_i}) are set to different values. Prefer just 'max_connections'."
    end
    if options[:pool] && options[:min_connections]
      raise "Ambiguous configuration: when setting 'min_connections', use 'max_connections' instead of 'pool'."
    end
    value = options.fetch(:max_connections) { options[:pool] }
    return nil if value.nil?
    value = value.to_i
    raise ArgumentError, "unbounded pool (max_connections #{value}) is not supported" if value.negative?
    value
  end

  # Rails' PostgreSQLAdapter.new_client: PG.connect, with a failure named
  # by what the server's message mentions.
  def self.new_client(conn_params)
    PG.connect(**conn_params)
  rescue ::PG::Error => error
    raise unless defined?(ActiveRecord::ConnectionNotEstablished)
    if conn_params && conn_params[:dbname] == "postgres"
      raise ActiveRecord::ConnectionNotEstablished, error.message
    elsif conn_params && conn_params[:dbname] && error.message.include?(conn_params[:dbname]) && defined?(ActiveRecord::NoDatabaseError)
      raise ActiveRecord::NoDatabaseError.db_error(conn_params[:dbname])
    elsif conn_params && conn_params[:user] && error.message.include?(conn_params[:user]) && defined?(ActiveRecord::DatabaseConnectionError)
      raise ActiveRecord::DatabaseConnectionError.username_error(conn_params[:user])
    elsif conn_params && conn_params[:host] && error.message.include?(conn_params[:host]) && defined?(ActiveRecord::DatabaseConnectionError)
      raise ActiveRecord::DatabaseConnectionError.hostname_error(conn_params[:host])
    else
      raise ActiveRecord::ConnectionNotEstablished, error.message
    end
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

  def self.configured?
    adopt_after_fork
    @databases.key?(context[:name])
  end

  def self.adopt_after_fork
    return if @owner_pid == Process.pid
    selected = context[:name]
    configurations = @databases.map do |name, pool|
      search_path = pool.connections.first&.search_path
      # Rails' PostgreSQL discard! strategy: a plain finish would send
      # Terminate over the shared socket and disconnect the parent.
      pool.connections.each(&:discard!)
      [name, pool.config, pool.size, search_path]
    end
    @databases = {}
    @owner_pid = Process.pid
    Thread.current[:roundhouse_pg_db] = { name: selected, leases: {}, cache: nil, capture: nil }
    configurations.each do |name, config, size, path|
      configure_database(name, config, pool_size: size, search_path: path)
    end
  end

  # The adapter leased to this thread/fiber for the selected database.
  def self.current_adapter
    pool = database_pool
    context[:leases][context[:name]] ||= pool.mutex.synchronize do
      pool.cv.wait(pool.mutex) while pool.free.empty?
      adapter = pool.free.shift
      adapter.instance_variable_set(:@rh_pg_lease, Object.new)
      adapter
    end
  end

  def self.current_dbh
    adapter = current_adapter
    adapter.raw_connection || adapter.valid_raw_connection
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
    current_adapter
    begin
      yield
    ensure
      release_connection
    end
  end

  def self.release_connection
    name = context[:name]
    adapter = context[:leases].delete(name)
    return unless adapter
    pool = @databases.fetch(name)
    begin
      conn = adapter.raw_connection
      if conn && !conn.finished?
        status = conn.transaction_status
        # A lost session (UNKNOWN) holds no transaction to roll back.
        conn.exec("ROLLBACK").clear if status != PG::PQTRANS_IDLE && status != PG::PQTRANS_UNKNOWN
      end
    ensure
      adapter.instance_variable_set(:@transaction_open, false)
      context[:cache]&.clear
      adapter.instance_variable_set(:@rh_pg_lease, nil)
      pool.mutex.synchronize do
        pool.free << adapter
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
    @databases.each_value { |pool| pool.connections.each(&:disconnect!) }
    @databases.clear
    Thread.current[:roundhouse_pg_db] = nil
    nil
  end

  def self.exec(sql)
    record_query(sql)
    context[:cache]&.clear
    adapter = current_adapter
    result = adapter.raw_execute(sql) { |conn| conn.exec(sql) }
    adapter.instance_variable_set(:@rh_pg_changes, result.cmd_tuples)
    nil
  ensure
    result&.clear
  end

  # The lowerer appends a typed RETURNING key, so trigger side effects or
  # unrelated sequences cannot affect the returned insert identity.
  def self.insert_returning_int(sql)
    record_query(sql)
    context[:cache]&.clear
    adapter = current_adapter
    result = adapter.raw_execute(sql) { |conn| conn.exec(sql) }
    raise "INSERT RETURNING must produce one key" unless result.ntuples == 1 && result.nfields == 1
    adapter.instance_variable_set(:@rh_pg_changes, result.cmd_tuples)
    Integer(result.getvalue(0, 0))
  ensure
    result&.clear
  end

  def self.last_insert_rowid
    raise NotImplementedError, "PostgreSQL inserts require an explicit RETURNING key"
  end

  def self.changes
    current_adapter.instance_variable_get(:@rh_pg_changes)
  end

  # Rails' PostgreSQLAdapter#translate_exception. A driver error without
  # a SQLSTATE is about the session, not the statement: "connection is
  # closed" / "no connection to the server" and pg-internal ConnectionBad
  # are ConnectionNotEstablished, and a libpq ConnectionBad (its message
  # ends in a newline: the server may have run part of the statement) is
  # ConnectionFailed. Their message is the driver error itself.
  def self.translate(error, sql, binds = [])
    return error unless defined?(ActiveRecord::StatementInvalid)
    state = error.result&.error_field(PG::Result::PG_DIAG_SQLSTATE)
    if state.nil? && defined?(ActiveRecord::ConnectionNotEstablished)
      if error.message.match?(/connection is closed/i) || error.message.match?(/no connection to the server/i)
        return ActiveRecord::ConnectionNotEstablished.new(error)
      elsif error.is_a?(PG::ConnectionBad)
        if error.message.end_with?("\n") && defined?(ActiveRecord::ConnectionFailed)
          return ActiveRecord::ConnectionFailed.new(error, sql: sql, binds: binds.dup)
        end
        return ActiveRecord::ConnectionNotEstablished.new(error)
      end
    end
    class_name = {
      "23505" => :RecordNotUnique, "23502" => :NotNullViolation,
      "23503" => :InvalidForeignKey, "23514" => :CheckViolation,
      "40001" => :SerializationFailure, "40P01" => :Deadlocked,
      "55P03" => :LockWaitTimeout, "57014" => :QueryCanceled
    }.fetch(state, :StatementInvalid)
    klass = ActiveRecord.const_defined?(class_name) ? ActiveRecord.const_get(class_name) : ActiveRecord::StatementInvalid
    klass.new("#{error.class}: #{error.message}", sql: sql, binds: binds.dup)
  end

  # Rails' translate_exception_class: only driver errors (those carrying a
  # result) are translated; the translation keeps the driver's backtrace.
  def self.translate_exception_class(error, sql, binds)
    return error unless error.respond_to?(:result)
    translated = translate(error, sql, binds)
    translated.set_backtrace(error.backtrace) if !translated.equal?(error) && error.backtrace
    translated
  end

  # Instrument the actual execution, after binding and before returning to the
  # caller (Rails' AbstractAdapter#log). The statement runs in
  # with_raw_connection, which translates driver errors inside the event, so
  # subscribers receive the same ActiveRecord exception that the caller
  # rescues; like Rails' log, nothing is translated here, so a driver error
  # raised by with_raw_connection's own recovery reaches the caller as is.
  def self.instrument_sql(sql, binds = [], name: "SQL", &operation)
    if defined?(ActiveSupport::Notifications::Event)
      active_record_instrumenter.instrument("sql.active_record", sql: sql, name: name, binds: binds.dup, &operation)
    else
      operation.call
    end
  end

  # Active Record's AbstractAdapter#instrumenter: cached per thread at the
  # first instrumented statement, so it keeps the notifier current at that
  # moment (LogSubscriber::TestHelper's swapped notifier does not receive
  # SQL events from a thread that already ran SQL).
  def self.active_record_instrumenter
    Thread.current.thread_variable_get(:roundhouse_active_record_instrumenter) ||
      Thread.current.thread_variable_set(:roundhouse_active_record_instrumenter,
        ActiveSupport::Notifications.instrumenter)
  end

  def self.unique_violation?(message)
    message.to_s.include?("PG::UniqueViolation")
  end

  # Each handle owns its result, even for nested identical SQL. Server-side
  # prepared statements belong to the connection and have a bounded cache.
  def self.prepare(sql, cacheable: false)
    adapter = current_adapter
    { adapter: adapter, sql: sql, binds: [], result: nil, row: nil, pos: 0, closed: false,
      owner: [Thread.current, Fiber.current], database: context[:name],
      lease: adapter.instance_variable_get(:@rh_pg_lease), cacheable: cacheable }
  end

  def self.validate_handle(handle)
    raise "finalized PostgreSQL statement" if handle[:closed]
    raise "PostgreSQL statement belongs to another thread or fiber" unless handle[:owner] == [Thread.current, Fiber.current]
    raise "PostgreSQL statement belongs to another database lease" unless handle[:database] == context[:name] && handle[:adapter].equal?(current_adapter)
    raise "PostgreSQL statement outlived its lease" unless handle[:lease].equal?(handle[:adapter].instance_variable_get(:@rh_pg_lease))
  end

  def self.execute_handle(handle)
    validate_handle(handle)
    return if handle[:result]
    sql, adapter, binds = handle.values_at(:sql, :adapter, :binds)
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
    result = adapter.raw_execute(sql, "SQL", binds) do |conn|
      if binds.empty?
        conn.exec(sql)
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
        conn.exec_prepared(name, binds)
      end
    end
    handle[:result] = result
    handle[:names] = result.fields
    adapter.instance_variable_set(:@rh_pg_changes, result.cmd_tuples)
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

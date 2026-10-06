# Primitive Db surface — the layer that per-model adapter code sits on
# top of. The contract is database-agnostic; this file is the SQLite-via-
# the-cruby-`sqlite3`-gem implementation. Other backends (sqlite via
# spinel FFI, postgres via libpq, etc.) implement the same `module Db`
# in sibling files; a future dispatcher picks one at require time. See
# project_level_3_adapter_emit.md.
#
# API (the contract every Db shim must satisfy):
#
#   Db.configure(path)         — open a database (":memory:" for tests)
#   Db.close                   — close the database
#   Db.exec(sql)               — run DDL / INSERT / UPDATE / DELETE
#   Db.prepare(sql)            — prepare a SELECT, returns stmt handle
#   Db.step?(stmt)             — advance, returns true if a row arrived
#   Db.column_int(stmt, i)     — read int column at zero-based index
#   Db.column_text(stmt, i)    — read text column at zero-based index
#   Db.column_count(stmt)      — number of columns in the prepared row
#   Db.column_name(stmt, i)    — name of column at zero-based index
#   Db.finalize(stmt)          — release the prepared stmt
#   Db.bind_int/bind_text/bind_bool(stmt, i, v) — bind `?` param i (1-based)
#   Db.last_insert_rowid       — id of the last INSERTed row
#   Db.changes                 — affected-row count of the last statement
#
# Stmt handles are opaque — under spinel FFI they're real `:ptr` values;
# under this CRuby shim each is a per-call Hash carrying the statement and
# its most recently stepped row (so column_int / column_text can pick
# fields by index, mirroring the FFI column accessors). Callers only ever
# hand it back to `Db.*`. It used to be an Integer indexing a process-wide
# table with a shared counter, which only CRuby's GVL made safe: Puma's
# worker threads run in parallel under JRuby and TruffleRuby, two of them
# drew the same id, and one request read the other's (finalized, nil)
# row. Like db_jruby.rb, there is now no shared mutable handle state.
#
# Per-database SQL dialect differences (placeholder syntax, RETURNING vs
# last_insert_rowid, etc.) live inside each shim or in a separate dialect
# helper consulted by the lowerer at SQL-composition time.
#
# The Db primitive surface backs the lowerer-emitted Level-3 per-model
# `_adapter_*` methods. This file is the CRuby (gem-backed) variant;
# `db.rb` in the same directory is the FFI variant the Spinel-AOT
# target compiles against.

require "sqlite3"

module Db
  @pool    = nil
  # WHAT PROCESS THIS POOL BELONGS TO, and what it would take to build
  # another one. Both exist for `fork`: see `adopt_after_fork`.
  @owner_pid = nil
  @path      = nil
  @pool_size = 0
  @mutex   = nil
  @cv      = nil
  # THE WRITE PERMIT: one writer at a time per process, handed out here
  # instead of by SQLite's busy handler. See `acquire_permit`.
  @permit_lock  = Mutex.new
  @permit_cv    = ConditionVariable.new
  @permit_owner = nil
  WRITE_PERMIT_TIMEOUT = 5.0
  @write_permit_timeout = WRITE_PERMIT_TIMEOUT
  # Background WAL checkpointing: asked for by the server boot
  # (`checkpoint_in_background!`), started per process on the first
  # lease. See `start_checkpointer`.
  @checkpoint_wanted = false
  @checkpointer_pid  = nil
  # Per-connection prepared-statement cache bound (roundhouse#12). The
  # cache is LRU (hits re-insert; at cap the oldest entry is closed and
  # evicted), so a working set larger than the cap degrades gracefully
  # instead of pinning the first N statements forever and re-parsing
  # everything else — profiling the lobsters bench showed the old
  # first-come-stays policy spending 13% of wall time in
  # sqlite3_prepare/close because inlined id literals key per-id. The
  # cap covers the lobsters sequence's per-iteration distinct-SQL
  # working set with room; a prepared stmt is ~KBs, so worst case is a
  # few MB per connection.
  STMT_CACHE_CAP = 4096
  # Query-log capture (issue #27). `nil` ⇒ not capturing; an Array ⇒
  # accumulate the SQL each prepare/exec issues. The funnel hook
  # `record_query` is near-free (one nil check) when not capturing, so
  # this stays out of the way on the production path.
  @query_log = nil

  # Pool size defaults to the Puma thread count (RAILS_MAX_THREADS) so
  # every concurrently-serving thread can hold its own handle without
  # contending. Override explicitly for tests.
  def self.configure(path, pool_size: ENV.fetch("RAILS_MAX_THREADS", "3").to_i)
    @path      = path
    @pool_size = pool_size
    @mutex     = Mutex.new
    @cv        = ConditionVariable.new
    @pool      = open_pool
    @owner_pid = Process.pid
  end

  # The pool construction, in one place because two callers need it: the
  # boot-time `configure` above and `adopt_after_fork` below.
  def self.open_pool
    path = @path
    # A `file:` URI (the test harness's shared-cache `file::memory:?
    # cache=shared`) needs the gem told to read it as one; a plain path
    # takes the gem's defaults, which are the same flags minus URI.
    flags = SQLite3::Constants::Open::READWRITE | SQLite3::Constants::Open::CREATE
    flags |= SQLite3::Constants::Open::URI if path.start_with?("file:")
    ActiveRecord::ConnectionAdapters::ConnectionPool.new(@pool_size) do
      db = SQLite3::Database.new(path, flags: flags)
      db.results_as_hash = false
      # PINNED, not inherited. This lane reads `synchronous = NORMAL`
      # today without asking for it, because the sqlite3 gem's bundled
      # SQLite defaults WAL that way — while the binary's own SQLite
      # defaults to FULL, which cost the spinel lane 5.5s on a
      # 1,000-socket connect storm (one fsync per presence write; see
      # runtime/spinel/db.rb's PRAGMAS). Three lanes agreeing by
      # compile-time accident is not agreement, so each states it.
      db.execute("PRAGMA journal_mode=WAL")
      db.execute("PRAGMA synchronous=NORMAL")
      # The gem's default is 0: a second writer fails at once with
      # SQLITE_BUSY. Rails' database.yml says `timeout: 5000`, and so do
      # the binary's PRAGMAS — the harness's file database (see
      # test/test_helper.rb) relies on writers waiting.
      #
      # The GVL-RELEASING handler, as Rails 8's adapter uses, not
      # `busy_timeout`: SQLite's own handler sleeps in C holding the GVL,
      # so a waiting writer stalls every other thread in the process —
      # including the one holding the lock it waits for. Writers inside
      # one process queue on the write permit (`exec`) and never reach
      # this; it is for a writer in ANOTHER process (a clustered Puma
      # sibling, a console, a migration).
      db.busy_handler_timeout = 5000
      # The app's SQL functions (`create_function` / `create_aggregate`
      # in an initializer), per connection as Rails' adapter registers
      # them. Defined only when the app has some (runtime/sql_functions.rb
      # is generated for it).
      SqlFunctions.install(db) if defined?(SqlFunctions)
      db
    end
  end

  # A FORKED CHILD DOES NOT INHERIT A USABLE POOL, and the failure is
  # silent in the worst way.
  #
  # A clustered Puma boots the app in the master and then forks a worker
  # per `WEB_CONCURRENCY`, so every handle this pool opened belongs to
  # the parent. The sqlite3 gem notices — "Writable sqlite database
  # connection(s) were inherited from a forked process ... being closed
  # to prevent possible data corruption" — and discards them. It does
  # not tell the pool, which goes on handing the discarded handles out,
  # and a discarded handle does not raise: it answers every query
  # against an EMPTY SCHEMA. campfire's sign-in came back
  # `SQLite3::SQLException: no such table: bans` from a database whose
  # `bans` table is right there on disk.
  #
  # One Puma worker never forks, which is the only reason this survived
  # every benchmark run this tree has ever been in.
  #
  # The check is a pid comparison once per REQUEST (`with_connection`),
  # not once per query — `current_dbh` is the hot path and pays nothing
  # while a lease is held.
  def self.adopt_after_fork
    return if @pool.nil? || @owner_pid == Process.pid
    @mutex.synchronize do
      # Re-checked under the lock: every worker thread in a fresh child
      # reaches this together on the first request.
      if @owner_pid != Process.pid
        # The parent's handles are simply dropped. They are already
        # discarded by the gem's fork safety, and closing a descriptor
        # this process shares with its parent is not ours to do.
        @pool      = open_pool
        @owner_pid = Process.pid
      end
    end
  end

  # The SQLite3::Database this thread should read/write through. Set by
  # `with_connection` (request scope) when wired; falls back to the
  # pool's first free handle for single-thread test/dev modes.
  # `Fiber[:k]` is fiber-storage — under Puma's thread-per-request it is
  # effectively thread-local (each worker thread's root fiber).
  def self.current_dbh
    h = Fiber[:db_handle]
    return h if !h.nil?
    # Only the UNLEASED path pays the fork check — boot, tests, and
    # single-threaded dev. Under a lease the line above has already
    # returned, so the per-query cost of all this is zero.
    adopt_after_fork
    @pool.free[0]
  end

  # Request-scoped connection lease. Checks out a handle, binds it to
  # this thread's fiber-storage so `current_dbh` resolves to it for the
  # block's duration, and returns it on completion (even on raise).
  #
  # Thread-safe for the CRuby/Puma target where N worker threads share
  # one pool: the pool's free list (not itself thread-safe) is mutated
  # only under @mutex, and a thread parks on @cv when the pool is
  # momentarily exhausted (size < live requests) rather than raising.
  # With pool_size == thread count, the wait loop never trips.
  # Whether this thread is inside a `with_connection` lease. What
  # `Rails::Executor#wrap` asks before taking one: Rails' executor is
  # re-entrant, and a second lease on a thread that holds one would
  # rebind the connection and, on release, unbind the outer lease's.
  def self.in_lease?
    !Fiber[:db_handle].nil?
  end

  def self.with_connection
    h = nil
    adopt_after_fork
    @mutex.synchronize do
      while @pool.available_count == 0
        @cv.wait(@mutex)
      end
      h = @pool.checkout
    end
    Fiber[:db_handle] = h
    prepare_for_checkpointer(h) if @checkpoint_wanted
    begin
      yield
    ensure
      # A snapshot bracket left open: the state is the connection's,
      # and the next lease must not inherit it. Closed FIRST, so the
      # check below sees only a transaction the request itself began.
      if h.instance_variable_get(:@rh_snapshot_depth).to_i > 0
        h.instance_variable_set(:@rh_snapshot_depth, 1)
        read_snapshot_end
      end
      # A transaction the request opened and never closed — only a
      # non-StandardError (an Interrupt, a Timeout) gets past
      # `transaction`'s own ROLLBACK — would otherwise hand the next
      # request a connection mid-transaction and keep the write permit,
      # stopping every writer in the process.
      release_abandoned_write(h)
      Fiber[:db_handle] = nil
      @mutex.synchronize do
        @pool.checkin(h)
        @cv.signal
      end
    end
  end

  def self.close
    return if @pool.nil?
    i = 0
    while i < @pool.free.length
      conn = @pool.free[i]
      # Finalize cached statements before closing the connection (older
      # sqlite3-gem builds refuse to close with unfinalized statements).
      cache = conn.instance_variable_get(:@rh_stmt_cache)
      cache.each_value { |st| st.close } if cache
      conn.close
      i += 1
    end
    @pool = nil
    @owner_pid = nil
  end

  def self.exec(sql)
    record_query(sql)
    # Any exec is (per the Db contract) DDL or a write — Rails
    # invalidates the whole query cache on write; so do we.
    qcache = Fiber[:rh_qcache]
    qcache.clear unless qcache.nil?
    conn = current_dbh
    # A write can't run inside the request's read snapshot: if another
    # connection committed since the snapshot began, SQLite refuses the
    # upgrade at once with SQLITE_BUSY (no busy handler is consulted).
    # End the snapshot first; the next read opens a fresh one that sees
    # this write.
    end_snapshot(conn)
    if permit_owned?
      # Inside this fiber's own transaction: the permit is already held.
      begin
        run_exec(conn, sql)
      ensure
        release_permit if sql == "ROLLBACK" || (sql == "COMMIT" && !conn.transaction_active?)
      end
    elsif conn.transaction_active?
      # Inside a transaction that began WITHOUT the permit (its wait
      # timed out — see `acquire_permit`): SQLite's lock is already
      # held, so there is nothing to queue for.
      run_exec(conn, sql)
    elsif sql == "BEGIN"
      # IMMEDIATE, as Rails 8's SQLite adapter begins: the write lock is
      # taken at BEGIN, not at the first write, so a transaction never
      # fails part way through upgrading a stale read.
      got = acquire_permit
      begin
        run_exec(conn, "BEGIN IMMEDIATE")
      rescue Exception
        release_permit if got
        raise
      end
    else
      got = acquire_permit
      begin
        run_exec(conn, sql)
      ensure
        release_permit if got
      end
    end
  end

  def self.run_exec(conn, sql)
    conn.execute(sql)
  rescue SQLite3::Exception => e
    raise translate(e, sql)
  end

  # ── The write permit ──
  #
  # SQLite allows one writer at a time. Left to itself, a second writer
  # gets SQLITE_BUSY and the busy handler retries on a timer, so writers
  # race: whoever retries at the right moment wins, and an unlucky one
  # loses again and again (the post-message tail). `exec` instead takes
  # the permit — for one statement in autocommit, or from BEGIN to
  # COMMIT/ROLLBACK — so writers in this process queue and go in turn.
  # The SQL still runs on the caller's own pooled connection; only the
  # permission is shared. A writer in another process still meets
  # SQLite's lock, and waits in `busy_handler_timeout`.
  #
  # The owner is a FIBER: BEGIN and COMMIT run on the same fiber, and a
  # different request on the same thread (Falcon) is a different fiber.
  #
  # THE WAIT IS BOUNDED. A transaction that starts a thread which writes
  # and then joins it would otherwise wait on itself forever. Before the
  # permit existed that case waited out SQLite's busy timeout and raised
  # SQLITE_BUSY; a hang is worse than that error. So after
  # `@write_permit_timeout` seconds a writer stops queueing and goes
  # straight to SQLite, which is exactly the old behaviour: it waits in
  # the busy handler and, if the lock never frees, raises BUSY.
  # Tests shorten the bound to exercise the timeout path quickly.
  def self.write_permit_timeout=(seconds)
    @write_permit_timeout = seconds
  end

  def self.acquire_permit
    fiber = Fiber.current
    @permit_lock.synchronize do
      deadline = Process.clock_gettime(Process::CLOCK_MONOTONIC) + @write_permit_timeout
      # A dead owner (a killed thread) never releases; a Mutex would
      # have been freed with it, so this permit treats it as free too.
      until @permit_owner.nil? || !@permit_owner.alive?
        remaining = deadline - Process.clock_gettime(Process::CLOCK_MONOTONIC)
        return false if remaining <= 0
        @permit_cv.wait(@permit_lock, remaining)
      end
      @permit_owner = fiber
    end
    true
  end

  def self.release_permit
    @permit_lock.synchronize do
      @permit_owner = nil
      @permit_cv.signal
    end
  end

  # Read without the lock: only the owning fiber ever sets the owner to
  # itself or clears it, so the answer for the CURRENT fiber is exact.
  def self.permit_owned?
    @permit_owner.equal?(Fiber.current)
  end

  def self.release_abandoned_write(conn)
    owned = permit_owned?
    return unless owned || conn.transaction_active?
    begin
      conn.execute("ROLLBACK") if conn.transaction_active?
    ensure
      release_permit if owned
    end
  end

  # ── The request read snapshot ──
  #
  # Outside a transaction every SELECT is its own transaction, and each
  # one sets up a WAL snapshot (shared lock on the WAL index, header
  # check). Inside an open transaction the same point query skips that:
  # measured 0.93 -> 0.36 us through this gem on an M-series Mac, and
  # the once-campfire Ruby port measured 4.5 -> 0.2 us on Linux. It also
  # gives the request one consistent view of the database, which
  # autocommit does not.
  #
  # The dispatcher brackets a GET/HEAD with `read_snapshot_begin` /
  # `read_snapshot_end`. Nothing happens until the first `prepare`
  # opens a deferred BEGIN; the first `exec` (any write) ends it, and the
  # read after that opens a new one. The app is never asked to promise
  # its GET handlers don't write. A request in a transaction of its own
  # is left alone: `prepare` opens a snapshot only when the connection
  # is in autocommit.
  #
  # States: nil (off), :wanted (on, not open), :open (BEGIN issued by
  # us). Brackets nest — a test that dispatches from inside a lease, a
  # job drained inside a request — and only the outermost end closes
  # the snapshot.
  #
  # The state lives ON THE CONNECTION, not in fiber storage. A snapshot
  # is a property of one SQLite connection, and a connection is leased
  # to one holder at a time. Fiber storage is copied into every Thread
  # created inside the request, so a thread spawned during a GET would
  # inherit `:open` and COMMIT a transaction its own connection never
  # began.
  def self.read_snapshot_begin
    conn  = current_dbh
    depth = conn.instance_variable_get(:@rh_snapshot_depth).to_i
    conn.instance_variable_set(:@rh_snapshot, :wanted) if depth == 0
    conn.instance_variable_set(:@rh_snapshot_depth, depth + 1)
  end

  def self.read_snapshot_end
    conn  = current_dbh
    depth = conn.instance_variable_get(:@rh_snapshot_depth).to_i - 1
    conn.instance_variable_set(:@rh_snapshot_depth, depth)
    return if depth > 0
    state = conn.instance_variable_get(:@rh_snapshot)
    conn.instance_variable_set(:@rh_snapshot, nil)
    conn.execute("COMMIT") if state == :open
  end

  def self.begin_snapshot(conn)
    return unless conn.instance_variable_get(:@rh_snapshot) == :wanted
    return if conn.transaction_active?
    conn.execute("BEGIN")
    conn.instance_variable_set(:@rh_snapshot, :open)
  end

  def self.end_snapshot(conn)
    return unless conn.instance_variable_get(:@rh_snapshot) == :open
    conn.execute("COMMIT")
    conn.instance_variable_set(:@rh_snapshot, :wanted)
  end

  # ── Checkpoints off the request path ──
  #
  # In WAL mode a write appends to the -wal file, and a checkpoint copies
  # it back into the database. SQLite's default runs one inside whichever
  # COMMIT pushes the log past 1,000 pages: that request pays for the
  # copy and an fsync (10-20 ms in the once-campfire port's measurements,
  # on about every 30th post — the p99). Under steady writes the log can
  # also never empty, because a checkpoint can only copy frames no reader
  # still needs.
  #
  # The server boot calls `checkpoint_in_background!`. Each serving
  # process then turns automatic checkpoints off on its pooled
  # connections and runs one thread with its own connection: PASSIVE
  # (copy what it can, wait on nobody) every CHECKPOINT_INTERVAL, and
  # RESTART once the log passes CHECKPOINT_RESTART_FRAMES, which makes
  # the next writer start the log from the beginning so the file stops
  # growing. RESTART holds the write permit, so in-process writers queue
  # behind it rather than meeting SQLITE_BUSY.
  #
  # Started from the first lease, not from boot, because a clustered
  # Puma boots the app in a master that forks and never serves: a thread
  # does not survive `fork`, and the master has no writes to checkpoint.
  # Tests, scripts and the console never ask, and keep SQLite's default.
  CHECKPOINT_INTERVAL = 0.25
  CHECKPOINT_RESTART_FRAMES = 8192 # ~32 MB of 4 KB pages

  def self.checkpoint_in_background!
    @checkpoint_wanted = true
  end

  def self.prepare_for_checkpointer(conn)
    start_checkpointer if @checkpointer_pid != Process.pid
    return if @checkpointer_pid != Process.pid
    return if conn.instance_variable_get(:@rh_manual_checkpoint)
    conn.execute("PRAGMA wal_autocheckpoint=0")
    conn.instance_variable_set(:@rh_manual_checkpoint, true)
  end

  def self.start_checkpointer
    @mutex.synchronize do
      return if @checkpointer_pid == Process.pid
      # Only a database file has a WAL to checkpoint.
      path = @path
      return if path.nil? || path == ":memory:" || path.start_with?("file:")
      @checkpointer_pid = Process.pid
      Thread.new { checkpoint_loop(path) }
    end
  end

  def self.checkpoint_loop(path)
    conn = SQLite3::Database.new(path)
    conn.busy_handler_timeout = 100
    loop do
      sleep CHECKPOINT_INTERVAL
      begin
        row = conn.execute("PRAGMA wal_checkpoint(PASSIVE)")[0]
        log_frames = row.nil? ? 0 : row[1].to_i
        if log_frames >= CHECKPOINT_RESTART_FRAMES
          if acquire_permit
            begin
              conn.execute("PRAGMA wal_checkpoint(RESTART)")
            ensure
              release_permit
            end
          end
        end
      rescue StandardError
        # A busy or failed checkpoint is retried on the next tick; the
        # log only grows meanwhile.
      end
    end
  end

  # Rails' sqlite3 `translate_exception` (activerecord 5813559f,
  # connection_adapters/sqlite3_adapter.rb): a driver error becomes the
  # Active Record class its SQLite message names, else StatementInvalid,
  # each named "<driver class>: <message>" and carrying the statement.
  # Raised from inside the driver's rescue, so the driver's exception is
  # its `cause`, as Rails' is.
  #
  # Rails answers a closed database with ConnectionNotEstablished, which
  # is not a StatementInvalid; that error passes through untranslated
  # (the ruby-family `ConnectionNotEstablished` class is not raised
  # here), so no `rescue StatementInvalid` catches it. `InvalidForeignKey`
  # is mapped as Rails maps it, but no `Db` turns on `PRAGMA
  # foreign_keys`, so SQLite never reports the violation. NOT modeled:
  # StatementTimeout for a `SQLite3::BusyException` (a StatementInvalid
  # here, as its Rails parent is), and translation of any non-driver
  # error.
  def self.translate(e, sql)
    m = e.message
    return e if m.match?(/called on a closed database/i)
    message = "#{e.class.name}: #{m}"
    klass =
      if m.match?(/(column(s)? .* (is|are) not unique|UNIQUE constraint failed: .*)/i)
        ActiveRecord::RecordNotUnique
      elsif m.match?(/(.* may not be NULL|NOT NULL constraint failed: .*)/i)
        ActiveRecord::NotNullViolation
      elsif m.match?(/FOREIGN KEY constraint failed/i)
        ActiveRecord::InvalidForeignKey
      elsif m.match?(/CHECK constraint failed: .*/i)
        ActiveRecord::CheckViolation
      else
        ActiveRecord::StatementInvalid
      end
    klass.new(message, sql: sql, binds: [])
  end

  # A UNIQUE-index violation is `ActiveRecord::RecordNotUnique`, not
  # whatever this driver raises. Rails' contract is what apps write
  # against — campfire's sign-up rescues it to turn a lost race into a
  # redirect to the login screen, and its first-run screen does the same
  # for two people opening a brand-new install at once. Without the
  # mapping the rescue never matched and the raw driver error reached
  # the dispatcher as a 500.
  #
  # THE TEST IS SQLITE'S OWN MESSAGE, not the driver's exception class,
  # and that is deliberate: "UNIQUE constraint failed: users.
  # email_address" comes out of the engine, so the same string appears
  # in the cruby gem's ConstraintException, in the JDBC SQLException and
  # in `sqlite3_errmsg` under spinel. One rule, three drivers, no
  # per-driver class table to keep in step. (The strict targets carry
  # their own `Db` and their own mapping; this is the ruby-family half.)
  def self.unique_violation?(message)
    message.to_s.include?("UNIQUE constraint failed")
  end

  # Prepared-statement cache (roundhouse#12). A SQLite3::Statement is bound
  # to the connection it was prepared on, so the cache lives ON the
  # connection object — and since `with_connection` leases a connection to
  # exactly one thread at a time, the per-connection cache needs no extra
  # lock. A cache hit rewinds the stmt (`reset!`) instead of re-parsing the
  # SQL; `finalize` resets rather than closes, so the stmt stays cached
  # (real `close` runs at pool shutdown). Key is the composed SQL: inlined
  # literals mean id-bearing queries key per-id (fine for the bench;
  # STMT_CACHE_CAP bounds growth, beyond which statements are transient and
  # closed on finalize). With placeholder binding on (roundhouse#12) the
  # key is instead the static shape (`WHERE id = ?`), so id-varying
  # queries share one cached statement.
  # ── per-request SQL query cache (Rails AR query-cache semantics) ──
  # Identical SELECTs within one request replay the first result set;
  # any `exec` (writes ride exec per the Db contract) invalidates.
  # Fiber-local so Puma's thread-per-request can't cross-pollute.
  # Entries capture rows AS CONSUMED plus an eof flag: a point-lookup
  # reader that steps once caches one row + eof=false, and a later
  # consumer wanting more rows promotes to a real re-executed
  # statement (rare). Enabled per-request by the dispatch; nil ⇒ off
  # (tests, scripts) with a single fiber-storage read of overhead.
  QC_CAPTURE_ROWS = 16

  def self.query_cache_begin
    Fiber[:rh_qcache] = {}
  end

  def self.query_cache_end
    Fiber[:rh_qcache] = nil
  end

  def self.prepare(sql)
    # A `?`-bearing SQL string is a placeholder query (roundhouse#12):
    # its result depends on the runtime binds set AFTER prepare, which
    # aren't in the SQL key — so it must NOT participate in the
    # result-replay query cache (replaying would serve one bind value's
    # rows for another). The prepared-statement cache below still keys on
    # the shared shape, which is the whole point. (Heuristic: a literal
    # value containing `?` would also skip qcache — a safe miss, never a
    # wrong result.)
    parameterized = sql.include?("?")
    qcache = Fiber[:rh_qcache]
    if !qcache.nil? && !parameterized && (hit = qcache[sql])
      # A replay is not a round trip, and `capture_sql` does not count
      # it — the same rule as Rails' SQLCounter, which skips CACHE
      # events, and as the spinel lane's `Db.prepare`.
      return { stmt: nil, row: nil, cached: false, replay: hit, pos: 0, sql: sql }
    end
    record_query(sql)
    conn  = current_dbh
    begin_snapshot(conn)
    cache = conn.instance_variable_get(:@rh_stmt_cache)
    if cache.nil?
      cache = {}
      conn.instance_variable_set(:@rh_stmt_cache, cache)
    end
    # Handles open on this connection whose stmt is the cached one —
    # what the LRU eviction below must not close. Keyed by identity: a
    # handle mutates (`:row`) while open, so its content hash cannot be
    # the key. Like the stmt cache, only the leasing thread touches it.
    open = conn.instance_variable_get(:@rh_open)
    if open.nil?
      open = {}.compare_by_identity
      conn.instance_variable_set(:@rh_open, open)
    end
    stmt   = cache[sql]
    cached = true
    if stmt.nil?
      begin
        stmt = conn.prepare(sql)
      rescue SQLite3::Exception => e
        raise translate(e, sql)
      end
      if cache.size >= STMT_CACHE_CAP
        # Evict least-recently-used (Ruby Hash is insertion-ordered and
        # hits below re-insert, so the earliest key is the LRU). Skip
        # any statement still held by an open handle — closing it
        # under a live cursor would break nested prepare patterns. If
        # everything is somehow in use, insert past the cap (soft
        # bound) rather than close a live statement.
        live = nil
        cache.each_key do |k|
          candidate = cache[k]
          next if open.each_key.any? { |e| e[:stmt].equal?(candidate) }
          live = k
          break
        end
        unless live.nil?
          cache.delete(live).close
        end
      end
      cache[sql] = stmt
    else
      # Reused: rewind the cursor before re-stepping (robust even if a
      # prior request raised before its finalize). Re-insert to record
      # recency (LRU discipline).
      cache.delete(sql)
      cache[sql] = stmt
      stmt.reset!
    end
    capture = nil
    qcache = Fiber[:rh_qcache]
    unless qcache.nil? || parameterized
      capture = { rows: [], names: stmt.columns, eof: false, sql: sql }
    end
    handle = { stmt: stmt, row: nil, cached: cached, capture: capture, open: open, sql: sql }
    open[handle] = true
    handle
  end

  def self.step?(handle)
    entry = handle
    if (hit = entry[:replay])
      if entry[:pos] < hit[:rows].length
        entry[:row] = hit[:rows][entry[:pos]]
        entry[:pos] += 1
        return true
      end
      return false if hit[:eof]
      # Cached prefix exhausted without eof (original consumer stopped
      # early) — promote to a real transient statement, fast-forwarded
      # past the rows already replayed.
      stmt = current_dbh.prepare(entry[:sql])
      entry[:pos].times { stmt.step }
      entry[:stmt] = stmt
      entry[:replay] = nil
      entry[:promoted] = true
      row = stmt.step
      entry[:row] = row
      return !row.nil?
    end
    row = begin
      entry[:stmt].step
    rescue SQLite3::Exception => e
      raise translate(e, entry[:sql])
    end
    entry[:row] = row
    if (c = entry[:capture])
      if row.nil?
        c[:eof] = true
      elsif c[:rows].length >= QC_CAPTURE_ROWS
        # Bounded like the spinel shim's (db.rb, `QC_CAPTURE_ROWS`): the
        # cache is for the point lookups a page repeats, and a result
        # past the bound is not kept — the same rows come back from a
        # re-execution rather than a replay.
        entry[:capture] = nil
      else
        c[:rows] << row
      end
    end
    !row.nil?
  end

  def self.column_int(handle, i)
    handle[:row][i].to_i
  end

  def self.column_float(handle, i)
    handle[:row][i].to_f
  end

  def self.column_text(handle, i)
    v = handle[:row][i]
    v.nil? ? "" : v.to_s
  end

  # Nullable-column reads. A column the schema declares nullable holds
  # NULL until something sets it, and NULL is not the type's zero:
  # `column_text` collapsing it to "" makes a nullable UNIQUE column
  # collide row-to-row, and `column_int`'s 0 makes `where(fk: nil)`
  # match nothing. These are the reads the lowerer emits for those
  # columns; the non-`_opt` readers stay exactly as they were for
  # NOT NULL columns, which is most of them.
  def self.column_int_opt(handle, i)
    v = handle[:row][i]
    v.nil? ? nil : v.to_i
  end

  def self.column_float_opt(handle, i)
    v = handle[:row][i]
    v.nil? ? nil : v.to_f
  end

  def self.column_text_opt(handle, i)
    v = handle[:row][i]
    v.nil? ? nil : v.to_s
  end

  def self.column_bool_opt(handle, i)
    v = handle[:row][i]
    v.nil? ? nil : v.to_i != 0
  end

  # Raw typed column read: the value exactly as the sqlite3 gem
  # returns it — Integer for INTEGER affinity, Float for REAL, String
  # for TEXT, and crucially nil for NULL (column_text collapses NULL
  # to "", which breaks Rails semantics like `group_by(&:fk)[nil]`
  # and integer-column truthiness). Whole-row hydration
  # (SqliteAdapter.select_rows) reads through this so model attributes
  # carry real types, matching what ActiveRecord hands the app.
  def self.column_value(handle, i)
    handle[:row][i]
  end

  def self.column_count(handle)
    e = handle
    return e[:replay][:names].length if e[:replay]
    e[:stmt].columns.length
  end

  def self.column_name(handle, i)
    e = handle
    return e[:replay][:names][i] if e[:replay]
    e[:stmt].columns[i]
  end

  # Release the per-call handle. A cached stmt is reset! (rewound + read
  # lock dropped) and kept for reuse; a transient (over-cap or
  # replay-promoted) stmt is closed; a pure replay handle held no
  # statement at all. A capture is published to the request's query
  # cache on release — even a partial one (eof=false): the next
  # identical SELECT replays the consumed prefix and promotes past it
  # only if it wants more.
  def self.finalize(entry)
    return if entry.nil?
    return if entry[:replay] # replay handle — nothing to release
    o = entry[:open]
    o.delete(entry) unless o.nil?
    if (c = entry[:capture])
      qcache = Fiber[:rh_qcache]
      qcache[c[:sql]] = c if !qcache.nil? && !qcache.key?(c[:sql])
    end
    if entry[:cached]
      entry[:stmt].reset!
    else
      entry[:stmt].close
    end
  end

  # Placeholder binding (roundhouse#12). Bind one `?` param (1-based) on
  # a prepared stmt before the first `step?`, via the gem's
  # `Statement#bind_param`. The emitted `_adapter_*` bodies always
  # re-bind every param before stepping, so a cached stmt's prior binds
  # are overwritten (SQLite `reset` keeps bindings; our re-bind replaces
  # them positionally — same param count for the same SQL shape). The
  # nil guard covers the replay-handle case, which `?` queries never take
  # (prepare skips replay for parameterized SQL).
  def self.bind_int(handle, idx, value)
    st = handle[:stmt]
    st.bind_param(idx, value) unless st.nil?
  end

  def self.bind_text(handle, idx, value)
    st = handle[:stmt]
    st.bind_param(idx, value) unless st.nil?
  end

  # SQLite has no native bool — bind 0/1, matching escape_bool's inline
  # form and the INTEGER affinity `t.boolean` columns get.
  def self.bind_bool(handle, idx, value)
    st = handle[:stmt]
    st.bind_param(idx, value ? 1 : 0) unless st.nil?
  end

  def self.last_insert_rowid
    current_dbh.last_insert_row_id
  end

  def self.changes
    current_dbh.changes
  end

  # Query-log capture — the test-side analog of Rails'
  # `ActiveSupport::Notifications.subscribed(counter, "sql.active_record")`
  # (activerecord testing/query_assertions.rb). Records the SQL every
  # prepare/exec issues during the block and returns it as an Array of
  # SQL strings, the shape Rails' `capture_sql` yields. Nestable: an
  # outer capture is restored on exit. Production never calls this, so
  # the funnel hook stays a single nil check off the hot path.
  #
  # The only instrument that can see the `includes(:assoc)` N+1:
  # byte-identical `compare` is blind to it (eager-load and N+1 render
  # the same HTML; only the query strategy differs). See issue #27.
  def self.capture_sql
    prev = @query_log
    log = []
    @query_log = log
    begin
      yield
    ensure
      @query_log = prev
    end
    log
  end

  # Funnel hook: record one SQL string into the active capture, if any.
  # No-op (single nil check) when no capture is installed.
  def self.record_query(sql)
    @query_log.push(sql) unless @query_log.nil?
  end

  # SQL-value escaping primitives — lowerer-emitted code uses these to
  # inline literals (and runtime values when the placeholder-bind gate is
  # off). With the gate on, runtime values instead flow through `bind_*`
  # above; both shims (this gem-backed one and spinel-FFI) now support
  # binding. Inlining stays safe regardless since the lowerer controls
  # every string that flows here.
  # BYTES go out as a hex BLOB literal, `X'…'`. A NUL cannot ride a
  # quoted literal at all (it ends the SQL text: "unrecognized token"),
  # and a binary value stored as TEXT sorts before every BLOB, so a
  # `t.binary` column filled half one way and half the other orders
  # wrong. Bytes = BINARY-encoded and not plain ASCII (lobsters'
  # `[a, b, c].pack("CCC")` confidence_order), or any string holding a
  # NUL. An ASCII-only BINARY string stays text, as it always was.
  def self.escape_string(s)
    str = s.to_s
    if str.include?("\0") || (str.encoding == Encoding::BINARY && !str.ascii_only?)
      return "X'" + str.unpack1("H*") + "'"
    end
    "'" + str.gsub("'", "''") + "'"
  end

  def self.escape_int(n)
    n.to_i.to_s
  end

  # Nullable-column writes: nil renders the SQL keyword NULL rather
  # than `''` / `0`, so what the schema calls nullable round-trips as
  # nil instead of coming back as the type's zero.
  def self.escape_string_opt(s)
    s.nil? ? "NULL" : escape_string(s)
  end

  def self.escape_int_opt(n)
    n.nil? ? "NULL" : escape_int(n)
  end

  def self.escape_float_opt(f)
    f.nil? ? "NULL" : f.to_f.to_s
  end

  def self.escape_bool_opt(b)
    b.nil? ? "NULL" : escape_bool(b)
  end

  # Render an integer list for `IN (...)` eager-load batches (issue
  # #27). Empty list → "NULL" so `IN (NULL)` is valid SQL matching no
  # rows (an empty `IN ()` is a syntax error).
  def self.escape_int_list(ids)
    return "NULL" if ids.empty?

    ids.map { |i| i.to_i.to_s }.join(", ")
  end

  # SQLite stores booleans as 0/1 integers (no native bool type) —
  # AR `t.boolean :col` maps to INTEGER affinity. Emit the inline
  # literal directly; saves a CAST round-trip vs `'true'`/`'false'`.
  def self.escape_bool(b)
    b ? "1" : "0"
  end

  # Read a boolean column. SQLite returns 0/1 (integer), we widen to
  # Ruby's bool. Nulls coerce to false.
  def self.column_bool(handle, idx)
    column_int(handle, idx) != 0
  end
end

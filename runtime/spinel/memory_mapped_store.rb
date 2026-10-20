# frozen_string_literal: true
# Runtime port of memory_mapped_store 0.4.4's Ruby side
# (lib/memory_mapped_store.rb): `MemoryMappedStore`'s clock helpers, its
# constants and the `MemoryMappedStore::DB` wrapper, which Core's
# `ActiveSupport::Cache::MemoryMappedStore` (compiled from the gem's source)
# builds on.
#
# The gem is an LMDB-backed store: `DB` is the Ruby class around the C
# extension (`memory_mapped_store/memory_mapped_store.so`), which defines
# `MemoryMappedStore::Error`, `FinalError`, `OOMError`, `ReadersFullError`,
# `LMDB_VERSION` and the private primitives `_put` / `_get` / `_del` /
# `_incr` / `_purge` / `_clear` / `_stats` / `_keys` / `_timestamps` /
# `_readers_count` / `_close` / `_initialize`. The extension is the gem's
# own and loads natively (project.rs::CRUBY_EXTENSION_GEMS puts its built
# directory on the load path without activating the gem), so the keys,
# values, per-entry timestamps, expiry, counters, page statistics, size
# limit (MDB_MAP_FULL raises OOMError) and on-disk persistence are the
# real LMDB's, not an imitation.
#
# The gem's own lib/memory_mapped_store.rb is not carried: its
# `if ::Process.respond_to?(:_fork)` block (a module plus a `prepend`) is a
# load-time conditional the ingest does not model, so the compiled entry
# file keeps only `VERSION`. This file is that entry file's Ruby, unchanged
# except for the three requires at its ends: the gem's `version.rb` and
# `active_support/cache/memory_mapped_store.rb` are compiled with the app.

require "fileutils"

require "memory_mapped_store/memory_mapped_store"

module MemoryMappedStore
  # The C extension defines these (rb_define_class_under); written out so
  # the names are declared where the check reads this port.
  class Error < StandardError; end
  class FinalError < Error; end
  class OOMError < FinalError; end
  class ReadersFullError < FinalError; end

  DEFAULT_SIZE = 10 * (2 ** 20) # 10 MB
  DEFAULT_MAX_READERS = 126 # from mdb.c DEFAULT_READERS
  TIMESTAMP_PRECISION = 1_000_000.0

  module ClockGetTime
    def now_us
      Process.clock_gettime(Process::CLOCK_REALTIME, :microsecond)
    end

    def now
      Process.clock_gettime(Process::CLOCK_REALTIME)
    end
  end
  extend ClockGetTime

  module RubyTime
    def now_us
      (Time.now.utc.to_f * TIMESTAMP_PRECISION).to_i
    end

    def now
      Time.now.utc.to_f
    end
  end

  class << self
    # Makes it easier to use Timecop or Rails time helpers
    def allow_time_stubbing!
      extend RubyTime
    end
  end

  class DB
    INSTANCES = ObjectSpace::WeakMap.new

    class << self
      def before_fork
        INSTANCES.each_value(&:lock_closed)
      end

      def after_fork
        INSTANCES.each_value(&:unlock)
      end
    end

    if ::Process.respond_to?(:_fork)
      module ForkHooks
        def _fork
          DB.before_fork
          begin
            super
          ensure
            DB.after_fork
          end
        end
      end
      Process.singleton_class.prepend(ForkHooks)
    end

    attr_reader :max_readers

    def initialize(path, mode: 0o660, max_size: DEFAULT_SIZE, max_readers: DEFAULT_MAX_READERS)
      @path = path
      @mode = mode
      @max_size = max_size
      @max_readers = max_readers
      _initialize(@path, @mode, @max_size, @max_readers)
      @mutex = Mutex.new
      INSTANCES[self] = self
    end

    def size
      stats[:entries]
    end

    def readers_count
      @mutex.synchronize { _readers_count }
    end

    def stats
      stats = @mutex.synchronize { _stats }
      %i[psize depth branch_pages leaf_pages overflow_pages entries].zip(stats).to_h
    end

    def get(key, timestamp: nil)
      now_us = MemoryMappedStore.now_us
      if timestamp
        timestamp = now_us if timestamp == true
        @mutex.synchronize { _get_and_set_timestamp(key, now_us, timestamp) }
      else
        @mutex.synchronize { _get(key, now_us) }
      end
    end

    def put(key, value, timestamp: nil)
      timestamp = MemoryMappedStore.now_us if timestamp == true
      @mutex.synchronize { _put(key, value, timestamp) }
    end

    def del(key)
      @mutex.synchronize { _del(key) }
    end

    def incr(key, incr_by = 1, timestamp: nil)
      @mutex.synchronize { _incr(key, incr_by, timestamp, MemoryMappedStore.now_us) }
    end

    def keys
      @mutex.synchronize { _keys }
    end

    def timestamps
      @mutex.synchronize { _timestamps }
    end

    def purge(cutoff = MemoryMappedStore.now_us)
      @mutex.synchronize { _purge(cutoff) }
    end

    def clear
      @mutex.synchronize do
        _clear
      rescue MemoryMappedStore::Error
        # Starting a transaction may require allocating
        # in which case we can't even truncate the DB, we have to
        # abandon ship.
        _close
        gc_path = "#{@path}.#{rand}"
        begin
          File.rename(@path, gc_path)
          FileUtils.rm_rf(gc_path)
        rescue Errno::ENOENT
          # Likey deleted by another process
        end
      end
    end

    def close
      @mutex.synchronize { _close }
    end

    def lock_closed
      @mutex.lock
      _close
    end

    def unlock
      @mutex.unlock
    end

    def _mkdir_p
      FileUtils.mkdir_p(@path)
    end
  end
end

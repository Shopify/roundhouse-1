# frozen_string_literal: true
# `ActiveSupport::Cache.expand_cache_key` and `ActiveSupport::Cache::
# MemoryStore` — ported from activesupport 8.1 (cache.rb,
# cache/memory_store.rb) for the ruby family and spinel, where campfire's
# `FragmentCache` keeps its bounded store (rubys/roundhouse#698):
#
#     STORE = ActiveSupport::Cache::MemoryStore.new(size: 64.megabytes)
#
# `RecordCache` reads and writes record snapshots in it; `MessagesHelper`
# `fetch`es rendered fragments; `CachedResponses` hands it to Rails as the
# controller's `cache_store`.
#
# THE STORE IS ACTIVESUPPORT'S, BOUND FOR BOUND. Entries are kept in
# least-recently-used order (a read moves its key to the end); an entry
# costs its key's bytes + its value's bytes + 240, as `cached_size` counts
# it; and a write that takes the total past `size` prunes from the
# least-recently-used end down to three quarters of it. A String value is
# copied on the way in and on the way out (`DupCoder`), so a caller
# mutating what it wrote or read does not change the cache. Keys are
# normalized as `Store#expanded_key` does: a record by its `cache_key`, an
# Array by its elements joined with "/", a Hash as sorted `k=v` pairs.
#
# ENTRY OPTIONS: every call takes Rails' `options` (a trailing Hash or
# keywords): `expires_in:`/`expires_at:` (an entry past its time is a
# miss), `force:`, `skip_nil:`, `race_condition_ttl:` (an expired entry
# stays readable that long while `fetch` recomputes it), `namespace:`
# and, for writes, `unless_exist:`. `increment`/`decrement` and
# `fetch_multi` are here. NOT HERE: `version:`, `compress:`, `raw:`,
# coders — a call passing one raises ArgumentError rather than being
# cached with the option ignored; instrumentation
# (`cache_read.active_support`).  Non-String values are
# limited to recursively copied Arrays/Hashes and immutable scalars; other
# object types fail closed rather than being retained by reference. Rails
# counts a Marshal dump for non-Strings; this port counts the copied value's
# `to_s` bytes.
#
# A SUBCLASS OF `Rails::Cache`, so one stands wherever the runtime's own
# store does: campfire's caching tests assign a fresh MemoryStore to
# `Rails.cache` and restore the old one after. The runtime's fragment
# lowering reaches `Rails.cache` through its typed String protocol
# (`read_str`/`write_str`/`fetch_str`), which this class inherits whole;
# the Rails API below is the bounded store.
module ActiveSupport
  module Cache
    # `ActiveSupport::Cache.expand_cache_key(key, namespace = nil)`.
    def self.expand_cache_key(key, namespace = nil)
      out = namespace.nil? ? "" : "#{namespace}/"
      prefix = ENV["RAILS_CACHE_ID"] || ENV["RAILS_APP_VERSION"]
      out += "#{prefix}/" unless prefix.nil?
      out + ActiveSupport::Cache.retrieve_cache_key(key)
    end

    # Rails' `retrieve_cache_key`: a record's `cache_key_with_version`,
    # an Array's elements joined with "/", a Hash as its pairs, nil as
    # "" (its `to_a` is empty), anything else as its `to_param`.
    def self.retrieve_cache_key(key)
      return key.cache_key_with_version.to_s if key.respond_to?(:cache_key_with_version)
      return key.cache_key.to_s if key.respond_to?(:cache_key)
      return key.map { |element| ActiveSupport::Cache.retrieve_cache_key(element) }.join("/") if key.is_a?(Array)
      return ActiveSupport::Cache.retrieve_cache_key(key.to_a) if key.is_a?(Hash)
      return "" if key.nil?
      key.to_s
    end

    # `Store#expanded_key`: the key a store files an entry under.
    def self.expanded_key(key)
      return key.cache_key.to_s if key.respond_to?(:cache_key)
      if key.is_a?(Array)
        return ActiveSupport::Cache.expanded_key(key.first) if key.length == 1
        return key.map { |element| ActiveSupport::Cache.expanded_key(element) }.join("/")
      end
      return key.map { |k, v| "#{k}=#{v}" }.sort.join("/") if key.is_a?(Hash)
      return "" if key.nil?
      key.to_s
    end

    class MemoryStore < Rails::Cache
      PER_ENTRY_OVERHEAD = 240

      def initialize(size: 32 * 1024 * 1024)
        super()
        @data = {}
        @expires = {}
        @max_size = size
        @cache_size = 0
        @lock = Mutex.new
      end

      def read(name, options = nil)
        opts = MemoryStore.options(options)
        key = storage_key(name, opts)
        _, value = @lock.synchronize { live_locked(key) }
        MemoryStore.copy(value)
      end

      def exist?(name, options = nil)
        opts = MemoryStore.options(options)
        key = storage_key(name, opts)
        @lock.synchronize { live_locked(key)[0] }
      end

      # `unless_exist: true` keeps a live entry and answers false.
      def write(name, value, options = nil)
        opts = MemoryStore.options(options)
        write_value(storage_key(name, opts), value, opts)
      end

      # The cached value, or the block's — which is then written. As
      # Rails' `Store#fetch`: `force: true` recomputes; `skip_nil: true`
      # does not cache a nil result; `race_condition_ttl:` keeps an
      # expired entry readable for that long while one caller recomputes
      # it; without a block it is `read` (and `force: true` is an error).
      def fetch(name, options = nil)
        opts = MemoryStore.options(options)
        # Keep one result path: Spinel inlines yield-bearing methods at
        # call sites, where an early return is mis-typed as the caller's.
        value = if block_given?
          key = storage_key(name, opts)
          found = false
          cached = nil
          unless opts[:force]
            found, cached = @lock.synchronize { fetchable_locked(key, opts[:race_condition_ttl].to_i) }
          end
          if found
            MemoryStore.copy(cached)
          else
            computed = yield(name)
            write_value(key, computed, opts) unless computed.nil? && opts[:skip_nil]
            computed
          end
        else
          raise ArgumentError, "Missing block: Calling `Cache#fetch` with `force: true` requires a block." if opts[:force]
          read(name, options)
        end
        value
      end

      def delete(name, options = nil)
        opts = MemoryStore.options(options)
        key = storage_key(name, opts)
        @lock.synchronize { delete_locked(key) }
      end

      # `read_multi(*names, **options)`: a trailing Hash is the options.
      def read_multi(*names)
        options = names.last.instance_of?(Hash) ? names.pop : nil
        opts = MemoryStore.options(options)
        out = {}
        names.each do |name|
          key = storage_key(name, opts)
          found, value = @lock.synchronize { live_locked(key) }
          out[name] = MemoryStore.copy(value) if found
        end
        out
      end

      def write_multi(hash, options = nil)
        hash.each { |name, value| write(name, value, options) }
        true
      end

      # `fetch_multi(*names, **options) { |name| … }`: the cached values by
      # name, the block's for the names missing (written, unless `skip_nil`
      # drops a nil). `force: true` recomputes every name.
      def fetch_multi(*names)
        raise ArgumentError, "Missing block: `Cache#fetch_multi` requires a block." unless block_given?
        options = names.last.instance_of?(Hash) ? names.pop : nil
        opts = MemoryStore.options(options)
        ordered = {}
        writes = {}
        names.each do |name|
          key = storage_key(name, opts)
          found = false
          cached = nil
          unless opts[:force]
            found, cached = @lock.synchronize { live_locked(key) }
          end
          if found
            ordered[name] = MemoryStore.copy(cached)
          else
            computed = yield(name)
            ordered[name] = computed
            writes[name] = computed unless computed.nil? && opts[:skip_nil]
          end
        end
        write_multi(writes, options)
        ordered
      end

      # Rails' `MemoryStore#increment`: an absent or expired counter is
      # created at `amount`; a live one is advanced and keeps its expiry.
      def increment(name, amount = 1, options = nil)
        modify_value(name, amount, options)
      end

      def decrement(name, amount = 1, options = nil)
        modify_value(name, -amount, options)
      end

      def clear
        @lock.synchronize do
          @data.clear
          @expires.clear
          @cache_size = 0
        end
        super
      end

      # Drop every expired entry.
      def cleanup
        @lock.synchronize do
          @data.keys.each { |key| delete_locked(key) if expired_locked?(key) }
        end
        nil
      end

      def prune(target_size)
        @lock.synchronize { prune_locked(target_size) }
        nil
      end

      def inspect
        "#<ActiveSupport::Cache::MemoryStore entries=#{@data.size}, size=#{@cache_size}>"
      end

      OPTIONS = [:expires_in, :expires_at, :force, :skip_nil, :race_condition_ttl, :namespace, :unless_exist]

      # The call's options, checked: `expires_in`/`expires_at` (not both,
      # not negative), `force`, `skip_nil`, `race_condition_ttl`,
      # `namespace` and (writes) `unless_exist`. Anything else — `version`,
      # `compress`, `raw`, a coder — is an option this store does not
      # carry, and raises rather than being dropped.
      def self.options(options)
        opts = options.nil? ? {} : options.dup
        raise ArgumentError, "cache options must be a Hash, got #{options.class}" unless opts.is_a?(Hash)
        opts.each_key do |name|
          raise ArgumentError, "ActiveSupport::Cache::MemoryStore does not support the #{name.inspect} option" unless OPTIONS.include?(name)
        end
        if opts.key?(:expires_in) && opts.key?(:expires_at)
          raise ArgumentError, "Either :expires_in or :expires_at can be supplied, but not both"
        end
        raise ArgumentError, "expires_in parameter should not be a Time. Did you mean to use expires_at? Got: #{opts[:expires_in]}" if opts[:expires_in].is_a?(Time)
        if !opts[:expires_in].nil? && opts[:expires_in].to_f < 0
          raise ArgumentError, "Cache expiration time is invalid, cannot be negative: #{opts[:expires_in]}"
        end
        opts
      end

      def self.copy(value, ancestors = [])
        # Any String, a SafeBuffer included: `dup` keeps the class, so the
        # html-safe body a helper renders (campfire's text presentation,
        # through `auto_link`) comes back html-safe, as Rails' store
        # returns it.
        return value.dup if value.is_a?(String)
        if value.instance_of?(Array)
          raise ArgumentError, "MemoryStore cannot safely copy a recursive value" if ancestors.any? { |ancestor| ancestor.equal?(value) }
          ancestors << value
          out = value.map { |item| MemoryStore.copy(item, ancestors) }
          ancestors.pop
          return out
        end
        if value.instance_of?(Hash)
          raise ArgumentError, "MemoryStore cannot safely copy a recursive value" if ancestors.any? { |ancestor| ancestor.equal?(value) }
          raise ArgumentError, "MemoryStore cannot safely copy an identity Hash" if value.compare_by_identity?
          raise ArgumentError, "MemoryStore cannot safely copy a Hash with a default proc" if value.default_proc
          ancestors << value
          out = Hash.new(MemoryStore.copy(value.default, ancestors))
          value.each do |key, item|
            out[MemoryStore.copy(key, ancestors)] = MemoryStore.copy(item, ancestors)
          end
          ancestors.pop
          return out
        end
        return value if value.nil? || value.equal?(true) || value.equal?(false)
        return value if value.instance_of?(Integer) || value.instance_of?(Float) || value.instance_of?(Symbol)
        raise ArgumentError, "MemoryStore cannot safely copy this value type"
      end

      def self.bytes(value)
        return 0 if value.nil?
        value.is_a?(String) ? value.bytesize : value.to_s.bytesize
      end

      private

      # The key an entry is filed under: the normalized key, behind the
      # call's `namespace:` as `Store#namespace_key` puts it.
      def storage_key(name, opts)
        key = ActiveSupport::Cache.expanded_key(name)
        namespace = opts[:namespace]
        namespace = namespace.call if namespace.respond_to?(:call)
        namespace.nil? ? key : "#{namespace}:#{key}"
      end

      # When the entry written under these options lapses (nil: never).
      def expiry_for(opts)
        if opts[:expires_at]
          opts[:expires_at].to_f
        elsif opts[:expires_in]
          Time.now.to_f + opts[:expires_in].to_f
        end
      end

      def expired_locked?(key)
        at = @expires[key]
        !at.nil? && at <= Time.now.to_f
      end

      # `read_locked`, but an entry past its expiry is dropped and missed.
      def live_locked(key)
        if expired_locked?(key)
          delete_locked(key)
          return [false, nil]
        end
        read_locked(key)
      end

      # `fetch`'s read: as `live_locked`, but an expired entry within its
      # `race_condition_ttl` is kept, lent that long to the callers still
      # reading it while this one recomputes (Rails `handle_expired_entry`).
      def fetchable_locked(key, race_ttl)
        if @data.key?(key) && expired_locked?(key)
          now = Time.now.to_f
          if race_ttl > 0 && now - @expires[key] <= race_ttl
            @expires[key] = now + race_ttl
          else
            delete_locked(key)
          end
          return [false, nil]
        end
        read_locked(key)
      end

      def write_value(key, value, opts)
        payload = MemoryStore.copy(value)
        @lock.synchronize { write_locked(key, payload, opts) }
      end

      # Call only while holding @lock; `payload` is already a copy.
      def write_locked(key, payload, opts)
        return false if opts[:unless_exist] && live_locked(key)[0]
        expires_at = expiry_for(opts)
        if @data.key?(key)
          @cache_size -= MemoryStore.bytes(@data[key]) - MemoryStore.bytes(payload)
          @data.delete(key)
        else
          @cache_size += key.bytesize + MemoryStore.bytes(payload) + PER_ENTRY_OVERHEAD
        end
        @data[key] = payload
        if expires_at.nil?
          @expires.delete(key)
        else
          @expires[key] = expires_at
        end
        prune_locked(@max_size * 3 / 4) if @cache_size > @max_size
        true
      end

      # An absent or expired counter is created at `amount` (with the
      # call's expiry); a live one is advanced and keeps its own.
      def modify_value(name, amount, options)
        opts = MemoryStore.options(options)
        key = storage_key(name, opts)
        @lock.synchronize do
          found, value = live_locked(key)
          if found
            kept = @expires[key]
            result = value.to_i + amount
            write_locked(key, result, {})
            @expires[key] = kept unless kept.nil?
            result
          else
            write_locked(key, Integer(amount), opts)
            amount
          end
        end
      end

      # Return presence separately because a cached nil is still a hit.
      # Call only while holding @lock.
      def read_locked(key)
        return [false, nil] unless @data.key?(key)
        value = @data.delete(key)
        @data[key] = value
        [true, value]
      end

      def delete_locked(key)
        return false unless @data.key?(key)
        payload = @data.delete(key)
        @expires.delete(key)
        @cache_size -= key.bytesize + MemoryStore.bytes(payload) + PER_ENTRY_OVERHEAD
        true
      end

      def prune_locked(target_size)
        @data.keys.each do |key|
          break if @cache_size <= target_size
          delete_locked(key)
        end
      end
    end
  end
end

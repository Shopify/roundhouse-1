# frozen_string_literal: true

# ActiveSupport::Cache::MemCacheStore, ported from the Rails Core runs,
# 8.2.0.alpha (Shopify/rails 5813559f4aed,
# activesupport/lib/active_support/cache/mem_cache_store.rb) for the CRuby and JRuby
# trees, on the real dalli gem. It extends runtime/active_support_cache.rb's
# Store and Strategy::LocalCache, so it loads after that file, and only in
# a tree whose app names the class (Core's VersionedMemCacheStore and
# LocalMemcacheStore subclass it).
#
# Each method follows that source; tests/mem_cache_store_runtime.rb
# runs the same observations against native ActiveSupport and this file,
# both on one dalli, and requires them equal.
#
# Declared with its full name (`class ActiveSupport::Cache::MemCacheStore`),
# so this file declares only that namespace (runtime_reads gates names by
# declaring file); the constants the native body finds lexically in
# ActiveSupport::Cache are written out in full.
#
# Differences, all at the edges:
# - The core extensions the native file requires (`extract_options!`,
#   `index_by`, `5.minutes`) are not installed on core classes; the three
#   calls are written out with the same results (an instance_of?(Hash)
#   last argument is the options, the later of two names with one key
#   wins, and dalli takes the TTL as `to_i` seconds).
# - `ActiveSupport.error_reporter` is reported to only where the tree
#   defines it, as the Store base class does.

begin
  require "dalli"
rescue LoadError => e
  warn "You don't have dalli installed in your application. Please add it to your Gemfile and run bundle install"
  raise e
end

require "connection_pool"

# = Memcached \Cache \Store
#
# A cache store implementation which stores data in Memcached:
# https://memcached.org
#
# +MemCacheStore+ implements the Strategy::LocalCache strategy which
# implements an in-memory cache inside of a block.
class ActiveSupport::Cache::MemCacheStore < ActiveSupport::Cache::Store
  # These options represent behavior overridden by this implementation and should
  # not be allowed to get down to the Dalli client
  OVERRIDDEN_OPTIONS = ActiveSupport::Cache::UNIVERSAL_OPTIONS

  # Advertise cache versioning support.
  def self.supports_cache_versioning?
    true
  end

  prepend ActiveSupport::Cache::Strategy::LocalCache

  # Creates a new Dalli::Client instance with specified addresses and options.
  # If no addresses are provided, we give nil to Dalli::Client, so it uses its fallbacks:
  # - ENV["MEMCACHE_SERVERS"] (if defined)
  # - "127.0.0.1:11211"        (otherwise)
  def self.build_mem_cache(*addresses) # :nodoc:
    addresses = addresses.flatten
    options = addresses.last.instance_of?(Hash) ? addresses.pop : {}
    addresses = nil if addresses.compact.empty?
    pool_options = retrieve_pool_options(options)

    if pool_options
      ConnectionPool.new(**pool_options) { Dalli::Client.new(addresses, options.merge(threadsafe: false)) }
    else
      Dalli::Client.new(addresses, options)
    end
  end

  # Creates a new +MemCacheStore+ object, with the given memcached server
  # addresses. Each address is either a host name, or a host-with-port string
  # in the form of "host_name:port". If no addresses are provided, but
  # <tt>ENV['MEMCACHE_SERVERS']</tt> is defined, it will be used instead.
  # Otherwise, +MemCacheStore+ will connect to localhost:11211 (the default
  # memcached port).
  def initialize(*addresses)
    addresses = addresses.flatten
    options = addresses.last.instance_of?(Hash) ? addresses.pop : {}
    if options.key?(:cache_nils)
      options[:skip_nil] = !options.delete(:cache_nils)
    end
    options[:max_key_size] ||= MAX_KEY_SIZE
    super(options)

    unless [String, NilClass].include?(addresses.first.class)
      raise ArgumentError, "First argument must be an empty array, address, or array of addresses."
    end

    @mem_cache_options = options.dup
    # The value "compress: false" prevents duplicate compression within Dalli.
    @mem_cache_options[:compress] = false
    (OVERRIDDEN_OPTIONS - %i(compress)).each { |name| @mem_cache_options.delete(name) }
    # Set the default serializer for Dalli to Marshal to maintain backward compatibility.
    @mem_cache_options[:serializer] = Marshal
    @data = self.class.build_mem_cache(*(addresses + [@mem_cache_options]))
  end

  def inspect
    instance = @data || @mem_cache_options
    "#<#{self.class} options=#{options.inspect} mem_cache=#{instance.inspect}>"
  end

  # Increment a cached integer value using the memcached incr atomic operator.
  # Returns the updated value.
  #
  # If the key is unset or has expired, it will be set to +amount+.
  # Incrementing a non-numeric value, or a value written without
  # <tt>raw: true</tt>, will fail and return +nil+.
  def increment(name, amount = 1, options = nil)
    options = merged_options(options)
    key = normalize_key(name, options)

    instrument(:increment, key, amount: amount) do
      rescue_error_with nil do
        @data.with { |c| c.incr(key, amount, options[:expires_in], amount) }
      end
    end
  end

  # Decrement a cached integer value using the memcached decr atomic operator.
  # Returns the updated value.
  #
  # If the key is unset or has expired, it will be set to 0. Memcached
  # does not support negative counters.
  def decrement(name, amount = 1, options = nil)
    options = merged_options(options)
    key = normalize_key(name, options)

    instrument(:decrement, key, amount: amount) do
      rescue_error_with nil do
        @data.with { |c| c.decr(key, amount, options[:expires_in], 0) }
      end
    end
  end

  # Clear the entire cache on all memcached servers. This method should
  # be used with care when shared cache is being used.
  def clear(options = nil)
    rescue_error_with(nil) { @data.with { |c| c.flush_all } }
  end

  # Get the statistics from the memcached servers.
  def stats
    @data.with { |c| c.stats }
  end

  private
    # Read an entry from the cache.
    def read_entry(key, **options)
      deserialize_entry(read_serialized_entry(key, **options), **options)
    end

    def read_serialized_entry(key, **options)
      rescue_error_with(nil) do
        @data.with { |c| c.get(key, options) }
      end
    end

    # Write an entry to the cache.
    def write_entry(key, entry, **options)
      write_serialized_entry(key, serialize_entry(entry, **options), **options)
    end

    def write_serialized_entry(key, payload, **options)
      method = options[:unless_exist] ? :add : :set
      expires_in = options[:expires_in].to_i
      if options[:race_condition_ttl] && expires_in > 0 && !options[:raw]
        # Set the memcache expire a few minutes in the future to support race condition ttls on read
        expires_in += 5 * 60
      end
      rescue_error_with nil do
        # Don't pass compress option to Dalli since we are already dealing with compression.
        options.delete(:compress)
        @data.with { |c| !!c.send(method, key, payload, expires_in, **options) }
      end
    end

    # Writes multiple entries in one set_multi, on a dalli that has it.
    if Dalli::Client.method_defined?(:set_multi)
      def write_multi_entries(entries, **options)
        return if entries.empty?

        entries = entries.transform_values { |entry| serialize_entry(entry, **options) }
        expires_in = options[:expires_in].to_i
        if options[:race_condition_ttl] && expires_in > 0 && !options[:raw]
          # Set the memcache expire a few minutes in the future to support race condition ttls on read
          expires_in += 5 * 60
        end

        rescue_error_with(nil) do
          @data.with { |c| c.set_multi(entries, expires_in, options) }
        end
      end
    end

    # Reads multiple entries from the cache implementation.
    def read_multi_entries(names, **options)
      keys_to_names = {}
      names.each { |name| keys_to_names[normalize_key(name, options)] = name }

      rescue_error_with({}) do
        raw_values = @data.with { |c| c.get_multi(keys_to_names.keys) }

        values = {}

        raw_values.each do |key, value|
          entry = deserialize_entry(value, raw: options[:raw])

          unless entry.nil? || entry.expired? || entry.mismatched?(normalize_version(keys_to_names[key], options))
            begin
              values[keys_to_names[key]] = entry.value
            rescue ActiveSupport::Cache::DeserializationError
            end
          end
        end

        values
      end
    end

    # Delete an entry from the cache.
    def delete_entry(key, **options)
      rescue_error_with(false) { @data.with { |c| c.delete(key) } }
    end

    if Gem::Version.new(Dalli::VERSION) >= Gem::Version.new("5.0.6")
      def delete_multi_entries(entries, **options)
        return 0 if entries.empty?

        rescue_error_with(0) do
          @data.with { |c| c.delete_multi(entries) }
        end
      end
    end

    def serialize_entry(entry, raw: false, **options)
      if raw
        entry.value.to_s
      else
        super(entry, raw: raw, **options)
      end
    end

    # Memcache keys are binaries. So we need to force their encoding to binary
    # before applying the regular expression to ensure we are escaping all
    # characters properly.
    def normalize_key(key, options)
      key = expand_and_namespace_key(key, options)
      key = key.b
      key.gsub!(/[\x00-\x20%\x7F-\xFF]/n) { |match| "%#{match.getbyte(0).to_s(16).upcase}" }
      truncate_key(key)
    end

    def deserialize_entry(payload, raw: false, **)
      if payload && raw
        ActiveSupport::Cache::Entry.new(payload)
      else
        super(payload)
      end
    end

    def rescue_error_with(fallback)
      yield
    rescue Dalli::DalliError, ConnectionPool::Error, ConnectionPool::TimeoutError => error
      logger.error("DalliError (#{error}): #{error.message}") if logger
      if ActiveSupport.respond_to?(:error_reporter)
        ActiveSupport.error_reporter&.report(
          error,
          severity: :warning,
          source: "mem_cache_store.active_support",
        )
      end
      fallback
    end
end

# frozen_string_literal: true

# ActiveSupport::Cache, ported from the activesupport native Core runs
# (Shopify/rails 5813559f4aed, 8.2.0.alpha) for the CRuby and JRuby trees: the Store base class (fetch / read / write / delete /
# exist? and their _multi forms, namespaces, versions, expiry, race
# condition TTL, key expansion and truncation, instrumentation), Entry,
# Coder and SerializerWithFallback (the 7.0 / 7.1 cache formats),
# Strategy::LocalCache with its LocalStore and Middleware, MemoryStore,
# NullStore and WriteOptions, plus the two support classes the stores
# read: IsolatedExecutionState and Digest.
#
# Each method follows that source (lib/active_support/cache.rb,
# cache/{entry,coder,serializer_with_fallback,memory_store,null_store}.rb,
# cache/strategy/local_cache{,_middleware}.rb, isolated_execution_state.rb,
# digest.rb); tests/cache_runtime.rb runs the same observations against
# native ActiveSupport and this file and requires them equal.
#
# Differences, all at the edges:
# - The core extensions the native files require (`extract_options!`,
#   `index_by` / `index_with`, `to_param` / `to_query`, `try`,
#   `underscore`, `camelize`, `1.kilobyte`, `cattr_accessor`) are not
#   installed on core classes; the few calls the stores make are written
#   out below with the same results.
# - `lookup_store` / `retrieve_store_class` find the stores this file
#   defines, and MemCacheStore where runtime/active_support_mem_cache_store.rb
#   is loaded (a tree whose app names the class). FileStore and
#   RedisCacheStore are not ported (RedisCacheStore needs redis); asking
#   for a missing one raises the same "Could not find cache store adapter"
#   error a missing native adapter does, naming what it needs.
# - The :message_pack serializer needs ActiveSupport::MessagePack and the
#   msgpack gem, which the tree does not carry; selecting it raises, and
#   a MessagePack payload is not recognized on load (native behavior when
#   msgpack is not installed).
# - `ActiveSupport::Cache.format_version` and `Digest.hash_digest_class`
#   start at the library defaults (7.0, MD5). A Rails app's
#   `load_defaults` / `config.active_support.*` set them through railtie
#   initializers; assign them the same way.

require "monitor"
require "openssl"
require "zlib"
require "cgi/escape"

module ActiveSupport
  # Execution-scoped state (isolated_execution_state.rb): per thread by
  # default, per fiber at `isolation_level = :fiber`.
  module IsolatedExecutionState
    @isolation_level = nil

    Thread.attr_accessor :active_support_execution_state
    Fiber.attr_accessor :active_support_execution_state

    class << self
      attr_reader :isolation_level, :scope

      def isolation_level=(level)
        return if level == @isolation_level

        unless %i(thread fiber).include?(level)
          raise ArgumentError, "isolation_level must be `:thread` or `:fiber`, got: `#{level.inspect}`"
        end

        clear if @isolation_level

        @scope =
          case level
          when :thread then Thread
          when :fiber then Fiber
          end

        @isolation_level = level
      end

      def [](key)
        if (state = @scope.current.active_support_execution_state)
          state[key]
        end
      end

      def []=(key, value)
        state = (@scope.current.active_support_execution_state ||= {})
        state[key] = value
      end

      def key?(key)
        @scope.current.active_support_execution_state&.key?(key)
      end

      def delete(key)
        @scope.current.active_support_execution_state&.delete(key)
      end

      def clear
        @scope.current.active_support_execution_state&.clear
      end

      def context
        scope.current
      end

      def share_with(other, except: [], &block)
        if (state = other.active_support_execution_state)
          copied_state = state.dup
          Array(except).each { |key| copied_state.delete(key) }
        end
        old_state, context.active_support_execution_state = context.active_support_execution_state, copied_state
        block.call
      ensure
        context.active_support_execution_state = old_state
      end
    end

    self.isolation_level = :thread
  end

  # digest.rb
  class Digest
    class << self
      def hash_digest_class
        @hash_digest_class ||= OpenSSL::Digest::MD5
      end

      def hash_digest_class=(klass)
        raise ArgumentError, "#{klass} is expected to implement hexdigest class method" unless klass.respond_to?(:hexdigest)
        @hash_digest_class = klass
      end

      def hexdigest(arg)
        hash_digest_class.hexdigest(arg)[0...32]
      end
    end
  end

  module Cache
    UNIVERSAL_OPTIONS = [
      :coder,
      :compress,
      :compress_threshold,
      :compressor,
      :expire_in,
      :expired_in,
      :expires_in,
      :namespace,
      :race_condition_ttl,
      :serializer,
      :skip_nil,
      :raw,
      :max_key_size,
    ].freeze

    OPTION_ALIASES = {
      expires_in: [:expire_in, :expired_in].freeze
    }.freeze

    DEFAULT_COMPRESS_LIMIT = 1024 # 1.kilobyte

    DeserializationError = Class.new(StandardError)

    # `ActiveSupport.error_reporter.report(error, source: "active_support.cache")`,
    # where the tree carries the reporter (runtime/active_support_reporters.rb,
    # which every Ruby-family tree naming Rails.error has). Returns nil, as
    # ErrorReporter#report does.
    def self.report_error(error)
      if ActiveSupport.respond_to?(:error_reporter) && (reporter = ActiveSupport.error_reporter)
        reporter.report(error, source: "active_support.cache")
      end
      nil
    end

    # The stores this file defines, by `lookup_store` symbol.
    PORTED_STORES = %i(memory_store null_store).freeze
    # Native adapters this file does not define, and why one is missing.
    # MemCacheStore is runtime/active_support_mem_cache_store.rb's, carried
    # where the app names the class; `lookup_store` finds it once loaded.
    UNPORTED_STORES = {
      file_store: "ActiveSupport::Cache::FileStore is not ported",
      mem_cache_store: "ActiveSupport::Cache::MemCacheStore is carried only where the app names the class",
      redis_cache_store: "ActiveSupport::Cache::RedisCacheStore needs the redis gem, which is not ported",
    }.freeze

    @format_version = 7.0

    class << self
      attr_accessor :format_version

      def lookup_store(store = nil, *parameters)
        case store
        when Symbol
          options = parameters.last.instance_of?(Hash) ? parameters.pop : {}
          retrieve_store_class(store).new(*parameters, **options)
        when Array
          lookup_store(*store)
        when nil
          ActiveSupport::Cache::MemoryStore.new
        else
          store
        end
      end

      def expand_cache_key(key, namespace = nil)
        expanded_cache_key = namespace ? +"#{namespace}/" : +""

        if (prefix = ENV["RAILS_CACHE_ID"] || ENV["RAILS_APP_VERSION"])
          expanded_cache_key << "#{prefix}/"
        end

        expanded_cache_key << retrieve_cache_key(key)
        expanded_cache_key
      end

      # Object#to_param from core_ext/object/to_param: nil, true and false
      # are themselves, an Array joins its members' params with "/", a
      # Hash is its query string, anything else answers its own
      # `to_param` or `to_s`.
      def to_param(value) # :nodoc:
        case value
        when nil, true, false then value
        when Array then value.map { |element| to_param(element) }.join("/")
        when Hash then to_query(value)
        else value.respond_to?(:to_param) ? value.to_param : value.to_s
        end
      end

      # Object#to_query / Array#to_query / Hash#to_query.
      def to_query(value, key = nil) # :nodoc:
        case value
        when Hash
          value.filter_map do |k, v|
            next if (v.is_a?(Hash) || v.is_a?(Array)) && v.empty?
            to_query(v, key ? "#{key}[#{k}]" : k)
          end.sort! * "&"
        when Array
          prefix = "#{key}[]"
          if value.empty?
            to_query(nil, prefix)
          else
            value.map { |element| to_query(element, prefix) }.join("&")
          end
        else
          "#{CGI.escape(to_param(key).to_s)}=#{CGI.escape(to_param(value).to_s)}"
        end
      end

      # String#underscore with no acronyms registered.
      def underscore(name) # :nodoc:
        return name unless /[A-Z-]|::/.match?(name)
        word = name.gsub("::", "/")
        word.gsub!(/(?<=[A-Z])(?=[A-Z][a-z])|(?<=[a-z\d])(?=[A-Z])/, "_")
        word.tr!("-", "_")
        word.downcase!
        word
      end

      private
        def retrieve_cache_key(key)
          if key.respond_to?(:cache_key_with_version) then key.cache_key_with_version
          elsif key.respond_to?(:cache_key)           then key.cache_key
          elsif key.is_a?(Array)                       then to_param(key.map { |element| retrieve_cache_key(element) })
          elsif key.respond_to?(:to_a)                 then retrieve_cache_key(key.to_a)
          else                                              to_param(key)
          end.to_s
        end

        def retrieve_store_class(store)
          name = store.to_s.split("_").map(&:capitalize).join
          loaded = UNPORTED_STORES.key?(store) && ActiveSupport::Cache.const_defined?(name, false)
          unless PORTED_STORES.include?(store) || loaded
            reason = UNPORTED_STORES.fetch(store) { "cannot load such file -- active_support/cache/#{store}" }
            raise "Could not find cache store adapter for #{store} (#{reason})"
          end
          ActiveSupport::Cache.const_get(name)
        end
    end

    # cache/entry.rb
    class Entry # :nodoc:
      class << self
        def unpack(members)
          new(members[0], expires_at: members[1], version: members[2])
        end
      end

      attr_reader :version

      def initialize(value, compressed: false, version: nil, expires_in: nil, expires_at: nil, **)
        @value      = value
        @version    = version
        @created_at = 0.0
        @expires_in = expires_at&.to_f || expires_in && (expires_in.to_f + Time.now.to_f)
        @compressed = true if compressed
      end

      def value
        compressed? ? uncompress(@value) : @value
      end

      def mismatched?(version)
        @version && version && @version != version
      end

      def expired?
        @expires_in && @created_at + @expires_in <= Time.now.to_f
      end

      def expires_at
        @expires_in ? @created_at + @expires_in : nil
      end

      def expires_at=(value)
        if value
          @expires_in = value.to_f - @created_at
        else
          @expires_in = nil
        end
      end

      def bytesize
        case value
        when NilClass
          0
        when String
          @value.bytesize
        else
          @s ||= Marshal.dump(@value).bytesize
        end
      end

      def compressed? # :nodoc:
        defined?(@compressed)
      end

      def compressed(compress_threshold)
        return self if compressed?

        case @value
        when nil, true, false, Numeric
          uncompressed_size = 0
        when String
          uncompressed_size = @value.bytesize
        else
          serialized = Marshal.dump(@value)
          uncompressed_size = serialized.bytesize
        end

        if uncompressed_size >= compress_threshold
          serialized ||= Marshal.dump(@value)
          compressed = Zlib::Deflate.deflate(serialized)

          if compressed.bytesize < uncompressed_size
            return Entry.new(compressed, compressed: true, expires_at: expires_at, version: version)
          end
        end
        self
      end

      def local?
        false
      end

      def dup_value!
        if @value && !compressed? && !(@value.is_a?(Numeric) || @value == true || @value == false)
          if @value.is_a?(String)
            @value = @value.dup
          else
            @value = Marshal.load(Marshal.dump(@value))
          end
        end
      end

      def pack
        members = [value, expires_at, version]
        members.pop while !members.empty? && members.last.nil?
        members
      end

      private
        def uncompress(value)
          marshal_load(Zlib::Inflate.inflate(value))
        end

        def marshal_load(payload)
          Marshal.load(payload)
        rescue ArgumentError => error
          raise Cache::DeserializationError, error.message
        end
    end

    # cache/coder.rb
    class Coder # :nodoc:
      def initialize(serializer, compressor, legacy_serializer: false)
        @serializer = serializer
        @compressor = compressor
        @legacy_serializer = legacy_serializer
      end

      def dump(entry)
        return @serializer.dump(entry) if @legacy_serializer

        dump_compressed(entry, Float::INFINITY)
      end

      def dump_compressed(entry, threshold)
        return @serializer.dump_compressed(entry, threshold) if @legacy_serializer

        if (type = type_for_string(entry.value))
          payload = entry.value.b
        else
          type = OBJECT_DUMP_TYPE
          payload = @serializer.dump(entry.value)
        end

        if (compressed = try_compress(payload, threshold))
          payload = compressed
          type = type | COMPRESSED_FLAG
        end

        expires_at = entry.expires_at || -1.0

        version = dump_version(entry.version) if entry.version
        version_length = version&.bytesize || -1

        header = [type, expires_at, version_length].pack(PACKED_TEMPLATE)
        "#{SIGNATURE}#{header}#{version}#{payload}".freeze
      end

      def load(dumped)
        unless signature?(dumped)
          return begin
            @serializer.load(dumped)
          rescue => error
            Cache.report_error(error)
          end
        end

        begin
          type = dumped.unpack1(PACKED_TYPE_TEMPLATE)
          expires_at = dumped.unpack1(PACKED_EXPIRES_AT_TEMPLATE)
          version_length = dumped.unpack1(PACKED_VERSION_LENGTH_TEMPLATE)

          expires_at = nil if expires_at < 0
          version = load_version(dumped.byteslice(PACKED_VERSION_INDEX, version_length)) if version_length >= 0
          payload = dumped.byteslice((PACKED_VERSION_INDEX + [version_length, 0].max)..)
        rescue ArgumentError, TypeError => error
          Cache.report_error(error)
          return
        end

        compressor = @compressor if type & COMPRESSED_FLAG > 0
        serializer = STRING_DESERIALIZERS[type & ~COMPRESSED_FLAG] || @serializer

        LazyEntry.new(serializer, compressor, payload, version: version, expires_at: expires_at)
      end

      SIGNATURE = "\x00\x11".b.freeze

      OBJECT_DUMP_TYPE = 0x01

      STRING_ENCODINGS = {
        0x02 => Encoding::UTF_8,
        0x03 => Encoding::BINARY,
        0x04 => Encoding::US_ASCII,
      }.freeze

      COMPRESSED_FLAG = 0x80

      PACKED_TEMPLATE = "CEl<"
      PACKED_TYPE_TEMPLATE = "@#{SIGNATURE.bytesize}C".freeze
      PACKED_EXPIRES_AT_TEMPLATE = "@#{[0].pack(PACKED_TYPE_TEMPLATE).bytesize}E".freeze
      PACKED_VERSION_LENGTH_TEMPLATE = "@#{[0].pack(PACKED_EXPIRES_AT_TEMPLATE).bytesize}l<".freeze
      PACKED_VERSION_INDEX = [0].pack(PACKED_VERSION_LENGTH_TEMPLATE).bytesize

      MARSHAL_SIGNATURE = "\x04\x08".b.freeze

      class StringDeserializer
        def initialize(encoding)
          @encoding = encoding
        end

        def load(payload)
          payload.force_encoding(@encoding)
        end
      end

      STRING_DESERIALIZERS = STRING_ENCODINGS.transform_values { |encoding| StringDeserializer.new(encoding) }

      class LazyEntry < Cache::Entry
        def initialize(serializer, compressor, payload, **options)
          super(payload, **options)
          @serializer = serializer
          @compressor = compressor
          @resolved = false
        end

        def value
          if !@resolved
            @value = begin
              @serializer.load(@compressor ? @compressor.inflate(@value) : @value)
            rescue => error
              Cache.report_error(error)
              raise DeserializationError, error.message
            end
            @resolved = true
          end
          @value
        end

        def mismatched?(version)
          super.tap { |mismatched| value if !mismatched }
        rescue Cache::DeserializationError
          true
        end
      end


      private
        def signature?(dumped)
          dumped.is_a?(String) && dumped.start_with?(SIGNATURE)
        end

        def type_for_string(value)
          STRING_ENCODINGS.key(value.encoding) if value.instance_of?(String)
        end

        def try_compress(string, threshold)
          if @compressor && string.bytesize >= threshold
            compressed = @compressor.deflate(string)
            compressed if compressed.bytesize < string.bytesize
          end
        end

        def dump_version(version)
          if version.encoding != Encoding::UTF_8 || version.start_with?(MARSHAL_SIGNATURE)
            Marshal.dump(version)
          else
            version.b
          end
        end

        def load_version(dumped_version)
          if dumped_version.start_with?(MARSHAL_SIGNATURE)
            Marshal.load(dumped_version)
          else
            dumped_version.force_encoding(Encoding::UTF_8)
          end
        end
    end

    # cache/serializer_with_fallback.rb
    module SerializerWithFallback # :nodoc:
      def self.[](format)
        if format.to_s.include?("message_pack")
          raise LoadError, "roundhouse: the :#{format} cache serializer needs ActiveSupport::MessagePack " \
            "and the msgpack gem, which are not ported"
        end
        SERIALIZERS.fetch(format)
      end

      def load(dumped)
        if dumped.is_a?(String)
          if MessagePackWithFallback.dumped?(dumped)
            MessagePackWithFallback._load(dumped)
          elsif Marshal71WithFallback.dumped?(dumped)
            Marshal71WithFallback._load(dumped)
          elsif Marshal70WithFallback.dumped?(dumped)
            Marshal70WithFallback._load(dumped)
          else
            Cache::Store.logger&.warn("Unrecognized payload prefix #{dumped.byteslice(0).inspect}; deserializing as nil")
            nil
          end
        elsif PassthroughWithFallback.dumped?(dumped)
          PassthroughWithFallback._load(dumped)
        else
          Cache::Store.logger&.warn("Unrecognized payload class #{dumped.class}; deserializing as nil")
          nil
        end
      end

      private
        def marshal_load(payload)
          Marshal.load(payload)
        rescue ArgumentError => error
          raise Cache::DeserializationError, error.message
        end

      module PassthroughWithFallback
        include SerializerWithFallback
        extend self

        def dump(entry)
          entry
        end

        def dump_compressed(entry, threshold)
          entry.compressed(threshold)
        end

        def _load(entry)
          entry
        end

        def dumped?(dumped)
          dumped.is_a?(Cache::Entry)
        end
      end

      module Marshal70WithFallback
        include SerializerWithFallback
        extend self

        MARK_UNCOMPRESSED = "\x00".b.freeze
        MARK_COMPRESSED   = "\x01".b.freeze

        def dump(entry)
          MARK_UNCOMPRESSED + Marshal.dump(entry.pack)
        end

        def dump_compressed(entry, threshold)
          dumped = Marshal.dump(entry.pack)

          if dumped.bytesize >= threshold
            compressed = Zlib::Deflate.deflate(dumped)
            return MARK_COMPRESSED + compressed if compressed.bytesize < dumped.bytesize
          end

          MARK_UNCOMPRESSED + dumped
        end

        def _load(marked)
          dumped = marked.byteslice(1..-1)
          if marked.start_with?(MARK_COMPRESSED)
            dumped = begin
              Zlib::Inflate.inflate(dumped)
            rescue Zlib::Error => error
              raise Cache::DeserializationError, "#{error.class}: #{error.message}"
            end
          end
          Cache::Entry.unpack(marshal_load(dumped))
        end

        def dumped?(dumped)
          dumped.start_with?(MARK_UNCOMPRESSED, MARK_COMPRESSED)
        end
      end

      module Marshal71WithFallback
        include SerializerWithFallback
        extend self

        MARSHAL_SIGNATURE = "\x04\x08".b.freeze

        def dump(value)
          Marshal.dump(value)
        end

        def _load(dumped)
          marshal_load(dumped)
        end

        def dumped?(dumped)
          dumped.start_with?(MARSHAL_SIGNATURE)
        end
      end

      # Native with msgpack missing: `available?` is false, so no payload
      # is taken for MessagePack.
      module MessagePackWithFallback
        include SerializerWithFallback
        extend self

        def dumped?(dumped)
          false
        end
      end

      SERIALIZERS = {
        passthrough: PassthroughWithFallback,
        marshal_7_0: Marshal70WithFallback,
        marshal_7_1: Marshal71WithFallback,
      }.freeze

    end

    module Strategy
      # cache/strategy/local_cache.rb
      module LocalCache
        # cache/strategy/local_cache_middleware.rb
        class Middleware # :nodoc:
          attr_reader :name
          attr_accessor :cache

          def initialize(name, cache)
            @name = name
            @cache = cache
            @app = nil
          end

          def new(app)
            @app = app
            self
          end

          def call(env)
            cache.new_local_cache
            response = @app.call(env)
            response[2] = ::Rack::BodyProxy.new(response[2]) do
              cache.unset_local_cache
            end
            cleanup_on_body_close = true
            response
          rescue ::Rack::Utils::InvalidParameterError
            [400, {}, []]
          ensure
            cache.unset_local_cache unless cleanup_on_body_close
          end
        end

        module LocalCacheRegistry # :nodoc:
          extend self

          def cache_for(local_cache_key)
            registry = ActiveSupport::IsolatedExecutionState[:active_support_local_cache_registry] ||= {}
            registry[local_cache_key]
          end

          def set_cache_for(local_cache_key, value)
            registry = ActiveSupport::IsolatedExecutionState[:active_support_local_cache_registry] ||= {}
            registry[local_cache_key] = value
          end
        end

        class LocalStore
          def initialize
            @data = {}
          end

          def clear(options = nil)
            @data.clear
          end

          def read_entry(key)
            @data[key]
          end

          def read_multi_entries(keys)
            @data.slice(*keys)
          end

          def write_entry(key, entry)
            @data[key] = entry
            true
          end

          def delete_entry(key)
            !!@data.delete(key)
          end

          def fetch_entry(key) # :nodoc:
            @data.fetch(key) { @data[key] = yield }
          end
        end

        def initialize(...)
          super
          @local_cache_key = "#{ActiveSupport::Cache.underscore(self.class.name)}_local_cache_#{object_id}".gsub(/[\/-]/, "_").to_sym
        end

        def with_local_cache(&block)
          use_temporary_local_cache(LocalStore.new, &block)
        end

        def new_local_cache
          LocalCacheRegistry.set_cache_for(local_cache_key, LocalStore.new)
        end

        def unset_local_cache
          LocalCacheRegistry.set_cache_for(local_cache_key, nil)
        end

        def local_cache
          LocalCacheRegistry.cache_for(local_cache_key)
        end

        def middleware
          @middleware ||= Middleware.new("ActiveSupport::Cache::Strategy::LocalCache", self)
        end

        def clear(options = nil) # :nodoc:
          return super unless (cache = local_cache)
          cache.clear(options)
          super
        end

        def cleanup(options = nil) # :nodoc:
          return super unless (cache = local_cache)
          cache.clear(options)
          super
        end

        def delete_matched(matcher, options = nil) # :nodoc:
          return super unless (cache = local_cache)
          cache.clear(options)
          super
        end

        def increment(name, amount = 1, **options) # :nodoc:
          return super unless local_cache
          value = super
          write_cache_value(name, value, raw: true, **options)
          value
        end

        def decrement(name, amount = 1, **options) # :nodoc:
          return super unless local_cache
          value = super
          write_cache_value(name, value, raw: true, **options)
          value
        end

        def fetch_multi(*names, &block) # :nodoc:
          return super if local_cache.nil? || names.empty?

          options = names.last.instance_of?(Hash) ? names.pop : {}
          options = merged_options(options)

          results = read_multi_entries_from_local_cache(names, options)

          if results.size < names.size
            results.merge!(super(*(names - results.keys), options, &block))
          end

          results.slice(*names)
        end

        private
          attr_reader :local_cache_key

          def read_serialized_entry(key, raw: false, **options)
            if (cache = local_cache)
              if options[:delete]
                entry = super
                cache.delete_entry(key)
                entry
              else
                hit = true
                entry = cache.fetch_entry(key) do
                  hit = false
                  super
                end
                options[:event][:store] = cache.class.name if hit && options[:event]
                entry
              end
            else
              super
            end
          end

          def read_multi_entries_from_local_cache(names, options)
            keys_to_names = names.each_with_object({}) { |name, map| map[normalize_key(name, options)] = name }

            local_entries = local_cache.read_multi_entries(keys_to_names.keys)
            local_entries.each_with_object({}) do |(key, value), result|
              next if value.nil? # recorded cache miss

              entry = deserialize_entry(value, **options)

              normalized_key = keys_to_names[key]
              if entry.nil?
                result[normalized_key] = nil
              elsif entry.expired? || entry.mismatched?(normalize_version(normalized_key, options))
                local_cache.delete_entry(key)
              else
                result[normalized_key] = entry.value
              end
            end
          end

          def read_multi_entries(names, **options)
            return super unless local_cache

            results = read_multi_entries_from_local_cache(names, options)

            if results.size < names.size
              results.merge!(super(names - results.keys, **options))
            end

            results.slice(*names)
          end

          def write_serialized_entry(key, payload, **)
            if (return_value = super)
              local_cache.write_entry(key, payload) if local_cache
            else
              local_cache.delete_entry(key) if local_cache
            end
            return_value
          end

          def delete_entry(key, **)
            local_cache.delete_entry(key) if local_cache
            super
          end

          def write_cache_value(name, value, **options)
            name = normalize_key(name, options)
            cache = local_cache
            if value
              cache.write_entry(name, serialize_entry(new_entry(value, **options), **options))
            else
              cache.delete_entry(name)
            end
          end

          def use_temporary_local_cache(temporary_cache)
            save_cache = LocalCacheRegistry.cache_for(local_cache_key)
            begin
              LocalCacheRegistry.set_cache_for(local_cache_key, temporary_cache)
              yield
            ensure
              LocalCacheRegistry.set_cache_for(local_cache_key, save_cache)
            end
          end
      end
    end

    # cache.rb: the abstract store.
    class Store
      DEFAULT_POOL_OPTIONS = { size: 5, timeout: 5 }.freeze
      MAX_KEY_SIZE = 250

      # class_attribute :logger, instance_predicate: false
      # class_attribute :raise_on_invalid_cache_expiration_time, instance_predicate: false, default: false
      #
      # A class reads the value the nearest class at or above it set; a
      # writer on a subclass sets it for that subclass only. An instance
      # reads its own value once written, else its class's.
      %i[logger raise_on_invalid_cache_expiration_time].each do |name|
        ivar = :"@__class_attr_#{name}"
        instance_variable_set(ivar, name == :logger ? nil : false)
        singleton_class.define_method(name) do
          owner = self
          owner = owner.superclass until owner.instance_variable_defined?(ivar)
          owner.instance_variable_get(ivar)
        end
        singleton_class.define_method(:"#{name}=") { |value| instance_variable_set(ivar, value) }
        define_method(name) do
          instance_variable_defined?(:"@#{name}") ? instance_variable_get(:"@#{name}") : self.class.public_send(name)
        end
        attr_writer name
      end

      attr_reader :silence, :options
      alias :silence? :silence

      class << self
        private
          def retrieve_pool_options(options)
            if options.key?(:pool)
              pool_options = options.delete(:pool)
            else
              pool_options = true
            end

            case pool_options
            when false, nil
              return false
            when true
              pool_options = DEFAULT_POOL_OPTIONS
            when Hash
              pool_options[:size] = Integer(pool_options[:size]) if pool_options.key?(:size)
              pool_options[:timeout] = Float(pool_options[:timeout]) if pool_options.key?(:timeout)
              pool_options = DEFAULT_POOL_OPTIONS.merge(pool_options)
            else
              raise TypeError, "Invalid :pool argument, expected Hash, got: #{pool_options.inspect}"
            end

            pool_options unless pool_options.empty?
          end
      end

      def initialize(options = nil)
        @options = options ? validate_options(normalize_options(options)) : {}

        @options[:compress] = true unless @options.key?(:compress)
        @options[:compress_threshold] ||= DEFAULT_COMPRESS_LIMIT

        @max_key_size = @options.delete(:max_key_size)
        @max_key_size = MAX_KEY_SIZE if @max_key_size.nil? # allow 'false' as a value

        @coder = @options.delete(:coder) do
          legacy_serializer = Cache.format_version < 7.1 && !@options[:serializer]
          serializer = @options.delete(:serializer) || default_serializer
          serializer = Cache::SerializerWithFallback[serializer] if serializer.is_a?(Symbol)
          compressor = @options.delete(:compressor) { Zlib }

          Cache::Coder.new(serializer, compressor, legacy_serializer: legacy_serializer)
        end

        @coder ||= Cache::SerializerWithFallback[:passthrough]

        @coder_supports_compression = @coder.respond_to?(:dump_compressed)
      end

      def silence!
        @silence = true
        self
      end

      def mute
        previous_silence, @silence = @silence, true
        yield
      ensure
        @silence = previous_silence
      end

      def fetch(name, options = nil, &block)
        if block_given?
          options = merged_options(options)
          key = normalize_key(name, options)

          entry = nil
          unless options[:force]
            instrument(:read, key, options) do |payload|
              cached_entry = read_entry(key, **options, event: payload)
              entry = handle_expired_entry(cached_entry, key, options)
              if entry
                if entry.mismatched?(normalize_version(name, options))
                  entry = nil
                else
                  begin
                    entry.value
                  rescue DeserializationError
                    entry = nil
                  end
                end
              end
              payload[:super_operation] = :fetch if payload
              payload[:hit] = !!entry if payload
            end
          end

          if entry
            get_entry_value(entry, key, options)
          else
            save_block_result_to_cache(name, key, options, &block)
          end
        elsif options && options[:force]
          raise ArgumentError, "Missing block: Calling `Cache#fetch` with `force: true` requires a block."
        else
          read(name, options)
        end
      end

      def read(name, options = nil)
        options = merged_options(options)
        key     = normalize_key(name, options)
        version = normalize_version(name, options)

        instrument(:read, key, options) do |payload|
          entry = read_entry(key, **options, event: payload)

          if entry
            if entry.expired?
              delete_entry(key, **options)
              payload[:hit] = false if payload
              nil
            elsif entry.mismatched?(version)
              payload[:hit] = false if payload
              nil
            else
              payload[:hit] = true if payload
              begin
                entry.value
              rescue DeserializationError
                payload[:hit] = false
                nil
              end
            end
          else
            payload[:hit] = false if payload
            nil
          end
        end
      end

      def read_multi(*names)
        return {} if names.empty?

        options = names.last.instance_of?(Hash) ? names.pop : {}
        options = merged_options(options)
        keys    = names.map { |name| normalize_key(name, options) }

        instrument_multi :read_multi, keys, options do |payload|
          read_multi_entries(names, **options, event: payload).tap do |results|
            payload[:hits] = results.keys.map { |name| normalize_key(name, options) }
          end
        end
      end

      def write_multi(hash, options = nil)
        return hash if hash.empty?

        options = merged_options(options)
        normalized_hash = hash.transform_keys { |key| normalize_key(key, options) }

        instrument_multi :write_multi, normalized_hash, options do |payload|
          entries = hash.each_with_object({}) do |(name, value), memo|
            memo[normalize_key(name, options)] = Entry.new(value, **options, version: normalize_version(name, options))
          end

          write_multi_entries entries, **options
        end
      end

      def fetch_multi(*names)
        raise ArgumentError, "Missing block: `Cache#fetch_multi` requires a block." unless block_given?
        return {} if names.empty?

        options = names.last.instance_of?(Hash) ? names.pop : {}
        options = merged_options(options)
        keys    = names.map { |name| normalize_key(name, options) }
        writes  = {}
        ordered = instrument_multi :read_multi, keys, options do |payload|
          if options[:force]
            reads = {}
          else
            reads = read_multi_entries(names, **options)
          end

          ordered = names.each_with_object({}) do |name, result|
            result[name] = reads.fetch(name) { writes[name] = yield(name) }
          end
          writes.compact! if options[:skip_nil]

          payload[:hits] = reads.keys.map { |name| normalize_key(name, options) }
          payload[:super_operation] = :fetch_multi

          ordered
        end

        write_multi(writes, options)

        ordered
      end

      def write(name, value, options = nil)
        options = merged_options(options)
        key = normalize_key(name, options)

        instrument(:write, key, options) do
          entry = Entry.new(value, **options, version: normalize_version(name, options))
          write_entry(key, entry, **options)
        end
      end

      def delete(name, options = nil)
        options = merged_options(options)
        key = normalize_key(name, options)

        instrument(:delete, key, options) do
          delete_entry(key, **options)
        end
      end

      def delete_multi(names, options = nil)
        return 0 if names.empty?

        options = merged_options(options)
        names = names.map { |key| normalize_key(key, options) }

        instrument_multi(:delete_multi, names, options) do
          delete_multi_entries(names, **options)
        end
      end

      def exist?(name, options = nil)
        options = merged_options(options)
        key = normalize_key(name, options)

        instrument(:exist?, key, options) do |payload|
          entry = read_entry(key, **options, event: payload)
          (entry && !entry.expired? && !entry.mismatched?(normalize_version(name, options))) || false
        end
      end

      def new_entry(value, options = nil) # :nodoc:
        Entry.new(value, **merged_options(options))
      end

      def delete_matched(matcher, options = nil)
        raise NotImplementedError.new("#{self.class.name} does not support delete_matched")
      end

      def increment(name, amount = 1, options = nil)
        raise NotImplementedError.new("#{self.class.name} does not support increment")
      end

      def decrement(name, amount = 1, options = nil)
        raise NotImplementedError.new("#{self.class.name} does not support decrement")
      end

      def read_counter(name, **options)
        options = merged_options(options).merge(raw: true)
        read(name, **options)&.to_i
      end

      def write_counter(name, value, **options)
        options = merged_options(options).merge(raw: true)
        write(name, value.to_i, **options)
      end

      def cleanup(options = nil)
        raise NotImplementedError.new("#{self.class.name} does not support cleanup")
      end

      def clear(options = nil)
        raise NotImplementedError.new("#{self.class.name} does not support clear")
      end

      def namespace
        @options[:namespace]
      end

      def namespace=(namespace)
        @options[:namespace] = namespace
      end

      private
        def default_serializer
          case Cache.format_version
          when 7.0
            Cache::SerializerWithFallback[:marshal_7_0]
          when 7.1
            Cache::SerializerWithFallback[:marshal_7_1]
          else
            raise ArgumentError, "Unrecognized ActiveSupport::Cache.format_version: #{Cache.format_version.inspect}"
          end
        end

        def key_matcher(pattern, options) # :doc:
          prefix = options[:namespace].is_a?(Proc) ? options[:namespace].call : options[:namespace]
          if prefix
            source = pattern.source
            if source.start_with?("^")
              source = source[1, source.length]
            else
              source = ".*#{source[0, source.length]}"
            end
            Regexp.new("^#{Regexp.escape(prefix)}:#{source}", pattern.options)
          else
            pattern
          end
        end

        def read_entry(key, **options)
          raise NotImplementedError.new
        end

        def write_entry(key, entry, **options)
          raise NotImplementedError.new
        end

        def serialize_entry(entry, **options)
          options = merged_options(options)
          if @coder_supports_compression && options[:compress]
            @coder.dump_compressed(entry, options[:compress_threshold])
          else
            @coder.dump(entry)
          end
        end

        def deserialize_entry(payload, **)
          payload.nil? ? nil : @coder.load(payload)
        rescue DeserializationError
          nil
        end

        def read_multi_entries(names, **options)
          names.each_with_object({}) do |name, results|
            key   = normalize_key(name, options)
            entry = read_entry(key, **options)

            next unless entry

            version = normalize_version(name, options)

            if entry.expired?
              delete_entry(key, **options)
            elsif !entry.mismatched?(version)
              results[name] = entry.value
            end
          end
        end

        def write_multi_entries(hash, **options)
          hash.each do |key, entry|
            write_entry key, entry, **options
          end
        end

        def delete_entry(key, **options)
          raise NotImplementedError.new
        end

        def delete_multi_entries(entries, **options)
          entries.count { |key| delete_entry(key, **options) }
        end

        def merged_options(call_options)
          if call_options
            call_options = normalize_options(call_options)
            if call_options.key?(:expires_in) && call_options.key?(:expires_at)
              raise ArgumentError, "Either :expires_in or :expires_at can be supplied, but not both"
            end

            expires_at = call_options.delete(:expires_at)
            call_options[:expires_in] = (expires_at - Time.now) if expires_at

            if call_options[:expires_in].is_a?(Time)
              expires_in = call_options[:expires_in]
              raise ArgumentError.new("expires_in parameter should not be a Time. Did you mean to use expires_at? Got: #{expires_in}")
            end
            if call_options[:expires_in]&.negative?
              expires_in = call_options.delete(:expires_in)
              handle_invalid_expires_in("Cache expiration time is invalid, cannot be negative: #{expires_in}")
            end

            if options.empty?
              call_options
            else
              options.merge(call_options)
            end
          else
            options
          end
        end

        def handle_invalid_expires_in(message)
          error = ArgumentError.new(message)
          if raise_on_invalid_cache_expiration_time
            raise error
          else
            ActiveSupport.error_reporter&.report(error, handled: true, severity: :warning) if ActiveSupport.respond_to?(:error_reporter)
            logger.error("#{error.class}: #{error.message}") if logger
          end
        end

        def normalize_options(options)
          options = options.dup
          OPTION_ALIASES.each do |canonical_name, aliases|
            alias_key = aliases.detect { |key| options.key?(key) }
            options[canonical_name] ||= options[alias_key] if alias_key
            aliases.each { |key| options.delete(key) }
          end

          options
        end

        def validate_options(options)
          if options.key?(:coder) && options[:serializer]
            raise ArgumentError, "Cannot specify :serializer and :coder options together"
          end

          if options.key?(:coder) && options[:compressor]
            raise ArgumentError, "Cannot specify :compressor and :coder options together"
          end

          if Cache.format_version < 7.1 && !options[:serializer] && options[:compressor]
            raise ArgumentError, "Cannot specify :compressor option when using" \
              " default serializer and cache format version is < 7.1"
          end

          options
        end

        def normalize_key(key, options = nil)
          key = expand_and_namespace_key(key, options)
          truncate_key(key)
        end

        def expand_and_namespace_key(key, options = nil)
          str_key = key.class == ::String ? key : expanded_key(key)
          raise(ArgumentError, "key cannot be blank") if !str_key || str_key.empty?

          namespace_key str_key, options
        end

        def truncate_key(key)
          if key && @max_key_size && key.bytesize > @max_key_size
            suffix = ":hash:#{ActiveSupport::Digest.hexdigest(key)}"
            truncate_at = @max_key_size - suffix.bytesize
            key = key.byteslice(0, truncate_at)
            key.scrub!("")
            "#{key}#{suffix}"
          else
            key
          end
        end

        def namespace_key(key, call_options = nil)
          namespace = if call_options&.key?(:namespace)
            call_options[:namespace]
          else
            options[:namespace]
          end

          if namespace.respond_to?(:call)
            namespace = namespace.call
          end

          if key && key.encoding != Encoding::UTF_8
            key = key.dup.force_encoding(Encoding::UTF_8)
          end

          if namespace
            "#{namespace}:#{key}"
          else
            key
          end
        end

        def expanded_key(key)
          return key.cache_key.to_s if key.respond_to?(:cache_key)

          Cache.to_param(
            case key
            when Array
              if key.size > 1
                key.collect { |element| expanded_key(element) }
              else
                expanded_key(key.first)
              end
            when Hash
              key.collect { |k, v| "#{k}=#{v}" }.sort!
            else
              key
            end
          )
        end

        def normalize_version(key, options = nil)
          version = options && options[:version]
          (version.nil? ? nil : Cache.to_param(version)) || expanded_version(key)
        end

        def expanded_version(key)
          if key.respond_to?(:cache_version) then Cache.to_param(key.cache_version)
          elsif key.is_a?(Array)              then Cache.to_param(key.map { |element| expanded_version(element) }.tap(&:compact!))
          elsif key.respond_to?(:to_a)        then expanded_version(key.to_a)
          end
        end

        def instrument(operation, key, options = nil, &block)
          unless silence?
            logger&.debug do
              debug_key = ": #{key}" if key
              debug_options = " (#{options.inspect})" unless cache_blank?(options)
              "Cache #{operation}#{debug_key}#{debug_options}"
            end
          end

          payload = {
            store: self.class.name,
            key: key
          }
          payload.merge!(options) if options.is_a?(Hash)
          ActiveSupport::Notifications.instrument("cache_#{operation}.active_support", payload) do
            block&.call(payload)
          end
        end

        def instrument_multi(operation, keys, options = nil, &block)
          unless silence?
            logger&.debug do
              debug_key = ": #{keys.size} key(s) specified"
              debug_options = " (#{options.inspect})" unless cache_blank?(options)
              "Cache #{operation}#{debug_key}#{debug_options}"
            end
          end

          payload = {
            store: self.class.name,
            key: keys
          }
          payload.merge!(options) if options.is_a?(Hash)
          ActiveSupport::Notifications.instrument("cache_#{operation}.active_support", payload) do
            block&.call(payload)
          end
        end

        # `options.blank?`
        def cache_blank?(options)
          options.nil? || options == false || (options.respond_to?(:empty?) && options.empty?)
        end

        def handle_expired_entry(entry, key, options)
          if entry && entry.expired?
            race_ttl = options[:race_condition_ttl].to_i
            if (race_ttl > 0) && (Time.now.to_f - entry.expires_at <= race_ttl)
              entry.expires_at = Time.now.to_f + race_ttl
              write_entry(key, entry, **options, expires_in: race_ttl * 2)
            else
              delete_entry(key, **options)
            end
            entry = nil
          end
          entry
        end

        def get_entry_value(entry, key, options)
          instrument(:fetch_hit, key, options)
          entry.value
        end

        def save_block_result_to_cache(name, key, options)
          options = options.dup

          result = instrument(:generate, key, options) do
            yield(name, WriteOptions.new(options))
          end

          write(name, result, options) unless result.nil? && options[:skip_nil]
          result
        end
    end

    # cache.rb: what `fetch`'s block may change about its own write.
    class WriteOptions
      def initialize(options) # :nodoc:
        @options = options
      end

      def version
        @options[:version]
      end

      def version=(version)
        @options[:version] = version
      end

      def expires_in
        @options[:expires_in]
      end

      def expires_in=(expires_in)
        @options.delete(:expires_at)
        @options[:expires_in] = expires_in
      end

      def expires_at
        @options[:expires_at]
      end

      def expires_at=(expires_at)
        @options.delete(:expires_in)
        @options[:expires_at] = expires_at
      end
    end

    # cache/memory_store.rb
    class MemoryStore < Store
      prepend Strategy::LocalCache

      module DupCoder # :nodoc:
        extend self

        def dump(entry)
          if entry.value && entry.value != true && !entry.value.is_a?(Numeric)
            Cache::Entry.new(dump_value(entry.value), expires_at: entry.expires_at, version: entry.version)
          else
            entry
          end
        end

        def dump_compressed(entry, threshold)
          compressed_entry = entry.compressed(threshold)
          compressed_entry.compressed? ? compressed_entry : dump(entry)
        end

        def load(entry)
          if !entry.compressed? && entry.value.is_a?(String)
            Cache::Entry.new(load_value(entry.value), expires_at: entry.expires_at, version: entry.version)
          else
            entry
          end
        end

        MARSHAL_SIGNATURE = "\x04\x08".b.freeze

        private
          def dump_value(value)
            if value.is_a?(String) && !value.start_with?(MARSHAL_SIGNATURE)
              value.dup
            else
              Marshal.dump(value)
            end
          end

          def load_value(string)
            if string.start_with?(MARSHAL_SIGNATURE)
              Marshal.load(string)
            else
              string.dup
            end
          end
      end

      def initialize(options = nil)
        options ||= {}
        options[:coder] = DupCoder unless options.key?(:coder) || options.key?(:serializer)
        # Compression is disabled by default for MemoryStore.
        options[:compress] ||= false
        super(options)
        @data = {}
        @max_size = options[:size] || 32 * 1024 * 1024 # 32.megabytes
        @max_prune_time = options[:max_prune_time] || 2
        @cache_size = 0
        @monitor = Monitor.new
        @pruning = false
      end

      def self.supports_cache_versioning?
        true
      end

      def clear(options = nil)
        synchronize do
          @data.clear
          @cache_size = 0
        end
      end

      def cleanup(options = nil)
        options = merged_options(options)
        instrument(:cleanup, nil, size: @data.size) do
          keys = synchronize { @data.keys }
          keys.each do |key|
            entry = deserialize_entry(@data[key])
            delete_entry(key, **options) if entry && entry.expired?
          end
        end
      end

      def prune(target_size, max_time = nil)
        return if pruning?
        @pruning = true
        begin
          start_time = Process.clock_gettime(Process::CLOCK_MONOTONIC)
          cleanup
          instrument(:prune, target_size, from: @cache_size) do
            keys = synchronize { @data.keys }
            keys.each do |key|
              delete_entry(key, **options)
              return if @cache_size <= target_size || (max_time && Process.clock_gettime(Process::CLOCK_MONOTONIC) - start_time > max_time)
            end
          end
        ensure
          @pruning = false
        end
      end

      def pruning?
        @pruning
      end

      def increment(name, amount = 1, **options)
        instrument(:increment, normalize_key(name, options), amount: amount) do
          modify_value(name, amount, **options)
        end
      end

      def decrement(name, amount = 1, **options)
        instrument(:decrement, normalize_key(name, options), amount: amount) do
          modify_value(name, -amount, **options)
        end
      end

      def delete_matched(matcher, options = nil)
        options = merged_options(options)
        matcher = key_matcher(matcher, options)

        instrument(:delete_matched, matcher.inspect) do
          keys = synchronize { @data.keys }
          keys.each do |key|
            delete_entry(key, **options) if key.match(matcher)
          end
        end
      end

      def inspect # :nodoc:
        "#<#{self.class.name} entries=#{@data.size}, size=#{@cache_size}, options=#{@options.inspect}>"
      end

      def synchronize(&block) # :nodoc:
        @monitor.synchronize(&block)
      end

      PER_ENTRY_OVERHEAD = 240

      private
        def cached_size(key, payload)
          key.to_s.bytesize + payload.bytesize + PER_ENTRY_OVERHEAD
        end

        def read_entry(key, **options)
          entry = nil
          synchronize do
            payload = @data.delete(key)
            if payload
              @data[key] = payload
              entry = deserialize_entry(payload)
            end
          end
          entry
        end

        def write_entry(key, entry, **options)
          payload = serialize_entry(entry, **options)
          synchronize do
            return false if options[:unless_exist] && exist?(key, namespace: nil)

            old_payload = @data[key]
            if old_payload
              @cache_size -= (old_payload.bytesize - payload.bytesize)
            else
              @cache_size += cached_size(key, payload)
            end
            @data[key] = payload
            prune(@max_size * 0.75, @max_prune_time) if @cache_size > @max_size
            true
          end
        end

        def delete_entry(key, **options)
          synchronize do
            payload = @data.delete(key)
            @cache_size -= cached_size(key, payload) if payload
            !!payload
          end
        end

        def modify_value(name, amount, **options)
          options = merged_options(options)
          key     = normalize_key(name, options)
          version = normalize_version(name, options)

          synchronize do
            entry = read_entry(key, **options)

            if !entry || entry.expired? || entry.mismatched?(version)
              write(name, Integer(amount), options)
              amount
            else
              num = entry.value.to_i + amount
              entry = Entry.new(num, expires_at: entry.expires_at, version: entry.version)
              write_entry(key, entry)
              num
            end
          end
        end
    end

    # cache/null_store.rb
    class NullStore < Store
      prepend Strategy::LocalCache

      def self.supports_cache_versioning?
        true
      end

      def clear(options = nil)
      end

      def cleanup(options = nil)
      end

      def increment(name, amount = 1, **options)
      end

      def decrement(name, amount = 1, **options)
      end

      def delete_matched(matcher, options = nil)
      end

      # ActiveSupport::InspectBackport.apply(self): Ruby 4's Kernel#inspect
      # reads instance_variables_to_inspect; an older Ruby gets the same
      # rendering defined here.
      if RUBY_VERSION < "4"
        def inspect # :nodoc:
          ivars = instance_variables_to_inspect
          klass = self.class.name || self.class.inspect
          addr = "0x%x" % object_id
          pairs = ivars.filter_map do |ivar|
            "#{ivar}=#{instance_variable_get(ivar).inspect}" if instance_variable_defined?(ivar)
          end
          pairs.empty? ? "#<#{klass}:#{addr}>" : "#<#{klass}:#{addr} #{pairs.join(", ")}>"
        end
      end

      private
        def instance_variables_to_inspect
          [:@options].freeze
        end

        def read_entry(key, **s)
          deserialize_entry(read_serialized_entry(key))
        end

        def read_serialized_entry(_key, **)
        end

        def write_entry(key, entry, **)
          write_serialized_entry(key, serialize_entry(entry))
        end

        def write_serialized_entry(_key, _payload, **)
          true
        end

        def delete_entry(key, **options)
          false
        end
    end
  end
end

# frozen_string_literal: true

# Active Record's multiple-database model, ported from Shopify/rails
# 5813559f4aed (8.2.0.alpha) for the CRuby PostgreSQL tree:
#
#   connection_handling.rb    establish_connection, connects_to,
#                             connected_to(_many / _all_shards),
#                             connecting_to, while_preventing_writes,
#                             prohibit_shard_swapping, connection_pool,
#                             lease_connection, connection,
#                             with_connection, ...
#   core.rb                   connection_handler, connected_to_stack,
#                             current_role / current_shard /
#                             current_preventing_writes, connection_class,
#                             primary_class?, default_role / default_shard
#   connection_handler.rb     ConnectionHandler and ConnectionDescriptor
#   pool_manager.rb           role -> shard -> PoolConfig
#   pool_config.rb            the lazily built pool of one entry
#   connection_pool.rb        the pool surface: lease, with_connection,
#                             release, pin/unpin, disconnect, checkout
#
# One handler entry (pool) per [connection name, role, shard], one per
# configuration name even when two configurations name the same
# database, each established without connecting. A pool is a `Db` pool
# of its own (runtime/db.rb, `lazy:`): its adapters are built on first
# checkout and connect on their first statement, as Rails' are.
#
# The configuration a pool connects with is its resolved database.yml
# entry with the run's endpoint over it (`DatabaseConfigurations.
# connection_entry`): ROUNDHOUSE_DATABASE_URL for the primary and for
# every configuration naming the same database the primary names;
# ROUNDHOUSE_<NAME>_DATABASE_URL for any other. A configuration that
# names another database and has no endpoint of its own is refused when
# its pool first connects, never sent to the primary's database.
#
# Model queries reach `Db` without a class (ActiveRecord.adapter, the
# lowered `_adapter_*` primitives, Relation). Each such entry point runs
# inside its class's pool (`ActiveRecord::ConnectionHandling.route`), so
# a model's statement runs on the connection Rails' `connection_pool`
# names for it, under the current role and shard.
#
# Not ported (each raises NotImplementedError naming it at the call):
# the schema cache, query cache control on a pool, async query
# executors, `shard_selector`, `connects_to` with a block, `reaper`, and
# `ActiveRecord.permanent_connection_checkout = :disallowed/:deprecated`.
require "set"
require "monitor"

module ActiveRecord
  class AdapterNotSpecified < (defined?(ActiveRecordError) ? ActiveRecordError : StandardError); end unless defined?(AdapterNotSpecified)
  class ReadOnlyError < (defined?(ActiveRecordError) ? ActiveRecordError : StandardError); end unless defined?(ReadOnlyError)

  # errors.rb: raised when a connection is looked up and none is defined.
  class ConnectionNotDefined < ConnectionNotEstablished
    attr_reader :connection_name, :role, :shard

    def initialize(message = nil, connection_name: nil, role: nil, shard: nil)
      super(message)
      @connection_name = connection_name
      @role = role
      @shard = shard
    end
  end unless defined?(ConnectionNotDefined)

  # errors.rb (subclasses ArgumentError for backwards compatibility).
  class ShardSwapProhibitedError < ArgumentError; end unless defined?(ShardSwapProhibitedError)

  # active_record.rb: `singleton_class.attr_accessor :writing_role`
  # (default :writing), `:reading_role` (default :reading),
  # `:application_record_class` (nil).
  class << self
    attr_writer :writing_role, :reading_role
    attr_accessor :application_record_class

    def writing_role
      @writing_role ||= :writing
    end

    def reading_role
      @reading_role ||= :reading
    end
  end

  module ConnectionHandling
    # Execution-scoped state: ActiveSupport::IsolatedExecutionState when
    # the tree carries it, else fiber-local (its default isolation is the
    # thread; Thread#[] is fiber-local, which only differs inside fibers).
    def self.state(key)
      if defined?(ActiveSupport::IsolatedExecutionState) && ActiveSupport::IsolatedExecutionState.respond_to?(:[])
        ActiveSupport::IsolatedExecutionState[key]
      else
        Thread.current[key]
      end
    end

    def self.set_state(key, value)
      if defined?(ActiveSupport::IsolatedExecutionState) && ActiveSupport::IsolatedExecutionState.respond_to?(:[]=)
        ActiveSupport::IsolatedExecutionState[key] = value
      else
        Thread.current[key] = value
      end
    end

    # Run the block on `klass`'s pool (its connection specification
    # under the current role and shard), as a model's statement runs on
    # `klass.connection_pool` in Rails. Re-entrant: inside the same pool
    # it only yields. With no pool in the handler (a program driving Db's
    # databases directly, see Base.connection) Db's current database.
    def self.route(klass)
      return yield unless klass.is_a?(Class) && klass <= ActiveRecord::Base
      return yield if klass.connection_handler.connection_pool_names.empty?
      key = klass.connection_pool.db_key
      return yield if Db.context[:name] == key
      Db.with_database(key) { yield }
    end

    # The pool an adapter belongs to, by its Db pool name.
    @pools_by_key = {}
    class << self
      attr_reader :pools_by_key
    end
  end

  module ConnectionAdapters
    class ConnectionHandler
      class ConnectionDescriptor # :nodoc:
        def initialize(name, primary = false)
          @name = name
          @primary = primary
        end

        def name
          primary_class? ? "ActiveRecord::Base" : @name
        end

        def primary_class?
          @primary
        end

        def current_preventing_writes
          ActiveRecord::Base.preventing_writes?(@name)
        end
      end

      def initialize
        @connection_name_to_pool_manager = {}
        @lock = Monitor.new
      end

      def prevent_writes # :nodoc:
        ConnectionHandling.state(:active_record_prevent_writes)
      end

      def prevent_writes=(prevent_writes) # :nodoc:
        ConnectionHandling.set_state(:active_record_prevent_writes, prevent_writes)
      end

      def connection_pool_names # :nodoc:
        connection_name_to_pool_manager.keys
      end

      def connection_pool_list(role = nil)
        if role.nil? || role == :all
          connection_name_to_pool_manager.values.flat_map { |m| m.pool_configs.map(&:pool) }
        else
          connection_name_to_pool_manager.values.flat_map { |m| m.pool_configs(role).map(&:pool) }
        end
      end
      alias :connection_pools :connection_pool_list

      def each_connection_pool(role = nil, &block) # :nodoc:
        role = nil if role == :all
        return enum_for(__method__, role) unless block_given?

        connection_name_to_pool_manager.values.each do |manager|
          manager.each_pool_config(role) do |pool_config|
            yield pool_config.pool
          end
        end
      end

      def establish_connection(config, owner_name: Base, role: Base.current_role, shard: Base.current_shard, clobber: false)
        owner_name = determine_owner_name(owner_name, config)

        pool_config = resolve_pool_config(config, owner_name, role, shard)
        db_config = pool_config.db_config

        pool_manager = set_pool_manager(pool_config.connection_descriptor)

        existing_pool_config = pool_manager.get_pool_config(role, shard)

        if !clobber && existing_pool_config && existing_pool_config.db_config == db_config
          if owner_name.primary_class? && (existing_pool_config.connection_descriptor != owner_name)
            existing_pool_config.connection_descriptor = owner_name
          end

          existing_pool_config.pool
        else
          disconnect_pool_from_pool_manager(pool_manager, role, shard)
          pool_manager.set_pool_config(role, shard, pool_config)

          payload = {
            connection_name: pool_config.connection_descriptor.name,
            role: role,
            shard: shard,
            config: db_config.configuration_hash
          }

          if defined?(ActiveSupport::Notifications) && ActiveSupport::Notifications.respond_to?(:instrumenter)
            ActiveSupport::Notifications.instrumenter.instrument("!connection.active_record", payload) do
              pool_config.pool
            end
          else
            pool_config.pool
          end
        end
      end

      def active_connections?(role = nil)
        each_connection_pool(role).any?(&:active_connection?)
      end

      def clear_active_connections!(role = nil)
        each_connection_pool(role).each do |pool|
          pool.release_connection
          pool.disable_query_cache!
        end
      end

      def clear_reloadable_connections!(role = nil)
        each_connection_pool(role).each(&:clear_reloadable_connections!)
      end

      def clear_all_connections!(role = nil)
        each_connection_pool(role).each(&:disconnect!)
      end

      def flush_idle_connections!(role = nil)
        each_connection_pool(role).each(&:flush!)
      end

      def retrieve_connection(connection_name, role: ActiveRecord::Base.current_role, shard: ActiveRecord::Base.current_shard) # :nodoc:
        pool = retrieve_connection_pool(connection_name, role: role, shard: shard, strict: true)
        pool.lease_connection
      end

      def connected?(connection_name, role: ActiveRecord::Base.current_role, shard: ActiveRecord::Base.current_shard)
        pool = retrieve_connection_pool(connection_name, role: role, shard: shard)
        pool && pool.connected?
      end

      def remove_connection_pool(connection_name, role: ActiveRecord::Base.current_role, shard: ActiveRecord::Base.current_shard)
        if pool_manager = get_pool_manager(connection_name)
          disconnect_pool_from_pool_manager(pool_manager, role, shard)
        end
      end

      def retrieve_connection_pool(connection_name, role: ActiveRecord::Base.current_role, shard: ActiveRecord::Base.current_shard, strict: false)
        pool_manager = get_pool_manager(connection_name)
        pool = pool_manager&.get_pool_config(role, shard)&.pool

        if strict && !pool
          selector = [
            ("'#{shard}' shard" unless shard == ActiveRecord::Base.default_shard),
            ("'#{role}' role" unless role == ActiveRecord::Base.default_role),
          ].compact.join(" and ")

          selector = [
            (connection_name unless connection_name == "ActiveRecord::Base"),
            (selector unless selector.empty?),
          ].compact.join(" with ")

          selector = " for #{selector}" unless selector.empty?
          message = "No database connection defined#{selector}."

          raise ConnectionNotDefined.new(message, connection_name: connection_name, shard: shard, role: role)
        end

        pool
      end

      private
        attr_reader :connection_name_to_pool_manager

        def get_pool_manager(connection_name)
          connection_name_to_pool_manager[connection_name]
        end

        def set_pool_manager(connection_descriptor)
          @lock.synchronize { connection_name_to_pool_manager[connection_descriptor.name] ||= PoolManager.new }
        end

        def disconnect_pool_from_pool_manager(pool_manager, role, shard)
          pool_config = pool_manager.remove_pool_config(role, shard)

          if pool_config
            pool_config.disconnect!
            pool_config.db_config
          end
        end

        def resolve_pool_config(config, connection_name, role, shard)
          db_config = Base.configurations.resolve(config)
          raise(AdapterNotSpecified, "database configuration does not specify adapter") unless db_config.adapter
          ConnectionAdapters::PoolConfig.new(connection_name, db_config, role, shard)
        end

        def determine_owner_name(owner_name, config)
          if owner_name.is_a?(String) || owner_name.is_a?(Symbol)
            ConnectionDescriptor.new(owner_name.to_s)
          elsif config.is_a?(Symbol)
            ConnectionDescriptor.new(config.to_s)
          else
            owner_name
          end
        end
    end

    class PoolManager # :nodoc:
      def initialize
        @role_to_shard_mapping = Hash.new { |h, k| h[k] = {} }
      end

      def shard_names
        @role_to_shard_mapping.values.flat_map { |shard_map| shard_map.keys }.uniq
      end

      def role_names
        @role_to_shard_mapping.keys
      end

      def pool_configs(role = nil)
        if role
          @role_to_shard_mapping[role].values
        else
          @role_to_shard_mapping.flat_map { |_, shard_map| shard_map.values }
        end
      end

      def each_pool_config(role = nil, &block)
        if role
          @role_to_shard_mapping[role].each_value(&block)
        else
          @role_to_shard_mapping.each_value do |shard_map|
            shard_map.each_value(&block)
          end
        end
      end

      def remove_role(role)
        @role_to_shard_mapping.delete(role)
      end

      def remove_pool_config(role, shard)
        @role_to_shard_mapping[role].delete(shard)
      end

      def get_pool_config(role, shard)
        @role_to_shard_mapping[role][shard]
      end

      def set_pool_config(role, shard, pool_config)
        if pool_config
          @role_to_shard_mapping[role][shard] = pool_config
        else
          raise ArgumentError, "The `pool_config` for the :#{role} role and :#{shard} shard was `nil`. Please check your configuration. If you want your writing role to be something other than `:writing` set `config.active_record.writing_role` in your application configuration. The same setting should be applied for the `reading_role` if applicable."
        end
      end
    end

    class PoolConfig # :nodoc:
      include MonitorMixin

      attr_reader :db_config, :role, :shard, :connection_descriptor

      def initialize(connection_class, db_config, role, shard)
        super()
        self.connection_descriptor = connection_class
        @db_config = db_config
        @role = role
        @shard = shard
        @pool = nil
      end

      def connection_descriptor=(connection_descriptor)
        case connection_descriptor
        when ConnectionHandler::ConnectionDescriptor
          @connection_descriptor = connection_descriptor
        else
          @connection_descriptor = ConnectionHandler::ConnectionDescriptor.new(connection_descriptor.name, connection_descriptor.primary_class?)
        end
      end

      def disconnect!(automatic_reconnect: false)
        return unless @pool

        synchronize do
          return unless @pool

          @pool.automatic_reconnect = automatic_reconnect
          @pool.disconnect!
        end

        nil
      end

      def pool
        @pool || synchronize { @pool ||= ConnectionAdapters::ConnectionPool.new(self) }
      end

      def discard_pool!
        return unless @pool

        synchronize do
          return unless @pool

          @pool.discard!
          @pool = nil
        end
      end

      def schema_reflection
        raise NotImplementedError, "PoolConfig#schema_reflection: the schema cache is not ported (the runtime's schema is compiled)"
      end
    end

    # Rails' ConnectionPool surface over one `Db` pool. Replaces the
    # eager MVP pool (runtime/active_record/connection_pool.rb) in this
    # tree; nothing here uses that one.
    class ConnectionPool
      attr_reader :pool_config, :db_config, :role, :shard, :size, :db_key
      # The environment variable whose endpoint this pool connects through
      # (DatabaseConfigurations.pool_endpoint), nil when it is refused.
      attr_reader :endpoint_variable
      attr_accessor :automatic_reconnect, :checkout_timeout

      remove_method :free, :free= if method_defined?(:free)
      remove_method :size= if method_defined?(:size=)

      def initialize(pool_config)
        @pool_config = pool_config
        @db_config = pool_config.db_config
        @role = pool_config.role
        @shard = pool_config.shard
        hash = @db_config.configuration_hash
        @checkout_timeout = (hash[:checkout_timeout] || 5).to_f
        @automatic_reconnect = true
        @pinned_connection = nil
        @pinned_connections_depth = 0
        @db_key = ConnectionPool.db_key_for(pool_config)
        # A stale Db pool under the same name belongs to a pool Rails has
        # already disconnected and replaced (establish_connection).
        Db.remove_database(@db_key) if Db.database_configured?(@db_key)
        entry = DatabaseConfiguration.connection_entry_for(@db_config)
        endpoint = DatabaseConfiguration.endpoint_for(@db_config)
        @endpoint_variable = endpoint.is_a?(Exception) ? nil : endpoint
        if entry.is_a?(Exception)
          @unconnectable = entry
          Db.configure_database(@db_key, {}, pool_size: 1, lazy: true)
        else
          Db.configure_database(@db_key, entry, lazy: true)
        end
        @size = Db.database_pool_size(@db_key)
        ConnectionHandling.pools_by_key[@db_key] = self
      end

      # Base's writing/default pool is `Db`'s "primary" (the database a
      # bare Db statement runs on); every other pool is named by its
      # connection name, role and shard.
      def self.db_key_for(pool_config)
        descriptor = pool_config.connection_descriptor
        if descriptor.name == "ActiveRecord::Base" && pool_config.role == ActiveRecord.writing_role && pool_config.shard == :default
          "primary"
        else
          "#{descriptor.name}/#{pool_config.role}/#{pool_config.shard}"
        end
      end

      def connection_descriptor
        pool_config.connection_descriptor
      end

      def connection_class # :nodoc:
        pool_config.connection_descriptor
      end
      alias :connection_klass :connection_class

      def inspect # :nodoc:
        name_field = " name=#{db_config.name.inspect}" unless db_config.name == "primary"
        shard_field = " shard=#{@shard.inspect}" unless @shard == :default
        "#<#{self.class.name} env_name=#{db_config.env_name.inspect}#{name_field} role=#{role.inspect}#{shard_field}>"
      end

      def lease_connection
        refuse_unconnectable!
        Db.with_database(@db_key) { Db.current_adapter }
      end

      # Rails' `connection_lease.sticky.nil?`: no lease taken yet, or one
      # `lease_connection` made sticky. The runtime's leases are sticky.
      def permanent_lease? # :nodoc:
        true
      end

      def active_connection?
        !Db.leased_adapter(@db_key).nil?
      end

      def active_connection
        Db.leased_adapter(@db_key)
      end

      def release_connection(existing_lease = nil)
        Db.with_database(@db_key) { Db.release_connection } if active_connection?
        nil
      end

      def with_connection(prevent_permanent_checkout: false)
        refuse_unconnectable!
        Db.with_database(@db_key) do
          Db.with_connection { yield Db.current_adapter }
        end
      end

      def connected?
        Db.pool_connections(@db_key).any?(&:connected?)
      end

      def connections
        Db.pool_connections(@db_key)
      end

      def checkout(checkout_timeout = @checkout_timeout)
        refuse_unconnectable!
        Db.checkout_from(@db_key)
      end

      def checkin(conn)
        Db.checkin_to(@db_key, conn)
      end

      def disconnect(raise_on_acquisition_timeout = true)
        Db.disconnect_database(@db_key)
      end

      def disconnect!
        disconnect(false)
      end

      def discard!
        Db.remove_database(@db_key)
        ConnectionHandling.pools_by_key.delete(@db_key)
      end

      def clear_reloadable_connections(raise_on_acquisition_timeout = true)
        disconnect(raise_on_acquisition_timeout)
      end

      def clear_reloadable_connections!
        clear_reloadable_connections(false)
      end

      def flush(minimum_idle = nil)
        nil
      end

      def flush!
        nil
      end

      # Rails 8.2 pin_connection!: one connection for every lease of the
      # pool, in a non-joinable transaction begun now (`_lazy: false`),
      # so a `transaction` inside it is a savepoint. `lock_thread` needs
      # nothing more: the adapter's lock is always a Monitor.
      def pin_connection!(lock_thread) # :nodoc:
        refuse_unconnectable!
        @pinned_connection ||= (active_connection || checkout)
        @pinned_connections_depth += 1
        Db.pin(@db_key, @pinned_connection)
        @pinned_connection.pinned = true
        @pinned_connection.begin_transaction(joinable: false, _lazy: false)
      end

      def unpin_connection! # :nodoc:
        raise "There isn't a pinned connection #{object_id}" unless @pinned_connection

        clean = true
        @pinned_connection.lock.synchronize do
          @pinned_connections_depth -= 1
          connection = @pinned_connection
          @pinned_connection = nil if @pinned_connections_depth.zero?

          if connection.transaction_open?
            connection.rollback_transaction
          else
            # Something committed or rolled back the transaction
            clean = false
            connection.reset!
          end

          if @pinned_connection.nil?
            connection.pinned = false
            Db.unpin(@db_key)
            Db.with_database(@db_key) { Db.release_connection } if Db.leased_adapter(@db_key).equal?(connection)
            checkin(connection) unless Db.free?(@db_key, connection)
          end
        end

        clean
      end

      def stat
        conns = connections
        { size: size, connections: conns.size, busy: conns.size - Db.free_count(@db_key), dead: 0,
          idle: Db.free_count(@db_key), waiting: 0, checkout_timeout: checkout_timeout }
      end

      def disable_query_cache!
        nil
      end

      def schema_cache
        raise NotImplementedError, "ConnectionPool#schema_cache: the schema cache is not ported (the runtime's schema is compiled)"
      end

      def schema_reflection
        raise NotImplementedError, "ConnectionPool#schema_reflection: the schema cache is not ported (the runtime's schema is compiled)"
      end

      def enable_query_cache!
        raise NotImplementedError, "ConnectionPool#enable_query_cache!: per-pool query cache control is not ported"
      end

      def query_cache_enabled
        false
      end

      def async_executor
        raise NotImplementedError, "ConnectionPool#async_executor: asynchronous queries are not ported"
      end

      def reaper
        raise NotImplementedError, "ConnectionPool#reaper: the pool reaper is not ported"
      end

      private

      def refuse_unconnectable!
        raise @unconnectable if @unconnectable
      end
    end
  end

  class Base
    class << self
      # core.rb: `class_attribute :default_role, :default_shard`; reading
      # walks to the nearest class that assigned one.
      def default_role
        klass = self
        while klass
          return klass.instance_variable_get(:@default_role) if klass.instance_variable_defined?(:@default_role)
          klass = klass.superclass
        end
        ActiveRecord.writing_role
      end

      def default_role=(role)
        @default_role = role
      end

      def default_shard
        klass = self
        while klass
          return klass.instance_variable_get(:@default_shard) if klass.instance_variable_defined?(:@default_shard)
          klass = klass.superclass
        end
        :default
      end

      def default_shard=(shard)
        @default_shard = shard
      end

      # core.rb: `configurations` (Base's alone; a subclass reads Base's).
      def configurations=(config)
        ActiveRecord::Base.instance_variable_set(:@configurations, config)
      end

      def configurations
        ActiveRecord::Base.instance_variable_get(:@configurations) or
          raise ConnectionNotDefined, "ActiveRecord::Base.configurations: database.yml has not been resolved " \
            "(DatabaseConfiguration.configure! runs at boot)"
      end

      def default_connection_handler
        ActiveRecord::Base.instance_variable_get(:@default_connection_handler)
      end

      def default_connection_handler=(handler)
        ActiveRecord::Base.instance_variable_set(:@default_connection_handler, handler)
      end

      def connection_handler
        ConnectionHandling.state(:active_record_connection_handler) || default_connection_handler
      end

      def connection_handler=(handler)
        ConnectionHandling.set_state(:active_record_connection_handler, handler)
      end

      def application_record_class? # :nodoc:
        if ActiveRecord.application_record_class
          self == ActiveRecord.application_record_class
        else
          if defined?(::ApplicationRecord) && self == ::ApplicationRecord
            true
          end
        end
      end

      def current_role
        connected_to_stack.reverse_each do |hash|
          return hash[:role] if hash[:role] && hash[:klasses].include?(Base)
          return hash[:role] if hash[:role] && hash[:klasses].include?(connection_class_for_self)
        end

        default_role
      end

      def current_shard
        connected_to_stack.reverse_each do |hash|
          return hash[:shard] if hash[:shard] && hash[:klasses].include?(Base)
          return hash[:shard] if hash[:shard] && hash[:klasses].include?(connection_class_for_self)
        end

        default_shard
      end

      def current_preventing_writes
        connected_to_stack.reverse_each do |hash|
          return hash[:prevent_writes] if !hash[:prevent_writes].nil? && hash[:klasses].include?(Base)
          return hash[:prevent_writes] if !hash[:prevent_writes].nil? && hash[:klasses].include?(connection_class_for_self)
        end

        false
      end

      def preventing_writes?(class_name) # :nodoc:
        connected_to_stack.reverse_each do |hash|
          return hash[:prevent_writes] if !hash[:prevent_writes].nil? && hash[:klasses].include?(Base)
          return hash[:prevent_writes] if !hash[:prevent_writes].nil? && hash[:klasses].any? { |klass| klass.name == class_name }
        end

        false
      end

      def connected_to_stack # :nodoc:
        if (stack = ConnectionHandling.state(:active_record_connected_to_stack))
          stack
        else
          stack = []
          ConnectionHandling.set_state(:active_record_connected_to_stack, stack)
          stack
        end
      end

      def connection_class=(b) # :nodoc:
        @connection_class = b
      end

      def connection_class # :nodoc:
        @connection_class ||= false
      end

      def connection_class? # :nodoc:
        connection_class
      end

      def connection_class_for_self # :nodoc:
        klass = self

        until klass == Base
          break if klass.connection_class?
          klass = klass.superclass
        end

        klass
      end

      # connection_handling.rb
      def establish_connection(config_or_env = nil)
        config_or_env ||= DatabaseConfiguration.default_env.to_sym
        db_config = resolve_config_for_connection(config_or_env)
        connection_handler.establish_connection(db_config, owner_name: self, role: current_role, shard: current_shard)
      end

      def connects_to(database: {}, shards: {})
        raise NotImplementedError, "`connects_to` can only be called on ActiveRecord::Base or abstract classes" unless self == Base || abstract_class?

        if !database.empty? && !shards.empty?
          raise ArgumentError, "`connects_to` can only accept a `database` or `shards` argument, but not both arguments."
        end

        connections = []
        @shard_keys = shards.keys

        if shards.empty?
          shards[:default] = database
        end

        self.default_shard = shards.keys.first

        shards.each do |shard, database_keys|
          database_keys.each do |role, database_key|
            db_config = resolve_config_for_connection(database_key)

            self.connection_class = true
            shard = shard.to_sym unless shard.is_a? Integer
            connections << connection_handler.establish_connection(db_config, owner_name: self, role: role, shard: shard)
          end
        end

        connections
      end

      def connected_to(role: nil, shard: nil, prevent_writes: role == ActiveRecord.reading_role, &blk)
        if self != Base && !abstract_class
          raise NotImplementedError, "calling `connected_to` is only allowed on ActiveRecord::Base or abstract classes."
        end

        if !connection_class? && !primary_class?
          raise NotImplementedError, "calling `connected_to` is only allowed on the abstract class that established the connection."
        end

        unless role || shard
          raise ArgumentError, "must provide a `shard` and/or `role`."
        end

        if role == ActiveRecord.reading_role && !prevent_writes
          raise ArgumentError, "cannot set `prevent_writes` to false when `role` is `reading`."
        end

        with_role_and_shard(role, shard, prevent_writes, &blk)
      end

      def connected_to_many(*classes, role:, shard: nil, prevent_writes: role == ActiveRecord.reading_role)
        classes = classes.flatten

        if self != Base || classes.include?(Base)
          raise NotImplementedError, "connected_to_many can only be called on ActiveRecord::Base."
        end

        if role == ActiveRecord.reading_role && !prevent_writes
          raise ArgumentError, "cannot set `prevent_writes` to false when `role` is `reading`."
        end

        append_to_connected_to_stack(role: role, shard: shard, prevent_writes: prevent_writes, klasses: classes)
        begin
          yield
        ensure
          connected_to_stack.pop
        end
      end

      def connected_to_all_shards(role: nil, prevent_writes: role == ActiveRecord.reading_role, &blk)
        if shard_keys.empty?
          raise ArgumentError, "`connected_to_all_shards` cannot be called on a model that is not connected to any shards."
        end

        shard_keys.map do |shard|
          connected_to(shard: shard, role: role, prevent_writes: prevent_writes, &blk)
        end
      end

      def connecting_to(role: default_role, shard: default_shard, prevent_writes: role == ActiveRecord.reading_role)
        if role == ActiveRecord.reading_role && !prevent_writes
          raise ArgumentError, "cannot set `prevent_writes` to false when `role` is `reading`."
        end

        append_to_connected_to_stack(role: role, shard: shard, prevent_writes: prevent_writes, klasses: [self])
      end

      def prohibit_shard_swapping(enabled = true)
        prev_value = ConnectionHandling.state(:active_record_prohibit_shard_swapping) || Set.new
        ConnectionHandling.set_state(:active_record_prohibit_shard_swapping, prev_value)

        if enabled
          ConnectionHandling.set_state(:active_record_prohibit_shard_swapping, prev_value.dup.add(connection_specification_name))
        else
          ConnectionHandling.set_state(:active_record_prohibit_shard_swapping, prev_value.dup.delete(connection_specification_name))
        end
        yield
      ensure
        ConnectionHandling.set_state(:active_record_prohibit_shard_swapping, prev_value)
      end

      def shard_swapping_prohibited?
        ConnectionHandling.state(:active_record_prohibit_shard_swapping)&.include?(connection_specification_name)
      end

      def while_preventing_writes(enabled = true, &block)
        connected_to(role: current_role, prevent_writes: enabled, &block)
      end

      def connected_to?(role:, shard: ActiveRecord::Base.default_shard)
        current_role == role.to_sym && current_shard == shard.to_sym
      end

      def lease_connection
        connection_pool.lease_connection
      end

      # With no pool defined in the handler (database.yml not resolved, or
      # a program driving Db's own databases directly) the connection is
      # Db's adapter for the current database once one is configured, and
      # before that a per-thread adapter without a session, as this
      # runtime's quoting at boot has always used.
      def connection
        if connection_handler.connection_pool_names.empty?
          return Db.current_adapter if Db.configured?
          return Thread.current.thread_variable_get(:rh_postgresql_adapter) ||
            Thread.current.thread_variable_set(:rh_postgresql_adapter, ConnectionAdapters::PostgreSQLAdapter.new({}))
        end
        connection_pool.lease_connection
      end

      def release_connection
        connection_pool.release_connection
      end

      def with_connection(prevent_permanent_checkout: false, &block)
        connection_pool.with_connection(prevent_permanent_checkout: prevent_permanent_checkout, &block)
      end

      attr_writer :connection_specification_name

      def connection_specification_name
        if !instance_variable_defined?(:@connection_specification_name) || @connection_specification_name.nil?
          return self == Base ? Base.name : superclass.connection_specification_name
        end
        @connection_specification_name
      end

      def primary_class? # :nodoc:
        self == Base || application_record_class?
      end

      def connection_db_config
        connection_pool.db_config
      end

      def connection_pool
        if (reason = __rh_unresolved_connection)
          raise NotImplementedError, "#{name}.connection_pool: #{reason}"
        end
        connection_handler.retrieve_connection_pool(connection_specification_name, role: current_role, shard: current_shard, strict: true)
      end

      def retrieve_connection
        connection_handler.retrieve_connection(connection_specification_name, role: current_role, shard: current_shard)
      end

      def connected?
        connection_handler.connected?(connection_specification_name, role: current_role, shard: current_shard)
      end

      def remove_connection
        name = @connection_specification_name if defined?(@connection_specification_name)

        if connection_handler.retrieve_connection_pool(name, role: current_role, shard: current_shard)
          self.connection_specification_name = nil
        end

        connection_handler.remove_connection_pool(name, role: current_role, shard: current_shard)
      end

      def schema_cache # :nodoc:
        connection_pool.schema_cache
      end

      def shard_keys
        connection_class_for_self.instance_variable_get(:@shard_keys) || []
      end

      def sharded?
        shard_keys.any?
      end

      def clear_query_caches_for_current_thread
        nil
      end

      # A model whose connection the compiler could not resolve (a
      # computed or conditional `connects_to`, `connects_to shards:`, a
      # `connects_to` on a concrete class: DatabaseConfiguration::
      # UNRESOLVED) refuses at its first connection lookup rather than
      # run on another class's pool.
      def __rh_unresolved_connection # :nodoc:
        return nil unless defined?(::DatabaseConfiguration::UNRESOLVED) && name
        ::DatabaseConfiguration::UNRESOLVED[name]
      end

      private
        def resolve_config_for_connection(config_or_env)
          raise "Anonymous class is not allowed." unless name

          connection_name = primary_class? ? Base.name : name
          self.connection_specification_name = connection_name

          Base.configurations.resolve(config_or_env)
        end

        def with_role_and_shard(role, shard, prevent_writes)
          append_to_connected_to_stack(role: role, shard: shard, prevent_writes: prevent_writes, klasses: [self])
          begin
            return_value = yield
            return_value.load if return_value.is_a? ActiveRecord::Relation
            return_value
          ensure
            connected_to_stack.pop
          end
        end

        def append_to_connected_to_stack(entry)
          if (entry[:shard].nil? || entry[:shard] == "") == false && entry[:klasses].any?(&:shard_swapping_prohibited?)
            raise ShardSwapProhibitedError, "cannot swap `shard` while shard swapping is prohibited."
          end

          connected_to_stack << entry
        end
    end

    self.default_connection_handler = ConnectionAdapters::ConnectionHandler.new
  end

  # Model entry points that reach `Db` without naming their class run on
  # the class's pool. Prepended on Base (class and instance side) and on
  # Relation; `inherited` prepends the same onto every model, so a
  # lowered model's own `_adapter_*` primitives are routed too.
  module ConnectionRouting
    CLASS_METHODS = %i[
      _adapter_find_by_id _adapter_all _adapter_last _adapter_count _adapter_any? _adapter_exists_by_id?
      _adapter_truncate find_by where delete_all transaction update_counters upsert_all insert_all _hydrate_all
    ].freeze
    INSTANCE_METHODS = %i[_adapter_insert _adapter_update _adapter_delete _adapter_reload].freeze
    RELATION_METHODS = %i[
      load_records count sum group_count exists? probe_existence more_than? size delete_all update_all
      touch_all pluck ids find find_ids
    ].freeze
    ADAPTER_METHODS = %i[
      execute select_rows exec_query select_all exec_update exec_delete exec_insert begin_db_transaction
      begin_isolated_db_transaction commit_db_transaction exec_rollback_db_transaction
    ].freeze

    # Arguments pass through untouched (ruby2_keywords), so a wrapped
    # method sees exactly the call it would have seen.
    def self.wrapper(names, &resolve)
      Module.new do
        names.each do |name|
          define_method(name) do |*args, &block|
            ActiveRecord::ConnectionHandling.route(instance_exec(&resolve)) { super(*args, &block) }
          end
          ruby2_keywords(name)
        end
      end
    end

    ClassSide = wrapper(CLASS_METHODS) { self }
    InstanceSide = wrapper(INSTANCE_METHODS) { self.class }
    RelationSide = wrapper(RELATION_METHODS) { @model }

    # An adapter's statements run on the pool it was checked out of.
    AdapterSide = Module.new do
      ADAPTER_METHODS.each do |name|
        define_method(name) do |*args, &block|
          key = @rh_pg_database
          if key.nil? || Db.context[:name] == key
            super(*args, &block)
          else
            Db.with_database(key) { super(*args, &block) }
          end
        end
        ruby2_keywords(name)
      end

      # Rails' check_if_write_query (abstract/database_statements.rb),
      # before every statement the adapter sends: a write while the
      # adapter's connection class prevents writes is a ReadOnlyError.
      def raw_execute(sql, *args, **kwargs, &block)
        if preventing_writes? && write_query?(sql)
          raise ActiveRecord::ReadOnlyError, "Write query attempted while in readonly mode: #{sql}"
        end
        super
      end
    end
  end

  class Base
    singleton_class.prepend(ConnectionRouting::ClassSide)
    prepend(ConnectionRouting::InstanceSide)

    class << self
      prepend(Module.new do
        def inherited(subclass)
          super
          subclass.singleton_class.prepend(ConnectionRouting::ClassSide)
          subclass.prepend(ConnectionRouting::InstanceSide)
        end
      end)
    end
  end

  class Relation
    prepend(ConnectionRouting::RelationSide)
  end if defined?(Relation)

  module ConnectionAdapters
    class PostgreSQLAdapter
      prepend(ConnectionRouting::AdapterSide)

      # postgresql/database_statements.rb READ_QUERY over
      # abstract/database_statements.rb DEFAULT_READ_QUERY and
      # build_read_query_regexp.
      COMMENT_REGEX = %r{(?:--.*\n)|/\*(?:[^*]|\*[^/])*\*/}
      READ_QUERY = /\A(?:[(\s]|#{COMMENT_REGEX})*#{Regexp.union(*%i[close declare fetch move set show begin commit explain release rollback savepoint select with].map { |part| /#{part}/i })}/

      def write_query?(sql) # :nodoc:
        !READ_QUERY.match?(sql)
      rescue ArgumentError # Invalid encoding
        !READ_QUERY.match?(sql.b)
      end

      # AbstractAdapter#pool: the pool this adapter was checked out of.
      def pool
        ConnectionHandling.pools_by_key[@rh_pg_database]
      end

      def replica?
        @config.is_a?(Hash) && (@config[:replica] || @config["replica"]) ? true : false
      end

      # AbstractAdapter#connection_descriptor / #preventing_writes?.
      def connection_descriptor
        pool&.connection_descriptor
      end

      def preventing_writes?
        return true if replica?
        return false if connection_descriptor.nil?

        connection_descriptor.current_preventing_writes
      end
    end
  end
end

# Transactions (TransactionManager, savepoints, `Base.transaction`).
require_relative "active_record_transactions"

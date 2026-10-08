# ActiveRecord::TestFixtures' transactional tests, ported from the Rails
# native Core runs (Shopify/rails 5813559f4aed, 8.2.0.alpha,
# activerecord/lib/active_record/test_fixtures.rb): with
# `use_transactional_tests` (true unless the test class sets it false),
# every writing pool of the connection handler is pinned to one
# connection inside a non-joinable transaction begun before the test's
# setup callbacks, as is a pool established while the test runs (the
# `!connection.active_record` subscription); teardown rolls each back and
# returns the connection, then clears every active connection.
#
# The runner (minitest_runner.rb) calls the two hooks below where Rails'
# before_setup and after_teardown run setup_fixtures and
# teardown_fixtures: after this tree's fixture load and reset
# (`__roundhouse_setup_fixtures`), and after the test's teardown and
# after_teardown.
#
# Fixtures load before every test here (SchemaSetup.reset!), so Rails'
# loaded-fixture cache (`@@already_loaded_fixtures`, cleared when a
# transaction was not clean) has nothing to keep. `pre_loaded_fixtures`
# and `use_instantiated_fixtures` are not ported. A class's
# `self.use_transactional_tests = <literal>` compiles to its class reader.
#
# Active Record autoloads the module in any process, so the PostgreSQL
# boot (runtime/database_configuration.rb) requires it; the test helper
# includes it into TestBase, as rails/test_help.rb does.
module ActiveRecord
  module TestFixtures
    module ClassMethods
      def use_transactional_tests
        true
      end

      def lock_threads
        true
      end

      def database_transactions_config
        return @database_transactions_config if instance_variable_defined?(:@database_transactions_config)
        superclass.respond_to?(:database_transactions_config) ? superclass.database_transactions_config : {}
      end

      def database_transactions_config=(config)
        @database_transactions_config = config
      end

      def skip_transactional_tests_for_database(database_name)
        use_transactional_tests_for_database(database_name, false)
      end

      def use_transactional_tests_for_database(database_name, enabled = true)
        self.database_transactions_config = database_transactions_config.merge(database_name => enabled)
      end

      def uses_transaction(*methods)
        @uses_transaction = [] unless defined?(@uses_transaction)
        @uses_transaction.concat methods.map(&:to_s)
      end

      def uses_transaction?(method)
        @uses_transaction = [] unless defined?(@uses_transaction)
        @uses_transaction.include?(method.to_s)
      end
    end

    def self.included(base)
      base.extend(ClassMethods)
    end

    def use_transactional_tests
      self.class.use_transactional_tests
    end

    def lock_threads
      self.class.lock_threads
    end

    def database_transactions_config
      self.class.database_transactions_config
    end

    # setup_fixtures' transactional half, for the test method `name`.
    def __roundhouse_setup_transactional_fixtures(name) # :nodoc:
      @__roundhouse_fixture_test_name = name
      @fixture_connection_pools = []
      @connection_subscriber = nil
      @saved_pool_configs = Hash.new { |hash, key| hash[key] = {} }
      setup_transactional_fixtures if run_in_transaction?
    end

    # teardown_fixtures.
    def __roundhouse_teardown_fixtures(name) # :nodoc:
      if instance_variable_defined?(:@fixture_connection_pools) && @fixture_connection_pools && run_in_transaction?
        teardown_transactional_fixtures
      end
    ensure
      ActiveRecord::Base.connection_handler.clear_active_connections!(:all)
    end

    private
      def run_in_transaction?
        has_explicit_config = database_transactions_config.any? { |_, enabled| enabled }
        (use_transactional_tests || has_explicit_config) &&
          !self.class.uses_transaction?(@__roundhouse_fixture_test_name)
      end

      def transactional_tests_for_pool?(pool)
        database_transactions_config.fetch(pool.db_config.name.to_sym, use_transactional_tests)
      end

      def setup_transactional_fixtures
        setup_shared_connection_pool

        pools = ActiveRecord::Base.connection_handler.connection_pool_list(:writing)

        pools.select! { |pool| transactional_tests_for_pool?(pool) }

        pools.each do |pool|
          pool.pin_connection!(lock_threads)
          @fixture_connection_pools << pool
          pool.lease_connection
        end

        @connection_subscriber = ActiveSupport::Notifications.subscribe("!connection.active_record") do |_, _, _, _, payload|
          connection_name = payload[:connection_name] if payload.key?(:connection_name)
          shard = payload[:shard] if payload.key?(:shard)

          if connection_name
            pool = ActiveRecord::Base.connection_handler.retrieve_connection_pool(connection_name, shard: shard)
            if pool
              setup_shared_connection_pool

              if !@fixture_connection_pools.include?(pool) && transactional_tests_for_pool?(pool)
                pool.pin_connection!(lock_threads)
                @fixture_connection_pools << pool
                pool.lease_connection
              end
            end
          end
        end
      end

      def teardown_transactional_fixtures
        ActiveSupport::Notifications.unsubscribe(@connection_subscriber) if @connection_subscriber

        # Rails clears its loaded-fixture cache unless every pin was
        # clean; fixtures load before every test here.
        @fixture_connection_pools.map(&:unpin_connection!)
        @fixture_connection_pools.clear
        teardown_shared_connection_pool
      end

      def setup_shared_connection_pool
        handler = ActiveRecord::Base.connection_handler

        handler.connection_pool_names.each do |name|
          pool_manager = handler.send(:connection_name_to_pool_manager)[name]
          pool_manager.shard_names.each do |shard_name|
            writing_pool_config = pool_manager.get_pool_config(ActiveRecord.writing_role, shard_name)
            @saved_pool_configs[name][shard_name] ||= {}
            pool_manager.role_names.each do |role|
              next unless pool_config = pool_manager.get_pool_config(role, shard_name)
              next if pool_config == writing_pool_config

              @saved_pool_configs[name][shard_name][role] = pool_config
              pool_manager.set_pool_config(role, shard_name, writing_pool_config)
            end
          end
        end
      end

      def teardown_shared_connection_pool
        handler = ActiveRecord::Base.connection_handler
        removed_pool_configs = []
        pool_managers = handler.send(:connection_name_to_pool_manager)

        @saved_pool_configs.each_pair do |name, shards|
          pool_manager = pool_managers[name]
          shards.each_pair do |shard_name, roles|
            roles.each_pair do |role, pool_config|
              if pool_manager.get_pool_config(role, shard_name)
                pool_manager.set_pool_config(role, shard_name, pool_config)
              else
                removed_pool_configs << pool_config
              end
            end
          end
        end

        @saved_pool_configs.clear

        remaining_pool_configs = pool_managers.values.flat_map(&:pool_configs)
        removed_pool_configs.compact.uniq.each do |pool_config|
          pool_config.disconnect! unless remaining_pool_configs.include?(pool_config)
        end
      end
  end
end

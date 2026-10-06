# frozen_string_literal: true

# Boot-time database configuration, resolved the way Rails 8.1.4 resolves
# it, from the app's own config/database.yml:
#
#   1. render: the template's ERB is compiled app code (the emitted
#      `DatabaseYml.render`), run here at boot as Rails runs
#      `ERB.new(content).result` (ActiveSupport::ConfigurationFile#render);
#   2. parse: `YAML.unsafe_load` (aliases, anchors and `<<` merges, Psych's
#      own semantics), ActiveSupport::ConfigurationFile#parse;
#   3. shared: the top-level `shared:` key, Rails::Application::
#      Configuration#database_configuration (railties 8.1.4,
#      configuration.rb:453-488);
#   4. resolve: ActiveRecord::DatabaseConfigurations#build_configs and
#      friends (database_configurations.rb), HashConfig (hash_config.rb),
#      UrlConfig (url_config.rb) and ConnectionUrlResolver
#      (connection_url_resolver.rb), including `DATABASE_URL` /
#      `<NAME>_DATABASE_URL` merging.
#
# Every key of every configuration is kept (nested hashes such as
# `semian:` stay string-keyed, as Rails' shallow `symbolize_keys` leaves
# them). Anything this port does not carry is a DatabaseConfigurations::
# Error naming the construct; nothing is dropped. Error messages never
# include configuration values, so a credential cannot reach a log.
require "yaml"
require "uri"
require "pathname"

module DatabaseConfigurations
  # A named boot-time refusal. `construct` is stable and groupable.
  class Error < StandardError
    attr_reader :construct

    def initialize(construct, message)
      @construct = construct
      super("#{construct}: #{message}")
    end
  end

  # ActiveRecord::DatabaseConfigurations::InvalidConfigurationError.
  class InvalidConfigurationError < Error
    def initialize(message)
      super("database_configuration_invalid", message)
    end
  end

  # ActiveRecord.protocol_adapters' defaults (active_record.rb:530-536).
  PROTOCOL_ADAPTERS = { "sqlite" => "sqlite3", "mysql" => "mysql2", "postgres" => "postgresql" }.freeze

  # ActiveSupport's Object#blank?, for the values a configuration holds.
  def self.blank?(value)
    case value
    when nil, false then true
    when String then value.match?(/\A[[:space:]]*\z/)
    else value.respond_to?(:empty?) ? !!value.empty? : false
    end
  end

  # ActiveSupport's Hash#symbolize_keys: shallow, keys that cannot become
  # symbols stay as they are.
  def self.symbolize_keys(hash)
    hash.transform_keys { |key| key.respond_to?(:to_sym) ? key.to_sym : key }
  end

  # ActiveRecord::DatabaseConfigurations::HashConfig (hash_config.rb).
  class HashConfig
    attr_reader :env_name, :name, :configuration_hash

    def initialize(env_name, name, configuration_hash)
      @env_name = env_name
      @name = name
      @configuration_hash = DatabaseConfigurations.symbolize_keys(configuration_hash).freeze
      validate_configuration!
    end

    def adapter
      configuration_hash[:adapter]&.to_s
    end

    def replica?
      configuration_hash[:replica]
    end

    def database_tasks?
      !replica? && !!configuration_hash.fetch(:database_tasks, true)
    end

    def migrations_paths
      configuration_hash[:migrations_paths]
    end

    private

    # hash_config.rb:221-234.
    def validate_configuration!
      if configuration_hash[:pool] && configuration_hash[:max_connections]
        pool_val = configuration_hash[:pool].to_i
        max_conn_val = configuration_hash[:max_connections].to_i
        if pool_val != max_conn_val
          raise Error.new("database_configuration_invalid", "Ambiguous configuration: 'pool' (#{pool_val}) and 'max_connections' (#{max_conn_val}) are set to different values. Prefer just 'max_connections'.")
        end
      end
      if configuration_hash[:pool] && configuration_hash[:min_connections]
        raise Error.new("database_configuration_invalid", "Ambiguous configuration: when setting 'min_connections', use 'max_connections' instead of 'pool'.")
      end
    end
  end

  # ActiveRecord::DatabaseConfigurations::UrlConfig (url_config.rb): the
  # URL's keys win over the hash's, every other key is kept.
  class UrlConfig < HashConfig
    attr_reader :url

    def initialize(env_name, name, url, configuration_hash = {})
      super(env_name, name, configuration_hash)
      @url = url
      @configuration_hash = @configuration_hash.merge(build_url_hash)
      @configuration_hash[:schema_dump] = false if @configuration_hash[:schema_dump] == "false"
      query_cache = parse_query_cache
      @configuration_hash[:query_cache] = query_cache unless query_cache.nil?
      to_boolean!(@configuration_hash, :replica)
      to_boolean!(@configuration_hash, :database_tasks)
      to_boolean!(@configuration_hash, :seeds)
      to_boolean!(@configuration_hash, :use_metadata_table)
      @configuration_hash.freeze
    end

    private

    def parse_query_cache
      case value = @configuration_hash[:query_cache]
      when /\A\d+\z/ then value.to_i
      when "false" then false
      else value
      end
    end

    def to_boolean!(configuration_hash, key)
      if configuration_hash[key].is_a?(String)
        configuration_hash[key] = configuration_hash[key] != "false"
      end
    end

    def build_url_hash
      if url.nil? || url.start_with?("jdbc:", "http:", "https:")
        { url: url }
      else
        ConnectionUrlResolver.new(url).to_hash
      end
    end
  end

  # ActiveRecord::DatabaseConfigurations::ConnectionUrlResolver.
  class ConnectionUrlResolver
    def initialize(url)
      raise Error.new("database_url", "Database URL cannot be empty") if DatabaseConfigurations.blank?(url)
      begin
        @uri = uri_parser.parse(url)
      rescue URI::Error
        # URI's message quotes the URL, which may carry a password.
        raise Error.new("database_url", "a database URL does not parse as a URI")
      end
      @adapter = resolved_adapter
      if @uri.opaque
        @uri.opaque, @query = @uri.opaque.split("?", 2)
      else
        @query = @uri.query
      end
    end

    def to_hash
      config = raw_config.reject { |_key, value| DatabaseConfigurations.blank?(value) }
      config.map { |key, value| config[key] = uri_parser.unescape(value) if value.is_a?(String) }
      config
    end

    private

    attr_reader :uri

    def uri_parser
      @uri_parser ||= URI::RFC2396_Parser.new
    end

    def query_hash
      DatabaseConfigurations.symbolize_keys(Hash[(@query || "").split("&").map { |pair| pair.split("=", 2) }])
    end

    def raw_config
      if uri.opaque
        query_hash.merge(adapter: @adapter, database: uri.opaque)
      else
        # `query_hash.reverse_merge(...)`: the query's keys win.
        { adapter: @adapter, username: uri.user, password: uri.password, port: uri.port,
          database: database_from_path, host: uri.hostname }.merge(query_hash)
      end
    end

    def resolved_adapter
      adapter = uri.scheme && @uri.scheme.tr("-", "_")
      adapter = PROTOCOL_ADAPTERS[adapter] if adapter && PROTOCOL_ADAPTERS[adapter]
      adapter
    end

    def database_from_path
      @adapter == "sqlite3" ? uri.path : uri.path.delete_prefix("/")
    end
  end

  # ActiveRecord::DatabaseConfigurations (database_configurations.rb),
  # over an explicit environment name and ENV, without a Base.
  class Configurations
    attr_reader :configurations, :default_env

    def initialize(configurations, default_env:, environment: ENV)
      @default_env = default_env.to_s
      @environment = environment
      @configurations = build_configs(configurations)
    end

    def configs_for(env_name: nil, name: nil, config_key: nil, include_hidden: false)
      env_name ||= default_env if name
      configs = env_name ? configurations.select { |c| c.env_name == env_name } : configurations
      configs = configs.select(&:database_tasks?) unless include_hidden
      configs = configs.select { |c| c.configuration_hash.key?(config_key) } if config_key
      name ? configs.find { |c| c.name == name.to_s } : configs
    end

    def find_db_config(env)
      env = env.to_s
      configurations.find { |c| for_current_env?(c) && (c.env_name == env || c.name == env) } ||
        configurations.find { |c| c.env_name == env }
    end

    def primary?(name)
      return true if name == "primary"
      first_config = find_db_config(default_env)
      first_config && name == first_config.name
    end

    private

    def for_current_env?(config)
      config.env_name == default_env
    end

    def build_configs(configs)
      db_configs = configs.flat_map do |env_name, config|
        if config.is_a?(Hash) && config.values.all?(Hash)
          config.map { |name, sub_config| build_db_config_from_raw_config(env_name.to_s, name.to_s, sub_config) }
        else
          build_db_config_from_raw_config(env_name.to_s, "primary", config)
        end
      end
      unless db_configs.find { |c| c && for_current_env?(c) }
        db_configs << environment_url_config(default_env, "primary", {})
      end
      merge_db_environment_variables(default_env, db_configs.compact)
    end

    def build_db_config_from_raw_config(env_name, name, config)
      case config
      when String
        uri = begin
          URI.parse(config)
        rescue URI::Error
          nil
        end
        if uri&.scheme
          UrlConfig.new(env_name, name, config)
        else
          # Rails' message quotes the value; a URL may carry a password.
          raise InvalidConfigurationError, "'{ #{env_name} => <String> }' (configuration #{name}) is not a valid configuration. Expected a URL string or a Hash."
        end
      when Hash
        config = DatabaseConfigurations.symbolize_keys(config)
        url = config[:url]
        config_without_url = config.dup
        config_without_url.delete(:url)
        url ? UrlConfig.new(env_name, name, url, config_without_url) : HashConfig.new(env_name, name, config_without_url)
      else
        raise InvalidConfigurationError, "'{ #{env_name} => <#{config.class}> }' (configuration #{name}) is not a valid configuration. Expected a URL string or a Hash."
      end
    end

    def merge_db_environment_variables(current_env, configs)
      configs.map do |config|
        next config if config.is_a?(UrlConfig) || config.env_name != current_env
        environment_url_config(current_env, config.name, config.configuration_hash) || config
      end
    end

    def environment_url_config(env, name, config)
      url = environment_value_for(name)
      return unless url
      UrlConfig.new(env, name, url, config)
    end

    def environment_value_for(name)
      url = @environment["#{name.upcase}_DATABASE_URL"]
      url ||= @environment["DATABASE_URL"] if name == "primary"
      url
    end
  end

  module_function

  # ActiveSupport::ConfigurationFile#parse over the rendered template.
  # `path` is the app-relative file, for messages.
  def parse(source, path)
    YAML.unsafe_load(source) || {}
  rescue Psych::SyntaxError => error
    raise Error.new("database_yml_syntax", "YAML syntax error occurred while parsing #{path}. " \
      "Please note that YAML must be consistently indented using spaces. Tabs are not allowed. " \
      "Error: #{error.message}")
  rescue Psych::BadAlias => error
    # Rails lets this through ConfigurationFile; `load` prefixes it as
    # `database_configuration` does.
    raise Error.new("database_yml_alias", error.message)
  end

  # Rails::Application::Configuration#database_configuration: the top-level
  # `shared:` configuration reverse-merged into every environment.
  def database_configuration(loaded_yaml, path)
    unless loaded_yaml.is_a?(Hash)
      raise Error.new("database_yml_shape", "#{path} is a #{loaded_yaml.class}, not a mapping of environments")
    end
    if (shared = loaded_yaml.delete("shared"))
      loaded_yaml.each do |_env, config|
        if config.is_a?(Hash) && config.values.all?(Hash)
          if shared.is_a?(Hash) && shared.values.all?(Hash)
            config.map { |name, sub_config| reverse_merge!(sub_config, shared[name]) if shared[name] }
          else
            config.map { |_name, sub_config| reverse_merge!(sub_config, shared) }
          end
        else
          reverse_merge!(config, shared)
        end
      end
    end
    Hash.new(shared).merge(loaded_yaml)
  end

  # ActiveSupport's Hash#reverse_merge!: `replace(other.merge(self))`.
  def reverse_merge!(hash, other)
    unless hash.respond_to?(:replace) && other.respond_to?(:merge)
      raise Error.new("database_yml_shared", "`shared:` cannot be merged into a #{hash.class}")
    end
    hash.replace(other.merge(hash))
  end

  # Render, parse and resolve. `renderer` responds to `render` (the
  # compiled template) or is nil when the app has no config/database.yml,
  # in which case Rails resolves from DATABASE_URL alone or refuses.
  def load(renderer:, path: "config/database.yml", env:, environment: ENV)
    raw = begin
      if renderer
        database_configuration(parse(renderer.render, path), path)
      elsif environment["DATABASE_URL"]
        {}
      else
        raise Error.new("database_yml_missing", "Could not load database configuration. No such file - [\"#{path}\"]")
      end
    rescue => e
      # railties configuration.rb:485: same class (and construct), prefixed.
      raise e, "Cannot load database configuration:\n#{e.message}", e.backtrace
    end
    Configurations.new(raw, default_env: env, environment: environment)
  end

  # The one database the emitted runtime opens: the environment's
  # configuration that Rails calls primary and that is not a replica, as
  # src/ingest/database_manifest.rs counts it. Anything else is refused.
  def runtime_primary(configurations)
    env = configurations.default_env
    all = configurations.configs_for(env_name: env, include_hidden: true)
    candidates = all.select { |c| configurations.primary?(c.name) && !c.replica? }
    return candidates.first if candidates.size == 1
    raise Error.new("database_primary", "environment `#{env}` has #{candidates.size} primary, non-replica configuration(s) " \
      "(#{all.map(&:name).join(", ")}); the emitted runtime opens exactly one")
  end

  # The facts a manifest records per configuration
  # (scripts/rails-database-manifest.rb `configuration`), from the
  # boot-resolved configurations: credential-free by construction.
  def identity(configurations)
    configurations.configs_for(env_name: configurations.default_env, include_hidden: true).map do |c|
      hash = c.configuration_hash
      {
        "name" => c.name,
        "adapter" => c.adapter.to_s,
        "primary" => configurations.primary?(c.name) ? true : false,
        "replica" => c.replica? ? true : false,
        "database_tasks" => c.database_tasks? ? true : false,
        "schema_search_path" => hash[:schema_search_path],
        # As src/ingest/database_manifest.rs carries them: scalars as text.
        "pragmas" => (hash[:pragmas] || {}).to_h { |k, v| [k.to_s, v.to_s] },
        "migrations_paths" => Array(c.migrations_paths).map { |p| Pathname.new(p.to_s).cleanpath.to_s },
      }
    end
  end

  # Compare the boot-resolved identity with a manifest's recorded one;
  # refuse on any difference. Values compared are manifest fields only.
  def cross_check!(configurations, manifest)
    resolved = identity(configurations)
    names = resolved.map { |c| c["name"] }
    expected = manifest.map { |c| c["name"] }
    unless names == expected
      raise Error.new("database_manifest_mismatch", "database.yml resolves configurations #{names.inspect} for " \
        "`#{configurations.default_env}`; the manifest records #{expected.inspect}")
    end
    manifest.each do |recorded|
      actual = resolved.find { |c| c["name"] == recorded["name"] }
      recorded.each do |key, value|
        next if actual[key] == value
        raise Error.new("database_manifest_mismatch", "configuration `#{recorded["name"]}` resolves #{key} " \
          "#{actual[key].inspect} at boot; the manifest records #{value.inspect}")
      end
    end
    nil
  end

  # libpq keywords the harness endpoint may name, as the Active Record
  # names a database.yml entry uses (Rails' PostgreSQLAdapter#initialize,
  # postgresql_adapter.rb:334-347, maps them back to libpq).
  ENDPOINT_NAMES = { user: :username, dbname: :database }.freeze

  # The harness's explicit endpoint (ROUNDHOUSE_DATABASE_URL: a libpq
  # connection string or URI) over a resolved entry. As Rails merges
  # DATABASE_URL into a configuration (url_config.rb `merge(build_url_hash)`),
  # every key the endpoint names wins and every other key is kept; a key
  # the entry spells either way (`username`/`user`) is replaced in both.
  def apply_endpoint(entry, endpoint)
    named = PG::Connection.conninfo_parse(endpoint.to_s).each_with_object({}) do |option, out|
      out[ENDPOINT_NAMES.fetch(option[:keyword].to_sym, option[:keyword].to_sym)] = option[:val] if option[:val]
    end
    replaced = ENDPOINT_NAMES.select { |_libpq, rails| named.key?(rails) }.keys
    entry.reject { |key, _| replaced.include?(key) }.merge(named)
  rescue PG::Error
    raise Error.new("database_endpoint", "ROUNDHOUSE_DATABASE_URL is not a libpq connection string or URI")
  end

  # semian's ActiveRecord PostgreSQL adapter, prepended onto the adapter
  # (runtime/semian_activerecord_postgresql_adapter.rb).
  def semian_adapter_loaded?
    !!(defined?(::Semian::ActiveRecordPostgreSQLAdapter) &&
      defined?(::ActiveRecord::ConnectionAdapters::PostgreSQLAdapter) &&
      ::ActiveRecord::ConnectionAdapters::PostgreSQLAdapter.ancestors.include?(::Semian::ActiveRecordPostgreSQLAdapter))
  end

  # Everything boot decides before connecting: render, parse and resolve
  # `env`; cross-check a manifest's recorded facts when the app has one;
  # pick the runtime primary; check it names the compiled adapter and that
  # every model's `connects_to` role resolves (`ownership`: [class,
  # writing, reading]). Returns [configurations, primary HashConfig].
  def resolve(renderer:, env:, compiled_adapter:, path: "config/database.yml", manifest: nil, ownership: [], environment: ENV)
    configurations = load(renderer: renderer, path: path, env: env, environment: environment)
    cross_check!(configurations, manifest) if manifest
    primary = runtime_primary(configurations)
    unless primary.adapter == compiled_adapter
      raise Error.new("database_adapter", "configuration `#{primary.name}` for `#{env}` uses adapter " \
        "#{primary.adapter.inspect}; the program is compiled for #{compiled_adapter.inspect}")
    end
    ownership.each do |klass, writing, reading|
      [writing, reading].compact.each do |role|
        next if configurations.find_db_config(role)
        # ActiveRecord::DatabaseConfigurations#resolve_symbol_connection.
        raise Error.new("connects_to", "The `#{role}` database is not configured for the `#{env}` environment " \
          "(`connects_to` on #{klass}).")
      end
      next if writing == primary.name
      raise Error.new("connects_to", "models under #{klass} write to `#{writing}`; the emitted runtime opens only " \
        "the primary configuration `#{primary.name}`")
    end
    [configurations, primary]
  end

  # The entry Db.configure_database receives: the primary's whole resolved
  # hash with the harness endpoint over it. `semian:` is honoured only by
  # semian's adapter, which must be loaded first; carried without it, the
  # key would be silently ignored, so that is refused.
  def connection_entry(primary, endpoint)
    entry = primary.configuration_hash
    if entry.key?(:semian) && !semian_adapter_loaded?
      raise Error.new("semian_adapter", "configuration `#{primary.name}` carries `semian:`, and semian's " \
        "ActiveRecord PostgreSQL adapter (`require \"semian/activerecord_postgresql_adapter\"`) is not loaded " \
        "before the database is configured")
    end
    if endpoint.nil? || endpoint.empty?
      raise Error.new("database_endpoint", "ROUNDHOUSE_DATABASE_URL must name the local PostgreSQL database")
    end
    apply_endpoint(entry, endpoint)
  end
end

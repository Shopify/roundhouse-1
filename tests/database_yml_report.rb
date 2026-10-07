# Shared report shape for tests/database_yml_boot.rs: the same JSON from
# the native Rails Core runs (tests/database_yml_oracle.rb) and from the emitted
# program's boot-time resolution. Plain Ruby; it only calls the methods
# both ActiveRecord::DatabaseConfigurations and the emitted
# DatabaseConfigurations::Configurations answer.
require "json"

module DatabaseYmlReport
  module_function

  def config(configurations, c)
    {
      "env_name" => c.env_name,
      "name" => c.name,
      "class" => c.class.name.split("::").last,
      "url" => c.respond_to?(:url) && !c.url.nil?,
      "primary" => configurations.primary?(c.name) ? true : false,
      "replica" => c.replica? ? true : false,
      "database_tasks" => c.database_tasks? ? true : false,
      # `inspect` keeps what JSON cannot: symbol vs string keys, Integer
      # vs String, Float.
      "inspect" => c.configuration_hash.inspect,
      "hash" => c.configuration_hash,
    }
  end

  def report(configurations, env)
    {
      "env" => env,
      "configurations" => configurations.configurations.map { |c| config(configurations, c) },
      "configs_for" => configurations.configs_for(env_name: env).map(&:name),
      "find_db_config" => configurations.find_db_config(env)&.name,
    }
  end

  def error(e)
    { "error" => e.class.name, "message" => e.message }
  end
end

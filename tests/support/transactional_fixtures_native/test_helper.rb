# The native side's test_helper for tests/transactional_fixtures_pg_runtime.rb:
# Core's pinned Rails 8.2 with the probe app's config/database.yml as
# Base.configurations (each entry given the run's host/port/user from
# PROBE_PG_*), the app's models, and ActiveRecord::TestFixtures in
# ActiveSupport::TestCase, as rails/test_help includes it.
ENV["RAILS_ENV"] = "test"
require_relative "../rails_oracle"
RailsOracle.activate("activesupport", "activemodel", "activerecord")
require "active_record"
require "active_support/test_case"
require "active_record/fixtures"
require "minitest/autorun"
require "erb"
require "yaml"

root = ENV.fetch("PROBE_APP_ROOT")
raw = YAML.unsafe_load(ERB.new(File.read(File.join(root, "config/database.yml"))).result)
endpoint = { "host" => ENV["PROBE_PG_HOST"], "port" => ENV["PROBE_PG_PORT"], "username" => ENV["PROBE_PG_USER"] }.compact
raw.each_value { |configs| configs.each_value { |c| c.merge!(endpoint) if c.is_a?(Hash) } }
ActiveRecord::Base.configurations = raw
ActiveRecord::Base.establish_connection
%w[application_record animals_record post comment dog].each { |m| require File.join(root, "app/models", m) }
ActiveSupport::TestCase.include(ActiveRecord::TestFixtures)

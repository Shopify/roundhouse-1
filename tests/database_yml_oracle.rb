# Native Rails oracle for tests/database_yml_boot.rs: the Rails native Core
# runs (tests/support/rails_oracle.rb; RH_RAILS_ORACLE overrides it). Only
# this script loads Rails; the emitted program never does.
#
#   ruby tests/database_yml_oracle.rb APP_ROOT [LIB_FILE ...]
#
# Loads each LIB_FILE plainly (the code the template's ERB calls), then
# reads APP_ROOT/config/database.yml exactly as a booting app does:
# Rails::Application::Configuration#database_configuration
# (ActiveSupport::ConfigurationFile: ERB.new(content).result, then
# YAML.unsafe_load; the `shared:` merge) and
# ActiveRecord::DatabaseConfigurations.new over it, for Rails.env
# (RAILS_ENV). No application is booted.
require_relative "support/rails_oracle"
RailsOracle.activate("activesupport", "activerecord", "railties")
require "active_support"
require "active_record"
require "rails"
require "rails/application/configuration"
require "active_record/database_configurations"
require_relative "database_yml_report"

root = Pathname.new(ARGV.fetch(0))
ARGV.drop(1).each { |lib| load root.join(lib).to_s }
env = Rails.env.to_s
out = begin
  raw = Rails::Application::Configuration.new(root).database_configuration
  DatabaseYmlReport.report(ActiveRecord::DatabaseConfigurations.new(raw), env)
rescue Exception => e
  DatabaseYmlReport.error(e)
end
puts JSON.generate(out)

# typed: strict
# frozen_string_literal: true

# Be sure to restart your server when you modify this file.

# The default rails silencer would ignore everything in components
Rails.backtrace_cleaner.remove_silencers!
Rails.backtrace_cleaner.remove_filters! if ENV["BACKTRACE"]
# rails filters before silencing, so we need to filter gem paths like "pry (0.10.4) lib/pry"...
Rails.backtrace_cleaner.add_silencer { |line| line.match?(/\A\S+ \(\S+\) \S+/) }
# With tectonix, each gem gets its own /nix/store/ path that isn't in Gem.path,
# so the default Rails gem filter can't transform them. Silence these raw nix paths directly.
Rails.backtrace_cleaner.add_silencer { |line| line.start_with?("/nix/store/") }
# filter lines for -e argument, e.g. "-e:1:in `<main>'"
Rails.backtrace_cleaner.add_silencer { |line| line.start_with?("-e:") }

# You can add backtrace silencers for libraries that you're using but don't wish to see in your backtraces.
Rails.backtrace_cleaner.add_silencer { |line| line.include?("app/utils/mutation_guard") }
Rails.backtrace_cleaner.add_silencer { |line| line.include?("lib/patches/job_callback_order.rb") }
Rails.backtrace_cleaner.add_silencer { |line| line.match?(%r{test/support/helpers/pitchfork_test_helper.rb.*run'}) }

instrumentation = Regexp.union(
  "lib/instrumentation",
  "checkouts/one/instrumentation",
  "checkout_platform/instrumentation",
  "service_proxy/instrumentation",
  "platform/lib/maintenance/maintenance_task_instrumentation.rb",
  "components/platform/essentials/lib/patches/active_record/mysql_monitoring.rb",
  "components/platform/essentials/lib/patches/active_record/retry.rb",
  "components/platform/essentials/lib/patches/active_record/deadline.rb",
  "components/platform/pagination/app/models/pagination/sql_pagination_backend.rb",
  "components/platform/pagination/app/models/pagination/paginator.rb",
  "components/platform/graph_model/app/graphql/graph_api/active_record_loader",
)

middleware = Regexp.union(
  "platform/essentials/lib/middleware",
  "components/platform/essentials/lib/podding",
  "components/platform/essentials/app/utils/podding",
)

noisy_components = Regexp.union(
  instrumentation,
  middleware,
  "graph_api/field.rb", # Complex GraphQL objects spam a lot of these
  "graph_api/query.rb",
  "graph_api/object_type.rb",
)
Rails.backtrace_cleaner.add_silencer { |line| line.match(noisy_components) }

ActiveSupport.on_load(:active_record) do
  # Used for verbose query logging in development or ?debug=true requests
  ActiveRecord::LogSubscriber.backtrace_cleaner = Rails.backtrace_cleaner.dup
  skipped_lines = Regexp.union(
    "config/initializers/filter_query_logging.rb",
    "lib/semian",
    "lib/patches/active_record",
    "app/models/concerns/accelerated_belongs_to.rb",
    "app/utils/deprecated_ar_transaction_changes.rb",
    "app/utils/association_preloader.rb",
    "lib/active_record_max_execution.rb",
    "app/models/concerns/component/read_only_associations.rb",
    "app/models/graph_api/active_record_backed_object.rb",
    "app/models/graph_api/query.rb",
    "app/models/pagination",
  )
  ActiveRecord::LogSubscriber.backtrace_cleaner.add_silencer { |line| line.match?(skipped_lines) }
end

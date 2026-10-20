# frozen_string_literal: true
# CRuby-only Rails::Application#load_tasks.
#
# Core's RakeTaskTestCase (components/platform/test/support/helpers/
# rake_task_test_case.rb) runs `Rails.application.load_tasks` in its
# setup, then asserts on `Rake::Task` and calls module methods the
# `.rake` files define. Rails' own `load_tasks` requires rake and loads
# every `*.rake` file under the `lib/tasks` path of the application and
# of each engine (railties engine.rb `run_tasks_blocks`). This is that,
# over the `.rake` files the emitted tree carries: `lib/tasks/**` at the
# root and under each component, in path order.
#
# The compiler emits `.rb` sources only: an app's own `.rake` files are
# not carried yet. When the tree has none, `load_tasks` names that on
# stderr instead of answering as if every task were defined, so a later
# `Rake::Task` lookup or an undefined task module is read against it.
# Overlay, not shared runtime: loading files at run time is a
# CRuby-dynamic idiom.
module Rails
  class Application
    TASK_GLOBS = ["lib/tasks/**/*.rake", "components/**/lib/tasks/**/*.rake"].freeze

    def load_tasks(_app = self)
      require "rake"
      tree = File.dirname(__dir__)
      files = TASK_GLOBS.flat_map { |glob| Dir.glob(File.join(tree, glob)) }.uniq.sort
      if files.empty? && !Application.instance_variable_get(:@no_tasks_named)
        Application.instance_variable_set(:@no_tasks_named, true)
        $stderr.puts "roundhouse: Rails.application.load_tasks found no .rake file in the emitted tree " \
                     "(#{TASK_GLOBS.join(", ")}); the compiler does not carry an app's .rake files, so no app task is defined"
      end
      files.each { |file| load file }
      self
    end
  end
end

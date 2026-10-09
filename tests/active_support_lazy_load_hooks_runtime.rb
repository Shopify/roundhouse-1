# Native ActiveSupport oracle (the Rails native Core runs; tests/support/
# rails_oracle.rb) for runtime/spinel/active_support_lazy_load_hooks.rb on
# the emitted PostgreSQL tree: ActiveSupport.on_load / run_load_hooks, the
# error[load_hook_not_run] guard around a native gem's hook, and the
# active_support.rb `autoload` entries the file declares.
#
#   ruby tests/active_support_lazy_load_hooks_runtime.rb compare
#
# `emitted` lays out a tree the way the emit does (runtime/<port>.rb,
# runtime/features/active_support/concern.rb) in a temporary directory,
# with a gem home beside it, and loads only the hooks file. `native`
# requires the pinned activesupport. Lines starting "= " are observations
# both make and must be equal: each declared autoload is one of
# active_support.rb's and resolves to the same kind of constant with
# nothing else loaded; ActiveSupport::Concern works; hooks run as Rails
# runs them, and a hook of the tree's own code that raises raises. Lines
# starting "! " are the emitted tree's own divergence, which Rails does
# not have and which the emitted mode checks itself: a hook from a file
# outside the tree (a gem home's) that raises does not run, boot names it
# (error[load_hook_not_run]) and goes on, as does a hook the tree's code
# registers inside a railtie initializer or body statement
# (runtime/rails_railtie.rb's gem_railtie_not_run and
# railtie_body_statement_not_run).
require_relative "support/rails_oracle"
require "fileutils"
require "stringio"
require "tmpdir"

ROOT = File.expand_path("..", __dir__)

if ARGV.first == "compare"
  require "open3"
  emitted, err, status = Open3.capture3(RbConfig.ruby, "-W0", __FILE__, "emitted")
  abort "emitted: #{err}#{emitted}" unless status.success?
  names = emitted.lines.grep(/\A= autoload /).map { |l| l.split[2].delete_suffix(":") }
  native, err, status = Open3.capture3(RbConfig.ruby, "-W0", __FILE__, "native", *names)
  abort "native: #{err}#{native}" unless status.success?
  shared = ->(out) { out.lines.grep(/\A= /) }
  unless shared.(native) == shared.(emitted)
    diff = shared.(native).zip(shared.(emitted)).each_with_index.reject { |(x, y), _| x == y }.first(8)
    abort "the hooks runtime differs from activesupport:\n" +
      diff.map { |(x, y), i| "line #{i + 1}\n  native:  #{x}  emitted: #{y}" }.join("\n")
  end
  puts emitted
  puts "observations: #{shared.(native).length}/#{shared.(native).length} equal"
  puts "Native ActiveSupport and the emitted hooks runtime agree (#{RailsOracle.label})"
  exit
end

MODE = ARGV.first
abort "usage: #{$PROGRAM_NAME} compare | emitted | native NAME..." unless %w[native emitted].include?(MODE)

def check(ok, what)
  abort "emitted: #{what}" unless ok
end

# Runs the block in a child, after $stdout is flushed, so each
# observation starts from the same loaded state.
def isolated(&block)
  $stdout.flush
  pid = fork do
    block.call
    $stdout.flush
    exit!(0)
  end
  Process.wait(pid)
  abort "#{MODE}: child failed (#{$?.inspect})" unless $?.success?
end

def captured_stderr
  saved, $stderr = $stderr, StringIO.new
  yield
  $stderr.string
ensure
  $stderr = saved
end

def outcome
  yield
  "ran"
rescue Exception => e # rubocop:disable Lint/RescueException
  "raises #{e.class}: #{e.message.lines.first.chomp}"
end

work = Dir.mktmpdir("rh-lazy-load-hooks")
at_exit { FileUtils.rm_rf(work) if Process.pid == $probe_parent }
$probe_parent = Process.pid
tree = File.join(work, "tree")
gem_dir = File.join(work, "gemhome", "gems", "fakegem-1.0.0")

# Hook files: one of the tree's own (app code), one in a gem home.
APP_HOOKS = <<~RUBY
  ActiveSupport.on_load(:probe_app) { $probe << [:app_hook_self, self] }
  ActiveSupport.on_load(:probe_app_failing) { AppMissing }
RUBY
GEM_HOOKS = <<~RUBY
  module Fakegem; end; ActiveSupport.on_load(:probe_gem) { include Fakegem::Missing }
  ActiveSupport.on_load(:probe_gem) { $probe << :second_gem_hook_ran }
  ActiveSupport.on_load(:probe_gem_load) { raise LoadError, "cannot load such file -- active_support/core_ext/array/extract_options" }
  ActiveSupport.on_load(:probe_gem_unported) { raise NotImplementedError, "unported" }
  ActiveSupport.on_load(:probe_gem_fatal) { raise ProbeFatal, "not a load failure" }
RUBY
FileUtils.mkdir_p(File.join(tree, "app/models"))
File.write(File.join(tree, "app/models/probe_hooks.rb"), APP_HOOKS)
FileUtils.mkdir_p(File.join(gem_dir, "lib/fakegem"))
File.write(File.join(gem_dir, "lib/fakegem/engine.rb"), GEM_HOOKS)
class ProbeFatal < Exception; end # rubocop:disable Lint/InheritException
$probe = []

if MODE == "native"
  RailsOracle.activate("activesupport")
  declared = {}
  Module.prepend(Module.new do
    define_method(:autoload) do |name, path|
      declared[name.to_sym] = path if self.name == "ActiveSupport"
      super(name, path)
    end
  end)
  require "active_support"
  ARGV.drop(1).each do |name|
    puts "= autoload #{name}: #{declared.key?(name.to_sym) ? "active_support.rb declares it" : "active_support.rb does not declare it"}"
  end
  names = ARGV.drop(1)
else
  runtime = File.join(tree, "runtime")
  FileUtils.mkdir_p(runtime)
  # As the emit lays the runtime out: each port at runtime/<name>.rb.
  [File.join(ROOT, "runtime/spinel/*.rb"), File.join(ROOT, "runtime/ruby/active_support_reporters.rb"),
   File.join(ROOT, "runtime/ruby/rails.rb")].each do |pattern|
    Dir[pattern].each { |f| FileUtils.cp(f, runtime) }
  end
  FileUtils.mkdir_p(File.join(runtime, "features/active_support"))
  File.write(File.join(runtime, "features/active_support/concern.rb"), "require_relative \"../../active_support_concern\"\n")
  require File.join(runtime, "active_support_lazy_load_hooks")
  names = ActiveSupport.constants.select { |c| ActiveSupport.autoload?(c) }.sort.map(&:to_s)
  check(names.include?("Concern"), "ActiveSupport::Concern is not declared for autoload")
  names.each do |name|
    path = ActiveSupport.autoload?(name)
    check(path.start_with?("#{runtime}/") && File.exist?(path), "#{name} autoloads #{path}, not a runtime file of the tree")
    puts "= autoload #{name}: active_support.rb declares it"
  end
end

# Each declared constant, resolved with nothing else loaded.
names.each do |name|
  isolated do
    const = ActiveSupport.const_get(name)
    kind = const.is_a?(Class) ? "Class < #{const.superclass}" : const.class.to_s
    puts "= #{name} resolves: #{kind}; autoload? after: #{ActiveSupport.autoload?(name).inspect}"
  end
end

isolated do
  concern = Module.new do
    extend ActiveSupport::Concern
    included { def self.included_ran = :included_ran }
    class_methods { def class_method = :class_method }
  end
  klass = Class.new { include concern }
  puts "= ActiveSupport::Concern: #{klass.included_ran} #{klass.class_method} #{ActiveSupport::Concern::MultipleIncludedBlocks.superclass}"
  twice = outcome do
    Module.new do
      extend ActiveSupport::Concern
      included { :first }
      included { :second }
    end
  end
  puts "= ActiveSupport::Concern two included blocks: #{twice}"
end

if MODE == "emitted"
  # A gem's `require "active_support/concern"` (runtime/features) after
  # the autoload: one load, no constant redefined.
  isolated do
    warnings = []
    Warning.singleton_class.prepend(Module.new { define_method(:warn) { |msg, **| warnings << msg } })
    $VERBOSE = true
    require File.join(tree, "runtime/features/active_support/concern")
    loads = $LOADED_FEATURES.count { |f| f.end_with?("/active_support_concern.rb") }
    check(ActiveSupport::Concern.is_a?(Module) && loads == 1 && warnings.empty?, "concern loaded #{loads} times: #{warnings.inspect}")
    puts "! require \"active_support/concern\" after the autoload: loaded once, no warnings"
  end
end

# Hooks as Rails runs them.
isolated do
  base = Class.new { def self.name = "ProbeBase" }
  ActiveSupport.on_load(:probe_order) { $probe << [:class_eval, self.name] }
  ActiveSupport.on_load(:probe_order, yield: true) { |b| $probe << [:yield, b.name] }
  once = proc { $probe << :once }
  ActiveSupport.on_load(:probe_order, run_once: true, &once)
  ActiveSupport.run_load_hooks(:probe_order, base)
  ActiveSupport.run_load_hooks(:probe_order, base)
  ActiveSupport.on_load(:probe_order) { $probe << [:late, self.name] }
  ActiveSupport.run_load_hooks(:probe_instance, Object.new.tap { |o| def o.tag = :instance })
  ActiveSupport.on_load(:probe_instance) { $probe << [:instance_eval, tag] }
  puts "= hook order: #{$probe.inspect}"
end

isolated do
  load File.join(tree, "app/models/probe_hooks.rb")
  ActiveSupport.run_load_hooks(:probe_app, Module.new)
  puts "= the tree's own hook: #{$probe.first.first}"
  puts "= the tree's own hook that raises: #{outcome { ActiveSupport.run_load_hooks(:probe_app_failing, Module.new) }}"
  load File.join(gem_dir, "lib/fakegem/engine.rb")
  puts "= a gem's hook that raises a non-load failure: #{outcome { ActiveSupport.run_load_hooks(:probe_gem_fatal, Module.new) }}"
end

if MODE == "emitted"
  isolated do
    spec = Gem::Specification.new { |s| s.name = "fakegem"; s.version = "1.0.0" }
    spec.loaded_from = File.join(work, "gemhome/specifications/fakegem-1.0.0.gemspec")
    Gem.loaded_specs["fakegem"] = spec
    load File.join(gem_dir, "lib/fakegem/engine.rb")
    result = nil
    err = captured_stderr { result = outcome { ActiveSupport.run_load_hooks(:probe_gem, Module.new) } }
    expected = "roundhouse: error[load_hook_not_run]: fakegem-1.0.0/lib/fakegem/engine.rb:1 on_load(:probe_gem) did not run: NameError: uninitialized constant Fakegem::Missing"
    check(result == "ran" && err.include?(expected) && $probe == [:second_gem_hook_ran], "gem hook: #{result}; #{err.inspect}; #{$probe.inspect}")
    puts "! a gem's hook that raises NameError: #{err.lines.first.chomp}; the next hook ran; run_load_hooks returned"

    err = captured_stderr { result = outcome { ActiveSupport.run_load_hooks(:probe_gem_load, Module.new) } }
    check(result == "ran" && err.include?("engine.rb:3 on_load(:probe_gem_load) did not run: LoadError: cannot load such file"), "gem LoadError hook: #{err.inspect}")
    puts "! a gem's hook that raises LoadError: #{err.lines.first.chomp}"
    # Registered after its base loaded, it runs at once, under the same guard
    # (this file is outside the tree too).
    late_line = __LINE__ + 1
    err = captured_stderr { result = outcome { ActiveSupport.on_load(:probe_gem_load) { raise LoadError, "late" } } }
    check(result == "ran" && err.include?("#{__FILE__}:#{late_line} on_load(:probe_gem_load) did not run: LoadError: late"), "late gem hook: #{err.inspect}")
    puts "! a native hook registered after its base loaded: did not run, named; on_load returned"
    err = captured_stderr { result = outcome { ActiveSupport.run_load_hooks(:probe_gem_unported, Module.new) } }
    check(result == "ran" && err.include?("engine.rb:4 on_load(:probe_gem_unported) did not run: NotImplementedError: unported"), "gem NotImplementedError hook: #{err.inspect}")
    puts "! a gem's hook that raises NotImplementedError: #{err.lines.first.chomp}"

    # Without a loaded spec (Core's boot puts the gem home's lib
    # directories on the load path), the gem home's layout names the gem;
    # a file in no gem directory, its full path.
    Gem.loaded_specs.delete("fakegem")
    err = captured_stderr { ActiveSupport.run_load_hooks(:probe_gem_unported, Module.new) }
    check(err.include?("error[load_hook_not_run]: fakegem-1.0.0/lib/fakegem/engine.rb:4 on_load"), "gem hook path: #{err.inspect}")
    loose = File.join(work, "site_ruby/loose_hooks.rb")
    FileUtils.mkdir_p(File.dirname(loose))
    File.write(loose, "ActiveSupport.on_load(:probe_loose) { LooseMissing }\n")
    load loose
    err = captured_stderr { ActiveSupport.run_load_hooks(:probe_loose, Module.new) }
    check(err.include?("error[load_hook_not_run]: #{loose}:1 on_load(:probe_loose) did not run: NameError: uninitialized constant LooseMissing"), "loose hook path: #{err.inspect}")
    puts "! with no loaded spec, the gem directory names the gem; a file outside any gem directory, its path"
  end

  # The tree's code registering inside a railtie initializer or body
  # statement (runtime/rails_railtie.rb, as the emit lays it out).
  isolated do
    require File.join(tree, "runtime/rails_railtie")
    engine_file = File.join(tree, "app/models/probe_engine.rb")
    File.write(engine_file, <<~RUBY)
      class ProbeEngine < Rails::Railtie
        run_body_statement("components/probe/lib/probe/engine.rb:7") do
          ActiveSupport.on_load(:probe_body) { ExceptionsController.append_view_path("x") }
        end
      end
      Rails::Railtie.not_run_unless_it_runs("ProbeEngine initializer \\"probe.hooks\\"") do
        ActiveSupport.on_load(:probe_initializer) { CustomerApi::GraphQLController }
      end
      ActiveSupport.on_load(:probe_top_level) { TopLevelMissing }
    RUBY
    load engine_file
    err = captured_stderr do
      ActiveSupport.run_load_hooks(:probe_body, Module.new)
      ActiveSupport.run_load_hooks(:probe_initializer, Module.new)
    end
    lines = err.lines.map(&:chomp)
    check(lines == [
      "roundhouse: error[load_hook_not_run]: app/models/probe_engine.rb:3 on_load(:probe_body) did not run: NameError: uninitialized constant ProbeEngine::ExceptionsController (registered by a railtie or engine body statement, error[railtie_body_statement_not_run])",
      "roundhouse: error[load_hook_not_run]: app/models/probe_engine.rb:7 on_load(:probe_initializer) did not run: NameError: uninitialized constant CustomerApi (registered by a railtie or engine initializer, error[gem_railtie_not_run])",
    ], "covered hooks: #{err.inspect}")
    lines.each { |l| puts "! #{l}" }
    top = outcome { ActiveSupport.run_load_hooks(:probe_top_level, Module.new) }
    check(top == "raises NameError: uninitialized constant TopLevelMissing", "the tree's uncovered hook: #{top}")
    puts "! the tree's hook registered outside a railtie statement still #{top}"
    # Where the emitted test helper fires the test-case hooks
    # (`project::test_case_load_hooks_block`): the same hook is named and
    # the next one runs.
    $probe = []
    ActiveSupport.on_load(:probe_top_level) { $probe << :after_failing }
    $probe = []
    err = captured_stderr do
      top = outcome do
        ActiveSupport::LazyLoadHooks.naming_failures("activesupport/lib/active_support/test_case.rb:408") do
          ActiveSupport.run_load_hooks(:probe_top_level, Module.new)
        end
      end
    end
    check(top == "ran" && $probe == [:after_failing] && err.lines.map(&:chomp) == [
      "roundhouse: error[load_hook_not_run]: app/models/probe_engine.rb:9 on_load(:probe_top_level) did not run: NameError: uninitialized constant TopLevelMissing (fired at activesupport/lib/active_support/test_case.rb:408)",
    ], "test helper firing: #{top}; #{$probe.inspect}; #{err.inspect}")
    puts "! #{err.lines.first.chomp}; the next hook ran"
    top = outcome { ActiveSupport.run_load_hooks(:probe_top_level, Module.new) }
    check(top == "raises NameError: uninitialized constant TopLevelMissing", "outside the firing: #{top}")
  end

  # A test base whose hooks the emitted test helper defers to its first
  # subclass (`project::test_case_load_hooks_block`), as Rails loads the
  # base where a test first names it: none run before; the first subclass
  # (or a subclass of a base whose file requires it) fires them once,
  # before its body; a failing one of the tree's own is named and the next
  # runs; a hook registered after runs at once.
  isolated do
    cases_file = File.join(tree, "app/models/probe_cases.rb")
    File.write(cases_file, <<~RUBY)
      ActiveSupport.on_load(:probe_controller_case) { $probe << [:controller, self] }
      ActiveSupport.on_load(:probe_view_case) { CaseMissing }
      ActiveSupport.on_load(:probe_view_case) { $probe << [:view, self] }
    RUBY
    load cases_file
    controller_base, view_base = Class.new, Class.new
    ActiveSupport::LazyLoadHooks.run_when_subclassed(:probe_controller_case, controller_base, "actionpack/lib/action_controller/test_case.rb:611")
    ActiveSupport::LazyLoadHooks.run_when_subclassed(:probe_view_case, view_base, "actionview/lib/action_view/test_case.rb:256", after: [:probe_controller_case])
    check($probe.empty?, "before a subclass: #{$probe.inspect}")
    seen = nil
    err = captured_stderr { Class.new(view_base) { seen = $probe.dup } }
    check(seen == [[:controller, controller_base], [:view, view_base]], "first subclass: #{seen.inspect}")
    check(err.lines.map(&:chomp) == [
      "roundhouse: error[load_hook_not_run]: app/models/probe_cases.rb:2 on_load(:probe_view_case) did not run: NameError: uninitialized constant CaseMissing (fired at actionview/lib/action_view/test_case.rb:256)",
    ], "failing deferred hook: #{err.inspect}")
    Class.new(Class.new(controller_base))
    Class.new(view_base)
    check($probe.length == 2, "fired once: #{$probe.inspect}")
    ActiveSupport.on_load(:probe_controller_case) { $probe << :registered_after }
    check($probe.last == :registered_after, "registered after: #{$probe.inspect}")
    puts "! a test base's hooks: none before its first subclass, then once, after the bases its file requires; #{err.lines.first.chomp}; a hook registered after runs at once"
  end
end

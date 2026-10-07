# The Rails a native oracle loads. By default it is the copy native Core
# runs: Shopify/rails at 5813559f4aed (8.2.0.alpha, branch
# sep-21-plus-query-constraints, Core's Gemfile.lock GIT entry), from
# Core's pinned gem home, with that gem home's other gems (i18n, tzinfo,
# concurrent-ruby, semian, dalli, ...) and nothing from ~/.local.
#
#   RH_RAILS_ORACLE=<path>   another Rails checkout (a directory holding
#                            activesupport/, activerecord/, ...)
#   RH_RAILS_ORACLE=8.1.4    the installed release gems of that version,
#                            for upstream Roundhouse tests that use them
#   CORE_GEM_HOME=<dir>      the pinned gem home
#
# `RailsOracle.activate("activesupport", ...)` must run before any Rails
# file is required. On the pinned copy it re-executes the script once
# under jemalloc (LD_PRELOAD): the gem home's native extensions are built
# against a jemalloc Ruby and fail to load without it.
require "rbconfig"

module RailsOracle
  CORE_GEM_HOME = ENV.fetch("CORE_GEM_HOME", "/nix/store/kr4swvh5mw01fs3kixfa9afy57zsyihg-ruby-gems-dev/lib/ruby/gems/4.0.0")
  PINNED = File.join(CORE_GEM_HOME, "bundler/gems/rails-5813559f4aed")
  JEMALLOC = ENV.fetch("RH_JEMALLOC", "/nix/store/c04j2i0fkin1bw5fab35gx896558knsd-jemalloc-5.3.0-unstable-2025-09-12/lib/libjemalloc.so.2")
  COMPONENTS = %w[activesupport activemodel activerecord activejob actionpack actionview railties].freeze

  def self.source
    value = ENV["RH_RAILS_ORACLE"].to_s
    value.empty? ? PINNED : value
  end

  # A release version (`8.1.4`) rather than a checkout.
  def self.release?
    source.match?(/\A\d+(\.[0-9A-Za-z]+)+\z/)
  end

  # What the oracle is, for messages.
  def self.label
    release? ? "Rails #{source} (installed gems)" : "Rails #{version} (#{File.basename(source)})"
  end

  def self.version
    File.read(File.join(source, "RAILS_VERSION")).strip
  rescue SystemCallError
    "unknown"
  end

  # The environment a child Ruby needs to activate the oracle itself:
  # the pinned gem home and, where it exists, jemalloc.
  def self.env
    return {} if release?
    env = { "GEM_HOME" => CORE_GEM_HOME, "GEM_PATH" => CORE_GEM_HOME }
    preload = ENV["LD_PRELOAD"].to_s
    if File.exist?(JEMALLOC) && !preload.include?("jemalloc")
      env["LD_PRELOAD"] = [JEMALLOC, *preload.split(/[: ]/).reject(&:empty?)].join(":")
    end
    env
  end

  # For a child that loads gems from the pinned gem home without the
  # oracle (an emitted-runtime mode on Core's dalli): that gem home ahead
  # of the current gem path, and jemalloc.
  def self.core_gems_env
    env = { "GEM_PATH" => [CORE_GEM_HOME, *Gem.path].uniq.join(File::PATH_SEPARATOR) }
    preload = ENV["LD_PRELOAD"].to_s
    if File.exist?(JEMALLOC) && !preload.include?("jemalloc")
      env["LD_PRELOAD"] = [JEMALLOC, *preload.split(/[: ]/).reject(&:empty?)].join(":")
    end
    env
  end

  # Activates the named Rails components from the oracle and each other
  # gem at its version (`semian: "0.28.3"`); on the pinned copy, every
  # gem comes from the pinned gem home.
  def self.activate(*gems, **versions)
    if release?
      gems.each { |name| gem name, source }
      versions.each { |name, version| gem name.to_s, version }
      return
    end
    abort "RH_RAILS_ORACLE: no Rails checkout at #{source}" unless File.directory?(File.join(source, "activesupport"))
    needed = env
    if needed.any? { |k, v| ENV[k] != v } && !ENV["RH_RAILS_ORACLE_EXEC"]
      exec(needed.merge("RH_RAILS_ORACLE_EXEC" => "1"), RbConfig.ruby, $PROGRAM_NAME, *ARGV)
    end
    Gem.paths = { "GEM_HOME" => CORE_GEM_HOME, "GEM_PATH" => CORE_GEM_HOME }
    register_checkout
    gems.each do |name|
      gem name
      loaded = Gem.loaded_specs.fetch(name).full_gem_path
      abort "#{name} activated from #{loaded}, not #{source}" unless loaded.start_with?(source)
    end
    versions.each { |name, version| gem name.to_s, version }
  end

  # Registers each component's gemspec from the checkout, as Bundler does
  # for a git source, so `gem "activesupport"` (and any gem depending on
  # it) activates the checkout.
  def self.register_checkout
    COMPONENTS.each do |name|
      dir = File.join(source, name)
      path = File.join(dir, "#{name}.gemspec")
      next unless File.exist?(path)
      spec = Dir.chdir(dir) { Gem::Specification.load(path) } or abort "cannot load #{path}"
      spec.loaded_from = path
      spec.full_gem_path = dir
      Gem::Specification.add_spec(spec)
    end
  end
end

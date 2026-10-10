# The Rails a native oracle loads. By default it is the copy native Core
# runs: Shopify/rails at 5eb8c4c45bee (8.2.0.alpha, Core's Gemfile.lock
# GIT entry), from Core's pinned gem home, with that gem home's other
# gems (i18n, tzinfo, concurrent-ruby, semian, dalli, ...) and nothing
# from ~/.local. The checkout is whichever `bundler/gems/rails-*` the gem
# home holds, so an older gem home needs no other setting.
#
#   RH_RAILS_ORACLE=<path>   another Rails checkout (a directory holding
#                            activesupport/, activerecord/, ...)
#   RH_RAILS_ORACLE=8.1.4    the installed release gems of that version,
#                            for upstream Roundhouse tests that use them
#   CORE_GEM_HOME=<dir>      the pinned gem home. The previous Core's is
#                            /nix/store/kr4swvh5mw01fs3kixfa9afy57zsyihg-ruby-gems-dev/lib/ruby/gems/4.0.0
#                            (Rails 5813559f4aed).
#   CORE_GEM_PATH=<dirs>     the gem path (colon separated). Defaults to
#                            Core's full path when CORE_GEM_HOME is the
#                            default, else to CORE_GEM_HOME alone.
#
# `RailsOracle.activate("activesupport", ...)` must run before any Rails
# file is required. On the pinned copy it re-executes the script once
# under jemalloc (LD_PRELOAD): the gem home's native extensions are built
# against a jemalloc Ruby and fail to load without it.
require "rbconfig"

module RailsOracle
  DEFAULT_GEM_HOME = "/nix/store/d958gj4a8pmld3fxa7fhyqasz61cafch-ruby-gems-dev/lib/ruby/gems/4.0.0"
  # What `shadowenv exec -- env` gives in Core, with the store gem home in
  # place of .dev/gem (bin/core-env.sh): the gem home, bundler's patched
  # source gems and the Ruby's default gems.
  DEFAULT_GEM_PATH = [
    DEFAULT_GEM_HOME,
    "/nix/store/kvgr3vdcmx43q239fnbz1nxmxwkagn85-bundler-source-tectonix-patched/lib/ruby/gems/4.0.0",
    "/nix/store/axc1fldsz9iwv6vp72bmzz4bnbz9124a-ruby-4.0.7/lib/ruby/gems/4.0.0"
  ].join(File::PATH_SEPARATOR)
  CORE_GEM_HOME = ENV.fetch("CORE_GEM_HOME", DEFAULT_GEM_HOME)
  CORE_GEM_PATH = ENV.fetch("CORE_GEM_PATH") { CORE_GEM_HOME == DEFAULT_GEM_HOME ? DEFAULT_GEM_PATH : CORE_GEM_HOME }
  PINNED = Dir[File.join(CORE_GEM_HOME, "bundler/gems/rails-*")].min || File.join(CORE_GEM_HOME, "bundler/gems/rails")
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
    env = { "GEM_HOME" => CORE_GEM_HOME, "GEM_PATH" => CORE_GEM_PATH }
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
    env = { "GEM_PATH" => [*CORE_GEM_PATH.split(File::PATH_SEPARATOR), *Gem.path].uniq.join(File::PATH_SEPARATOR) }
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
    Gem.paths = { "GEM_HOME" => CORE_GEM_HOME, "GEM_PATH" => CORE_GEM_PATH }
    pin_gem_home_versions
    register_checkout
    gems.each do |name|
      gem name
      loaded = Gem.loaded_specs.fetch(name).full_gem_path
      abort "#{name} activated from #{loaded}, not #{source}" unless loaded.start_with?(source)
    end
    versions.each { |name, version| gem name.to_s, version }
  end

  # Gems the Ruby ships as bundled gems can sit on the gem path at a
  # newer version than the one Core's Gemfile.lock selects (minitest 6.0.0
  # in the Ruby's own gem dir, 5.27.0 in the gem home). Bundler picks the
  # locked one; without it RubyGems would take the highest, so activate
  # the gem home's version.
  LOCKED_IN_GEM_HOME = %w[minitest].freeze

  def self.pin_gem_home_versions
    LOCKED_IN_GEM_HOME.each do |name|
      specs = Dir[File.join(CORE_GEM_HOME, "specifications", "#{name}-[0-9]*.gemspec")]
      versions = specs.map { |path| File.basename(path, ".gemspec").delete_prefix("#{name}-") }
      next if versions.empty?
      gem name, versions.map { |v| Gem::Version.new(v) }.max.to_s
    end
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

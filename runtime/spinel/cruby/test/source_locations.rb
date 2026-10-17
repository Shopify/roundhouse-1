# frozen_string_literal: true
# Source locations of emitted code, answered with the authored paths.
#
# The emitted tree moves code: a test written in
# components/checkouts/test/models/x_test.rb runs from
# test/models/x_test.rb, a class from components/…/app/models/y.rb from
# app/models/y.rb, a required file program from
# app/models/roundhouse_files/<path>. Natively Method#source_location,
# UnboundMethod#source_location, Proc#source_location and
# Module#const_source_location name the authored file under Rails.root,
# and app code decides by it (Core's MochaStrictStubbing keeps legacy
# Mocha stubbing for opted-out components). Each answers here from the
# generated table (runtime/roundhouse_source_paths.rb,
# emit/ruby/source_map.rs) when it names an emitted file the table knows,
# and as Ruby answered otherwise.
#
# method_source's `Method#source` / `#comment` read the file a
# `source_location` names at its line, so they read the emitted
# location (the authored file is not in the tree, and its lines are not
# the emitted ones): the text they answer is the emitted code of the
# method (Core's GraphQLTestHelper looks for `assert_no_resolve_queries`
# in a preload test's `method(name).source`).
#
# DIVERGENCE, stated: the path is the authored file, the line is still
# the emitted one, except const_source_location's, which is the authored
# `class`/`module` line. Code under runtime/ and anything the table does
# not know keeps its emitted location. caller_locations/backtraces keep
# the emitted paths: their lines index the emitted file, and the run's
# failure sites are read from them.
#
# Rails.root is the tree root as an absolute path, as Rails' is
# (config.root is expanded): `"#{Rails.root}/"` prefixes the absolute
# paths source_location answers, which "." never did.
require_relative "roundhouse_source_paths"

module RoundhouseSourceLocations
  ROOT = File.expand_path("..", __dir__)
  PREFIX = "#{ROOT}/"
  PROGRAMS = "app/models/roundhouse_files/"
  MODULE_NAME = Module.instance_method(:name)

  # `location` ([path, line]) with an emitted path the table knows
  # replaced by the authored one. `owner`, a method's owner, names the
  # file of a class whose declarations are all in one authored file.
  def self.authored(location, owner = nil)
    rel = emitted(location)
    return location unless rel
    path = RoundhouseSourcePaths::FILES[rel]
    path ||= rel.delete_prefix(PROGRAMS) if rel.start_with?(PROGRAMS)
    if path.nil? && owner && rel.start_with?("app/")
      entry = RoundhouseSourcePaths::CONSTANTS[owner_name(owner)]
      path = entry[0] if entry && entry[2]
    end
    return location unless path
    [File.join(ROOT, path), location[1]]
  end

  # The tree-relative path of `location`'s file, or nil outside the tree.
  # A file loaded by a relative path (the runner's `load
  # "test/models/x_test.rb"`) answers it relative to the working
  # directory it was loaded from, the tree root.
  def self.emitted(location)
    return unless location.is_a?(Array) && location[0].is_a?(String)
    path = File.expand_path(location[0], ROOT)
    path.delete_prefix(PREFIX) if path.start_with?(PREFIX)
  end

  def self.owner_name(owner)
    owner = owner.attached_object if owner.singleton_class?
    owner.is_a?(Module) ? MODULE_NAME.bind_call(owner) : nil
  rescue TypeError
    nil
  end

  # The authored declaration of constant `name` read through `scope`, when
  # Ruby's answer is emitted code.
  def self.constant(scope, name, location)
    rel = emitted(location)
    return location if rel.nil? || rel.start_with?("runtime/")
    name = name.to_s
    candidates = [name.delete_prefix("::")]
    scope_name = MODULE_NAME.bind_call(scope)
    candidates.unshift("#{scope_name}::#{name}") if scope_name && scope != Object && !name.start_with?("::")
    candidates.each do |candidate|
      entry = RoundhouseSourcePaths::CONSTANTS[candidate]
      return [File.join(ROOT, entry[0]), entry[1]] if entry
    end
    authored(location)
  end

  # While method_source reads a method's text, source_location answers
  # the emitted location (see above).
  def self.emitted_locations
    previous = Thread.current[:roundhouse_emitted_locations]
    Thread.current[:roundhouse_emitted_locations] = true
    yield
  ensure
    Thread.current[:roundhouse_emitted_locations] = previous
  end

  def self.emitted_locations?
    Thread.current[:roundhouse_emitted_locations]
  end

  module MethodLocation
    def source_location
      location = super
      RoundhouseSourceLocations.emitted_locations? ? location : RoundhouseSourceLocations.authored(location, owner)
    end
  end

  module ProcLocation
    def source_location
      location = super
      RoundhouseSourceLocations.emitted_locations? ? location : RoundhouseSourceLocations.authored(location)
    end
  end

  # method_source's readers (the gem is required by app code, after this
  # file loads, and defines these on the same three classes).
  module MethodSourceReaders
    def source
      RoundhouseSourceLocations.emitted_locations { super }
    end

    def comment
      RoundhouseSourceLocations.emitted_locations { super }
    end
  end

  module ConstLocation
    def const_source_location(name, inherit = true)
      RoundhouseSourceLocations.constant(self, name, super)
    end
  end

  module AbsoluteRoot
    def root
      Rails::AppPath.new(RoundhouseSourceLocations::ROOT)
    end
  end
end

Method.prepend(RoundhouseSourceLocations::MethodLocation)
UnboundMethod.prepend(RoundhouseSourceLocations::MethodLocation)
Proc.prepend(RoundhouseSourceLocations::ProcLocation)
[Method, UnboundMethod, Proc].each { |klass| klass.prepend(RoundhouseSourceLocations::MethodSourceReaders) }
Module.prepend(RoundhouseSourceLocations::ConstLocation)
module Rails
  singleton_class.prepend(RoundhouseSourceLocations::AbsoluteRoot)
end

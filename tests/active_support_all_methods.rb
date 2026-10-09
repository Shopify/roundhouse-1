# What Rails' `require "active_support/all"` adds to Ruby's own classes
# and modules (Integer#months, Time.zone, Object#as_json, ...), each with
# the Active Support feature that defines it, against the Rails native
# Core runs (tests/support/rails_oracle.rb).
#
#   ruby tests/active_support_all_methods.rb native     # Rails' additions
#   ruby tests/active_support_all_methods.rb literal    # as the Ruby hash literal
#                                                       # project.rs::ACTIVE_SUPPORT_ALL_METHODS holds
#   ruby tests/active_support_all_methods.rb tree TREE  # the table TREE carries
#
# `native` and `tree` print one `feature Owner#name` (instance) or
# `feature Owner.name` (singleton) line per method, sorted, so the two
# compare as text. On the tree, the table names the method an emitted
# NameError lacks (runtime/active_support_not_ported.rb).
require_relative "support/rails_oracle"

MODE = ARGV.first
abort "usage: #{$PROGRAM_NAME} native | literal | tree TREE" unless %w[native literal tree].include?(MODE)

if MODE == "tree"
  tree = ARGV.fetch(1)
  require File.join(tree, "runtime/active_support_not_ported")
  lines = RoundhouseActiveSupportNotPorted::ALL.flat_map do |feature, owners|
    owners.flat_map { |owner, names| names.split.map { |label| "#{feature} #{owner}#{label}" } }
  end
  puts lines.sort
  exit
end

RailsOracle.activate("activesupport")
# Ruby's own classes Active Support extends, loaded first so their
# methods before it are the baseline.
require "date"
require "bigdecimal"
require "pathname"
require "uri"
require "ipaddr"
require "erb"
require "json"
require "securerandom"
require "digest"
require "logger"
require "set"

CORE = %w[
  BasicObject Object Kernel Module Class Comparable Enumerable String Symbol Integer Float Numeric Rational Complex
  Array Hash Range NilClass TrueClass FalseClass Regexp Exception NameError LoadError NoMethodError Method
  UnboundMethod Proc Struct Process File Dir IO Thread Time Date DateTime BigDecimal Pathname URI IPAddr ERB
  Digest Digest::UUID SecureRandom JSON Set Logger
].freeze

def methods_of(mod)
  inst = mod.public_instance_methods(false) + mod.protected_instance_methods(false) + mod.private_instance_methods(false)
  sing = mod.singleton_class.public_instance_methods(false)
  [inst.map { |m| "##{m}" }, sing.map { |m| ".#{m}" }].flatten
end

def snapshot
  CORE.to_h do |name|
    mod = Object.const_defined?(name) ? Object.const_get(name) : nil
    [name, mod.is_a?(Module) ? methods_of(mod) : []]
  end
end

require "active_support"
baseline = snapshot
lib = File.join(RailsOracle.source, "activesupport/lib")
# Leaves first (core_ext/integer/time before core_ext/integer), so a
# method is credited to the file that defines it.
features = Dir.glob("active_support/core_ext/**/*.rb", base: lib)
  .map { |path| path.delete_suffix(".rb") }
  .sort_by { |feature| [-feature.count("/"), feature] }
features += %w[active_support/core_ext active_support/time active_support/all]
credited = {}
# Each require's newly loaded Active Support files, in load order.
loaded_by = {}
seen = baseline
features.each do |feature|
  before = $LOADED_FEATURES.dup
  require feature
  now = snapshot
  files = ($LOADED_FEATURES - before).select { |f| f.include?("/activesupport/lib/active_support/core_ext/") }
  now.each do |owner, labels|
    (labels - seen[owner]).each do |label|
      key = "#{owner}#{label}"
      next if credited.key?(key)
      credited[key] = feature
      loaded_by[key] = files
    end
  end
  seen = now
end

# A method's own source file, when Active Support's, names its feature
# better than the require that first loaded it.
def defined_in(owner, label)
  mod = Object.const_get(owner)
  method = label.start_with?("#") ? mod.instance_method(label[1..]) : mod.method(label[1..])
  file = method.source_location&.first.to_s
  feature = file[%r{/activesupport/lib/(active_support/.+)\.rb\z}, 1]
  feature if feature&.start_with?("active_support/")
rescue NameError
  nil
end

# An alias of a C method (`alias_method :compare_without_coercion, :<=>`)
# has no source: the first file its require loaded that names it.
def named_in(files, label)
  name = Regexp.escape(label[1..])
  word = /(?<![\w@$])[:]?#{name}(?![\w?!=])/
  file = files.find { |f| File.read(f).match?(word) }
  file && file[%r{/activesupport/lib/(active_support/.+)\.rb\z}, 1]
end

# Only Active Support's own: a method a require loaded from elsewhere
# (psych's Set#encode_with, under date/calculations) is not its.
table = {}
credited.each_key do |key|
  owner, label = key.split(/(?=[#.])/, 2)
  feature = defined_in(owner, label) || named_in(loaded_by[key], label)
  if feature
    table[key] = feature
  else
    warn "not Active Support's: #{key} (loaded under #{credited[key]})"
  end
end

if MODE == "native"
  puts table.map { |key, feature| "#{feature} #{key}" }.sort
  exit
end

# The literal: feature => { owner => "#a #b .c" }, sorted.
grouped = Hash.new { |h, k| h[k] = Hash.new { |g, o| g[o] = [] } }
table.each do |key, feature|
  owner, label = key.split(/(?=[#.])/, 2)
  grouped[feature][owner] << label
end
grouped.sort.each do |feature, owners|
  body = owners.sort.map { |owner, labels| "#{owner.inspect} => #{labels.sort.join(" ").inspect}" }.join(", ")
  puts "  #{feature.inspect} => { #{body} },"
end

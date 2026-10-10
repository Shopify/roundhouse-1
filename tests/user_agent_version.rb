# Native useragent 0.16.11 oracle for UserAgent::Version
# (runtime/spinel/user_agent_version.rb over the port runtime/ruby/user_agent.rb).
#
# Modes:
# - `native` loads the installed gem (Core's gem home, CORE_GEM_HOME) and
#   `port` loads the runtime port plus the Version file, the order the
#   CRuby PostgreSQL tree loads them. Each prints one line per observation:
#   what Version.new, to_a, to_s, inspect, nil?, ==, eql?, <=> and the
#   Comparable operators answer over a battery of version strings, the
#   errors included, plus the comparisons secure_headers makes.
# - `compare` runs both and requires every observation equal.
require_relative "support/rails_oracle"

CORE_GEM_HOME = RailsOracle::CORE_GEM_HOME
GEM_LIB = File.join(CORE_GEM_HOME, "gems/useragent-0.16.11/lib")

if ARGV.first == "compare"
  require "open3"
  abort "native: #{GEM_LIB} is missing (set CORE_GEM_HOME)" unless File.directory?(GEM_LIB)
  outputs = %w[native port].map do |mode|
    stdout, stderr, status = Open3.capture3(RbConfig.ruby, __FILE__, mode)
    abort "#{mode}: #{stderr}" unless status.success?
    stdout
  end
  unless outputs.uniq.length == 1
    a, b = outputs.map(&:lines)
    diff = a.zip(b).each_with_index.reject { |(x, y), _| x == y }.first(12)
    abort "Version differs (#{a.length} native lines, #{b.length} port):\n" +
      diff.map { |(x, y), i| "line #{i + 1}\n  native: #{x}  port:   #{y}" }.join("\n")
  end
  puts outputs.first
  puts "Native useragent and the ported UserAgent::Version agree"
  exit
end

if ARGV.first == "native"
  $LOAD_PATH.unshift(GEM_LIB)
  require "useragent"
  abort "native: loaded #{$LOADED_FEATURES.grep(/user_agent\/version/).inspect}" unless
    $LOADED_FEATURES.any? { |f| f == File.realpath(File.join(GEM_LIB, "user_agent/version.rb")) }
else
  root = File.expand_path("..", __dir__)
  require File.join(root, "runtime/ruby/user_agent")
  require File.join(root, "runtime/spinel/user_agent_version")
  abort "port: the gem leaked in" if $LOADED_FEATURES.any? { |f| f.include?("useragent-") }
end

V = UserAgent::Version

def outcome
  value = yield
  value.is_a?(V) ? "Version(#{value.to_s.inspect})" : value.inspect
rescue Exception => e
  "#{e.class}: #{e.message}"
end

STRINGS = [
  nil, "", " ", "\t", "0", "00", "1", "9", "10", "46", "46.0", "45.9.9", "47", "126.0.6478.126",
  "4.0b2", "4.0", "4.0.0", "4.0a", "4.0b", "4.0b10", "17.2", "17.10", "17.2.1", "1.2.3.4.5.6",
  "1.2.3.4.5.6.7", "1.2.3.4.5.6.8", "1.2.3.4.5.7", "Unknown", "abc", "ABC", "10a", "10.a", "1..2",
  "1.2-beta", "1.2beta-3", "1.2.beta", "v1.0", " 10", "10 ", "1_2", "a\n10", "10\nabc", "1.0\n2",
  "534.52.7", "605.1.15", "0.0", "0.1", "1.", ".1", "-1",
]

STRINGS.each do |s|
  puts "new #{s.inspect}: #{outcome { V.new(s) }}"
  v = V.new(s)
  puts "#{s.inspect} to_a: #{outcome { v.to_a }}"
  puts "#{s.inspect} to_s/to_str/inspect: #{outcome { [v.to_s, v.to_str, v.inspect] }}"
  puts "#{s.inspect} nil?: #{outcome { v.nil? }}"
  puts "#{s.inspect} frozen/dup: #{outcome { [v.to_s.frozen?, v.to_s.equal?(v.to_s), v.to_a.equal?(v.to_a)] }}"
  puts "#{s.inspect} == nil / <=> nil: #{outcome { [v == nil, v <=> nil] }}"
  puts "#{s.inspect} <=> 1 / == 1: #{outcome { [v <=> 1, v == 1] }}"
  puts "#{s.inspect} >= 1: #{outcome { v >= 1 }}"
  STRINGS.each do |t|
    w = V.new(t)
    puts "#{s.inspect} vs #{t.inspect}: #{outcome { [v <=> w, v == w, v.eql?(w), v < w, v <= w, v > w, v >= w, v.between?(w, w)] }}"
    next if t.nil?
    puts "#{s.inspect} vs String #{t.inspect}: #{outcome { [v <=> t, v == t, v.eql?(t), v < t, v >= t] }}"
  end
end

# Construction from a Version is the same object, and from anything
# else (not String, Version or nil) an ArgumentError.
puts "new(Version) identity: #{outcome { v = V.new("1.2"); V.new(v).equal?(v) }}"
puts "new(): #{outcome { V.new }}"
[1, 1.5, :sym, ["1"], Object].each { |o| puts "new #{o.inspect}: #{outcome { V.new(o) }}" }
puts "ancestors: #{outcome { V.ancestors.take_while { |m| m != Object }.map(&:to_s) }}"
puts "clamp: #{outcome { V.new("50").clamp(V.new("10"), V.new("46")) }}"
puts "sort: #{outcome { STRINGS.compact.map { |s| V.new(s) }.select { |v| v.to_a.all?(Integer) }.sort.map(&:to_s) }}"

# secure_headers 5.2.0: content_security_policy.rb's constants and the
# `(version || FALLBACK_VERSION) >= VERSION_46` tests, with the version
# a Version (what the gem's UserAgent.parse(...).version answers) and with
# the String the port's UserAgent#version answers. String#<=> compares a
# Version through its to_str, lexicographically, so the String rows differ
# from the Version rows ("126.0" is not >= "46"): that is the port gap
# runtime/spinel/user_agent_version.rb's header names, recorded here.
VERSION_46 = V.new("46")
VERSION_10 = V.new("10")
FALLBACK_VERSION = V.new("0")
["", "45.0", "46", "46.0.1", "126.0", "9.1", "10", "17.2", "Unknown"].each do |s|
  puts "secure_headers Version #{s.inspect}: #{outcome { [(V.new(s) || FALLBACK_VERSION) >= VERSION_46, (V.new(s) || FALLBACK_VERSION) >= VERSION_10] }}"
  puts "secure_headers String #{s.inspect}: #{outcome { [(s || FALLBACK_VERSION) >= VERSION_46, (s || FALLBACK_VERSION) >= VERSION_10] }}"
end
puts "secure_headers nil: #{outcome { (nil || FALLBACK_VERSION) >= VERSION_46 }}"

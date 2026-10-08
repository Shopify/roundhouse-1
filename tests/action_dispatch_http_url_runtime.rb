# Native Rails oracle (the Rails native Core runs; tests/support/
# rails_oracle.rb) for ActionDispatch::Http::URL's module functions
# against runtime/spinel/action_dispatch_http_url.rb.
#
#   ruby tests/action_dispatch_http_url_runtime.rb compare
#
# runs this script twice: `native` loads the pinned actionpack with
# activesupport's core_ext (Object#to_query, #to_param, as `require
# "rails/all"` does); `emitted` loads what the emitted CRuby tree loads:
# the overlay's Object#to_param / #blank? and the port, and no
# `to_query` reopen. Each prints one line per observation (`path_for`
# with scalar, nested, Array, nil and empty params, records answering
# `to_param`, escaped anchors; `full_url_for` across protocols, ports,
# hosts carrying a scheme or port, IP hosts, subdomain/domain options,
# userinfo, the two ArgumentErrors; `url_for`; the domain extractors);
# `compare` requires them equal.
require_relative "support/rails_oracle"

if ARGV.first == "compare"
  require "open3"
  outputs = ["native", "emitted"].map do |mode|
    stdout, stderr, status = Open3.capture3(RbConfig.ruby, File.expand_path(__FILE__), mode)
    abort "#{mode}: #{stderr}" unless status.success?
    stdout
  end
  native, emitted = outputs.map(&:lines)
  equal = native.zip(emitted).count { |x, y| x == y }
  unless outputs.uniq.length == 1
    diff = native.zip(emitted).each_with_index.reject { |(x, y), _| x == y }.first(8)
    abort "URL differs (#{equal}/#{native.length} observations equal, emitted has #{emitted.length}):\n" +
      diff.map { |(x, y), i| "line #{i + 1}\n  native:  #{x}  emitted: #{y}" }.join("\n")
  end
  puts outputs.first
  puts "observations: #{equal}/#{native.length} equal"
  puts "Native Rails and emitted ActionDispatch::Http::URL agree (#{RailsOracle.label})"
  exit
end

MODE = ARGV.first
abort "usage: #{$PROGRAM_NAME} compare | native | emitted" unless %w[native emitted].include?(MODE)

if MODE == "native"
  RailsOracle.activate("activesupport", "actionpack")
  require "active_support"
  require "active_support/core_ext"
  require "action_dispatch"
  require "action_dispatch/http/url"
else
  require_relative "../runtime/spinel/scaffold/ruby_overlay/runtime/active_support_core_ext"
  require_relative "../runtime/spinel/action_dispatch_http_url"
end

def show(label, value)
  puts "#{label}: #{value.inspect}"
end

def outcome
  value = yield
  "#{value.inspect} #{value.encoding if value.is_a?(String)}"
rescue Exception => e
  "#{e.class}: #{e.message.lines.first.chomp.inspect}"
end

# A record: its `to_param` is its id, which Object#to_s is not.
class Slugged
  def initialize(id)
    @id = id
  end

  def to_param
    "#{@id}-slug"
  end
end

URL = ActionDispatch::Http::URL

show("path only", outcome { URL.path_for(path: "/admin") })
show("no path", outcome { URL.path_for({}) })
show("script name", outcome { URL.path_for(script_name: "/shop/", path: "/admin") })
show("trailing slash empty", outcome { URL.path_for(path: "", trailing_slash: true) })
show("trailing slash kept", outcome { URL.path_for(path: "/a", trailing_slash: true) })
show("params scalar", outcome { URL.path_for(path: "/admin", params: { search: "search value" }) })
show("params nil anchor", outcome { URL.path_for(path: "/admin", params: {}, anchor: nil) })
show("params empty", outcome { URL.path_for(path: "/admin", params: {}) })
show("params sorted", outcome { URL.path_for(path: "/p", params: { b: 2, a: 1, "c" => true, d: false }) })
show("params nil dropped", outcome { URL.path_for(path: "/p", params: { a: nil, b: "x" }) })
show("params escaped", outcome { URL.path_for(path: "/p", params: { "q[x] y" => "a&b=c/d?e#f~g*h", u: "ünï" }) })
show("params nested", outcome { URL.path_for(path: "/p", params: { user: { name: "N", age: 3, tags: [] }, z: 1 }) })
show("params array", outcome { URL.path_for(path: "/p", params: { ids: [3, 1, 2] }) })
show("params array of hashes", outcome { URL.path_for(path: "/p", params: { rows: [{ b: 1, a: 2 }, {}, { c: nil }] }) })
show("params array nil", outcome { URL.path_for(path: "/p", params: { ids: [nil, 1] }) })
show("params empty array", outcome { URL.path_for(path: "/p", params: { ids: [], h: {} }) })
show("params nested nil", outcome { URL.path_for(path: "/p", params: { user: { name: nil } }) })
show("params record", outcome { URL.path_for(path: "/p", params: { shop: Slugged.new(7), list: [Slugged.new(1)] }) })
show("params non-hash", outcome { URL.path_for(path: "/p", params: "raw value") })
show("params non-hash nil", outcome { URL.path_for(path: "/p", params: nil) })
show("params non-hash array", outcome { URL.path_for(path: "/p", params: [1, 2]) })
show("params integer keys", outcome { URL.path_for(path: "/p", params: { 2 => "b", 1 => "a" }) })
show("params symbol value", outcome { URL.path_for(path: "/p", params: { s: :sym, f: 1.5 }) })
show("anchor", outcome { URL.path_for(path: "/admin", anchor: "anchor") })
show("anchor escaped", outcome { URL.path_for(path: "/a", anchor: "a b#c/d?e:f@g!h%i[j]ü") })
show("anchor record", outcome { URL.path_for(path: "/a", anchor: Slugged.new(3)) })
show("anchor symbol", outcome { URL.path_for(path: "/a", anchor: :top) })
show("anchor false", outcome { URL.path_for(path: "/a", anchor: false) })
show("anchor array", outcome { URL.path_for(path: "/a", anchor: ["x y", 2]) })
show("params then anchor", outcome { URL.path_for(path: "/admin", params: { search: "search value" }, anchor: "anchor") })
show("frozen path", outcome { URL.path_for(path: "/admin".freeze, params: { a: 1 }) })

show("full url", outcome { URL.full_url_for(path: "/admin", host: "www.shop1.com", protocol: "https", port: "80", params: { search: "search value" }, anchor: "anchor") })
show("full url default port https", outcome { URL.full_url_for(path: "/a", host: "example.org", protocol: "https", port: nil, params: {}, anchor: nil) })
show("full url https 443", outcome { URL.full_url_for(path: "/a", host: "example.org", protocol: "https", port: 443) })
show("full url http 80", outcome { URL.full_url_for(path: "/a", host: "example.org", protocol: "http", port: "80") })
show("full url http 8080", outcome { URL.full_url_for(path: "/a", host: "example.org", protocol: "http", port: 8080) })
show("full url no protocol", outcome { URL.full_url_for(path: "/a", host: "example.org") })
show("full url protocol colon", outcome { URL.full_url_for(path: "/a", host: "example.org", protocol: "ftp:") })
show("full url protocol slashes", outcome { URL.full_url_for(path: "/a", host: "example.org", protocol: "ftp://") })
show("full url protocol relative", outcome { URL.full_url_for(path: "/a", host: "example.org", protocol: "//", port: 80) })
show("full url protocol false", outcome { URL.full_url_for(path: "/a", host: "example.org", protocol: false) })
show("full url invalid protocol", outcome { URL.full_url_for(path: "/a", host: "example.org", protocol: "a:b") })
show("full url missing host", outcome { URL.full_url_for(path: "/a", host: nil) })
show("full url host scheme", outcome { URL.full_url_for(path: "/a", host: "http://example.org:3000") })
show("full url host scheme with protocol", outcome { URL.full_url_for(path: "/a", host: "http://example.org:3000", protocol: "https") })
show("full url host port, port key", outcome { URL.full_url_for(path: "/a", host: "example.org:3000", port: nil) })
show("full url ipv6", outcome { URL.full_url_for(path: "/a", host: "[::1]:3000") })
show("full url ip host", outcome { URL.full_url_for(path: "/a", host: "127.0.0.1", subdomain: "x") })
show("full url subdomain", outcome { URL.full_url_for(path: "/a", host: "www.example.com", subdomain: "api") })
show("full url subdomain false", outcome { URL.full_url_for(path: "/a", host: "www.example.com", subdomain: false) })
show("full url domain", outcome { URL.full_url_for(path: "/a", host: "www.example.com", domain: "example.org") })
show("full url tld", outcome { URL.full_url_for(path: "/a", host: "www.example.co.uk", subdomain: "x", tld_length: 2) })
show("full url userinfo", outcome { URL.full_url_for(path: "/a", host: "example.org", user: "a b@c", password: "p:w*~") })
show("full url user only", outcome { URL.full_url_for(path: "/a", host: "example.org", user: "a") })
show("full url script name", outcome { URL.full_url_for(script_name: "/s", path: "/a", host: "example.org", trailing_slash: true) })
show("url_for only_path", outcome { URL.url_for(only_path: true, path: "/a", host: "x.org", params: { a: 1 }) })
show("url_for full", outcome { URL.url_for(path: "/a", host: "x.org", anchor: "t") })

show("extract_domain", outcome { URL.extract_domain("dev.www.example.co.uk", 2) })
show("extract_domain ip", outcome { URL.extract_domain("127.0.0.1", 1) })
show("extract_subdomains", outcome { URL.extract_subdomains("dev.www.example.com", 1) })
show("extract_subdomains ip", outcome { URL.extract_subdomains("127.0.0.1", 1) })
show("extract_subdomain", outcome { URL.extract_subdomain("dev.www.example.co.uk", 2) })
show("tld_length", URL.tld_length)
show("secure_protocol", URL.secure_protocol)
show("domain_extractor", URL.domain_extractor.name)
URL.secure_protocol = true
show("full url secure default", outcome { URL.full_url_for(path: "/a", host: "example.org") })
URL.secure_protocol = false
show("private add_params", outcome { URL.add_params(+"/a", {}) })
show("escape_path", outcome { ActionDispatch::Journey::Router::Utils.escape_path("/a b/ü?#") })
show("escape_segment", outcome { ActionDispatch::Journey::Router::Utils.escape_segment("a/b c") })
show("escape_fragment", outcome { ActionDispatch::Journey::Router::Utils.escape_fragment("a b?/") })

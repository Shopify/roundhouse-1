# frozen_string_literal: true

# ActionDispatch::Http::URL's module functions (Shopify/rails
# 5813559f4aed, 8.2.0.alpha actionpack/lib/action_dispatch/http/url.rb):
# `url_for`, `full_url_for`, `path_for`, the domain extractors and the
# private host/protocol/port normalizers, verbatim; with the one
# Journey::Router::Utils escaper they reach (`escape_fragment`, and its
# `escape_path` / `escape_segment` siblings, journey/router/utils.rb).
# Core's WebUrlHelpersFactory builds every `<key>_path` / `<key>_url`
# helper through `path_for` and `full_url_for`.
#
# Not ported: the instance half of the module (`url`, `protocol`,
# `host_with_port`, …), which Rails mixes into ActionDispatch::Request;
# a request object here answers its own.
#
# Two substitutions, each the exact behavior the original reaches:
#
# - `params.to_query` / `value.to_param` are activesupport's core_ext
#   (object/to_query.rb, object/to_param.rb), which reopen Object, nil,
#   true, false, Array and Hash. This tree does not reopen those builtins
#   with `to_query` (nor Hash#to_param, which Rails aliases to it), so
#   the two methods are private here, by receiver kind, rule for rule.
# - `Rack::Utils.escape` (the userinfo of a full URL) is
#   `URI.encode_www_form_component` (rack/utils.rb), called directly so
#   the port does not load rack.
#
# Ruby family only, carried where app code names ActionDispatch::Http::URL
# (`project::ruby_family_files`).
require "cgi/escape"
require "uri"

module ActionDispatch
  module Journey # :nodoc:
    class Router # :nodoc:
      class Utils # :nodoc:
        # URI path and fragment escaping https://tools.ietf.org/html/rfc3986
        class UriEncoder # :nodoc:
          ENCODE   = "%%%02X"
          US_ASCII = Encoding::US_ASCII
          EMPTY    = (+"").force_encoding(US_ASCII).freeze
          DEC2HEX  = (0..255).map { |i| (ENCODE % i).force_encoding(US_ASCII).freeze }.freeze

          ALPHA = "a-zA-Z"
          DIGIT = "0-9"
          UNRESERVED = "#{ALPHA}#{DIGIT}\\-\\._~".freeze
          SUB_DELIMS = "!\\$&'\\(\\)\\*\\+,;="

          FRAGMENT = /[^#{UNRESERVED}#{SUB_DELIMS}:@\/?]/
          SEGMENT  = /[^#{UNRESERVED}#{SUB_DELIMS}:@]/
          PATH     = /[^#{UNRESERVED}#{SUB_DELIMS}:@\/]/

          def escape_fragment(fragment)
            escape(fragment, FRAGMENT)
          end

          def escape_path(path)
            escape(path, PATH)
          end

          def escape_segment(segment)
            escape(segment, SEGMENT)
          end

          private
            def escape(component, pattern)
              component.gsub(pattern) { |unsafe| percent_encode(unsafe) }.force_encoding(US_ASCII)
            end

            def percent_encode(unsafe)
              safe = EMPTY.dup
              unsafe.each_byte { |b| safe << DEC2HEX[b] }
              safe
            end
        end

        ENCODER = UriEncoder.new.freeze

        def self.escape_path(path)
          ENCODER.escape_path(path.to_s)
        end

        def self.escape_segment(segment)
          ENCODER.escape_segment(segment.to_s)
        end

        def self.escape_fragment(fragment)
          ENCODER.escape_fragment(fragment.to_s)
        end
      end
    end
  end

  module Http
    module URL
      IP_HOST_REGEXP  = /\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}$/
      HOST_REGEXP     = /(^[^:]+:\/\/)?(\[[^\]]+\]|[^:]+)(?::(\d+$))?/
      PROTOCOL_REGEXP = /^([^:]+)(:)?(\/\/)?$/

      # Extracts the domain and subdomains of a host string given the
      # number of top-level domain components (`tld_length`).
      module DomainExtractor
        extend self

        def domain_from(host, tld_length)
          host.split(".").last(1 + tld_length).join(".")
        end

        def subdomains_from(host, tld_length)
          parts = host.split(".")
          parts[0..-(tld_length + 2)]
        end
      end

      singleton_class.attr_accessor :secure_protocol, :tld_length, :domain_extractor
      self.secure_protocol = false
      self.tld_length = 1
      self.domain_extractor = DomainExtractor

      class << self
        def extract_domain(host, tld_length)
          extract_domain_from(host, tld_length) if named_host?(host)
        end

        def extract_subdomains(host, tld_length)
          if named_host?(host)
            extract_subdomains_from(host, tld_length)
          else
            []
          end
        end

        def extract_subdomain(host, tld_length)
          extract_subdomains(host, tld_length).join(".")
        end

        def url_for(options)
          if options[:only_path]
            path_for options
          else
            full_url_for options
          end
        end

        def full_url_for(options)
          host     = options[:host]
          protocol = options[:protocol]
          port     = options[:port]

          unless host
            raise ArgumentError, "Missing host to link to! Please provide the :host parameter, set default_url_options[:host], or set :only_path to true"
          end

          build_host_url(host, port, protocol, options, path_for(options))
        end

        def path_for(options)
          path = options[:script_name].to_s.chomp("/")
          path << options[:path] if options.key?(:path)

          path = +"/" if options[:trailing_slash] && path.blank?

          add_params(path, options[:params]) if options.key?(:params)
          add_anchor(path, options[:anchor]) if options.key?(:anchor)

          path
        end

        private
          def add_params(path, params)
            params = { params: params } unless params.is_a?(Hash)
            params = params.reject { |_, v| to_param(v).nil? }
            query = to_query(params)
            path << "?#{query}" unless query.empty?
          end

          def add_anchor(path, anchor)
            if anchor
              path << "##{Journey::Router::Utils.escape_fragment(to_param(anchor))}"
            end
          end

          def extract_domain_from(host, tld_length)
            domain_extractor.domain_from(host, tld_length)
          end

          def extract_subdomains_from(host, tld_length)
            domain_extractor.subdomains_from(host, tld_length)
          end

          def build_host_url(host, port, protocol, options, path)
            if match = host.match(HOST_REGEXP)
              protocol_from_host = match[1] if protocol.nil?
              host               = match[2]
              port               = match[3] unless options.key? :port
            end

            protocol = protocol_from_host || normalize_protocol(protocol).dup
            host     = normalize_host(host, options)
            port     = normalize_port(port, protocol)

            result = protocol

            if options[:user] && options[:password]
              result << "#{URI.encode_www_form_component(options[:user])}:#{URI.encode_www_form_component(options[:password])}@"
            end

            result << host

            result << ":" << port.to_s if port

            result.concat path
          end

          def named_host?(host)
            !IP_HOST_REGEXP.match?(host)
          end

          def normalize_protocol(protocol)
            case protocol
            when nil
              secure_protocol ? "https://" : "http://"
            when false, "//"
              "//"
            when PROTOCOL_REGEXP
              "#{$1}://"
            else
              raise ArgumentError, "Invalid :protocol option: #{protocol.inspect}"
            end
          end

          def normalize_host(_host, options)
            return _host unless named_host?(_host)

            tld_length = options[:tld_length] || self.tld_length
            subdomain  = options.fetch :subdomain, true
            domain     = options[:domain]

            host = +""
            if subdomain == true
              return _host if domain.nil?

              host << extract_subdomains_from(_host, tld_length).join(".")
            elsif subdomain
              host << to_param(subdomain)
            end
            host << "." unless host.empty?
            host << (domain || extract_domain_from(_host, tld_length))
            host
          end

          def normalize_port(port, protocol)
            return unless port

            case protocol
            when "//" then port
            when "https://"
              port unless port.to_i == 443
            else
              port unless port.to_i == 80
            end
          end

          # core_ext/object/to_param.rb: nil, true and false are
          # themselves, an Array joins its members' params with "/", a
          # Hash is its query string, anything else answers `to_param`
          # (a record's id) or, failing that, Object's `to_s`.
          def to_param(value)
            case value
            when nil, true, false then value
            when Array then value.collect { |element| to_param(element) }.join "/"
            when Hash then to_query(value)
            else value.respond_to?(:to_param) ? value.to_param : value.to_s
            end
          end

          # core_ext/object/to_query.rb: Hash#to_query(namespace),
          # Array#to_query(key), NilClass#to_query(key) and
          # Object#to_query(key), by receiver kind.
          def to_query(value, key = nil)
            case value
            when Hash
              query = value.filter_map do |k, v|
                next if (v.is_a?(Hash) || v.is_a?(Array)) && v.empty?
                to_query(v, key ? "#{key}[#{k}]" : k)
              end

              query.sort! unless key.to_s.include?("[]")
              query.join("&")
            when Array
              prefix = "#{key}[]"

              if value.empty?
                to_query(nil, prefix)
              else
                value.filter_map { |element|
                  next if (element.is_a?(Hash) || element.is_a?(Array)) && element.empty?
                  to_query(element, prefix)
                }.join "&"
              end
            when nil
              CGI.escape(to_param(key))
            else
              "#{CGI.escape(to_param(key))}=#{CGI.escape(to_param(value).to_s)}"
            end
          end
      end
    end
  end
end

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
require "strscan"
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

  # The route-pattern half of Journey (actionpack journey/scanner.rb,
  # parser.rb, nodes/node.rb and visitors.rb): `Parser.parse` a pattern
  # (`"(/:locale)/account/:id(.:format)"`) into its node tree, `Ast`
  # (path_params, root) and `Visitors::FormatBuilder` (a tree into the
  # `Format` that fills it from a params hash). Core's StorefrontUrlHelpers
  # generates its legacy storefront paths through them. Verbatim, but for
  # the visualizer (`Visitors::Dot`, `Node#to_dot`), the route table
  # (Route, Routes, the NFA/GTG matchers) and the router, which this tree
  # does not port.
  module Journey # :nodoc:
    class Scanner # :nodoc:
      STATIC_TOKENS = Array.new(150)
      STATIC_TOKENS[".".ord] = :DOT
      STATIC_TOKENS["/".ord] = :SLASH
      STATIC_TOKENS["(".ord] = :LPAREN
      STATIC_TOKENS[")".ord] = :RPAREN
      STATIC_TOKENS["|".ord] = :OR
      STATIC_TOKENS[":".ord] = :SYMBOL
      STATIC_TOKENS["*".ord] = :STAR
      STATIC_TOKENS.freeze

      class Scanner < StringScanner
        unless method_defined?(:peek_byte) # https://github.com/ruby/strscan/pull/89
          def peek_byte
            string.getbyte(pos)
          end
        end
      end

      def initialize
        @scanner = nil
        @length = nil
      end

      def scan_setup(str)
        @scanner = Scanner.new(str)
      end

      def next_token
        return if @scanner.eos?

        until token = scan || @scanner.eos?; end
        token
      end

      def last_string
        -@scanner.string.byteslice(@scanner.pos - @length, @length)
      end

      def last_literal
        last_str = @scanner.string.byteslice(@scanner.pos - @length, @length)
        last_str.tr! "\\", ""
        -last_str
      end

      private
        def scan
          next_byte = @scanner.peek_byte
          case
          when (token = STATIC_TOKENS[next_byte]) && (token != :SYMBOL || next_byte_is_not_a_token?)
            @scanner.pos += 1
            @length = @scanner.skip(/\w+/).to_i + 1 if token == :SYMBOL || token == :STAR
            token
          when @length = @scanner.skip(/(?:[\w%\-~!$&'*+,;=@]|\\[:()])+/)
            :LITERAL
          when @length = @scanner.skip(/./)
            :LITERAL
          end
        end

        def next_byte_is_not_a_token?
          !STATIC_TOKENS[@scanner.string.getbyte(@scanner.pos + 1)]
        end
    end

    class Ast # :nodoc:
      attr_reader :names, :path_params, :tree, :wildcard_options, :terminals
      alias :root :tree

      def initialize(tree, formatted)
        @tree = tree
        @path_params = []
        @names = []
        @symbols = []
        @stars = []
        @terminals = []
        @wildcard_options = {}

        visit_tree(formatted)
      end

      def requirements=(requirements)
        # inject any regexp requirements for `star` nodes so they can be determined
        # nullable, which requires knowing if the regex accepts an empty string.
        (symbols + stars).each do |node|
          re = requirements[node.to_sym]
          node.regexp = re if re
        end
      end

      def route=(route)
        terminals.each { |n| n.memo = route }
      end

      def glob?
        stars.any?
      end

      private
        attr_reader :symbols, :stars

        def visit_tree(formatted)
          tree.each do |node|
            if node.symbol?
              path_params << node.to_sym
              names << node.name
              symbols << node
            elsif node.star?
              stars << node

              if formatted != false
                # Add a constraint for wildcard route to make it non-greedy and match the
                # optional format part of the route by default.
                wildcard_options[node.name.to_sym] ||= /.+?/m
              end
            end

            if node.terminal?
              terminals << node
            end
          end
        end
    end

    module Nodes # :nodoc:
      class Node # :nodoc:
        include Enumerable

        attr_accessor :left, :memo

        def initialize(left)
          @left = left
          @memo = nil
          @to_s = nil
        end

        def each(&block)
          Visitors::Each::INSTANCE.accept(self, block)
        end

        def to_s
          @to_s ||= Visitors::String::INSTANCE.accept(self, "".dup).freeze
        end

        def to_sym
          name.to_sym
        end

        def name
          -left.tr("*:", "")
        end

        def type
          raise NotImplementedError
        end

        def symbol?; false; end
        def literal?; false; end
        def terminal?; false; end
        def star?; false; end
        def cat?; false; end
        def group?; false; end
      end

      class Terminal < Node # :nodoc:
        alias :symbol :left
        def terminal?; true; end
      end

      class Literal < Terminal # :nodoc:
        def literal?; true; end
        def type; :LITERAL; end
      end

      class Dummy < Literal # :nodoc:
        def initialize(x = Object.new)
          super
        end

        def literal?; false; end
      end

      class Slash < Terminal # :nodoc:
        def type; :SLASH; end
      end

      class Dot < Terminal # :nodoc:
        def type; :DOT; end
      end

      class Symbol < Terminal # :nodoc:
        attr_accessor :regexp
        alias :symbol :regexp
        attr_reader :name

        DEFAULT_EXP = /[^.\/?]+/
        GREEDY_EXP = /(.+)/
        def initialize(left, regexp = DEFAULT_EXP)
          super(left)
          @regexp = regexp
          @name = -left.tr("*:", "")
        end

        def type; :SYMBOL; end
        def symbol?; true; end
      end

      class Unary < Node # :nodoc:
        def children; [left] end
      end

      class Group < Unary # :nodoc:
        def type; :GROUP; end
        def group?; true; end
      end

      class Star < Unary # :nodoc:
        attr_accessor :regexp

        def initialize(left)
          super(left)

          # By default wildcard routes are non-greedy and must match something.
          @regexp = /.+?/m
        end

        def star?; true; end
        def type; :STAR; end

        def name
          left.name.tr "*:", ""
        end
      end

      class Binary < Node # :nodoc:
        attr_accessor :right

        def initialize(left, right)
          super(left)
          @right = right
        end

        def children; [left, right] end
      end

      class Cat < Binary # :nodoc:
        def cat?; true; end
        def type; :CAT; end
      end

      class Or < Node # :nodoc:
        attr_reader :children

        def initialize(children)
          @children = children
        end

        def type; :OR; end
      end
    end

    class Parser # :nodoc:
      include Journey::Nodes

      def self.parse(string)
        new.parse string
      end

      def initialize
        @scanner = Scanner.new
        @next_token = nil
      end

      def parse(string)
        @scanner.scan_setup(string)
        advance_token
        do_parse
      end

      private
        def advance_token
          @next_token = @scanner.next_token
        end

        def do_parse
          parse_expressions
        end

        def parse_expressions
          node = parse_expression

          while @next_token
            case @next_token
            when :RPAREN
              break
            when :OR
              node = parse_or(node)
            else
              node = Cat.new(node, parse_expressions)
            end
          end

          node
        end

        def parse_or(lhs)
          advance_token
          node = parse_expression
          Or.new([lhs, node])
        end

        def parse_expression
          if @next_token == :STAR
            parse_star
          elsif @next_token == :LPAREN
            parse_group
          else
            parse_terminal
          end
        end

        def parse_star
          node = Star.new(Symbol.new(@scanner.last_string, Symbol::GREEDY_EXP))
          advance_token
          node
        end

        def parse_group
          advance_token
          node = parse_expressions
          if @next_token == :RPAREN
            node = Group.new(node)
            advance_token
            node
          else
            raise ArgumentError, "missing right parenthesis."
          end
        end

        def parse_terminal
          node = case @next_token
          when :SYMBOL
            Symbol.new(@scanner.last_string)
          when :LITERAL
            Literal.new(@scanner.last_literal)
          when :SLASH
            Slash.new("/")
          when :DOT
            Dot.new(".")
          end

          advance_token
          node
        end
    end

    class Format
      ESCAPE_PATH    = ->(value) { Router::Utils.escape_path(value) }
      ESCAPE_SEGMENT = ->(value) { Router::Utils.escape_segment(value) }

      Parameter = Struct.new(:name, :escaper) do
        def escape(value); escaper.call value; end
      end

      def self.required_path(symbol)
        Parameter.new symbol, ESCAPE_PATH
      end

      def self.required_segment(symbol)
        Parameter.new symbol, ESCAPE_SEGMENT
      end

      def initialize(parts)
        @parts      = parts
        @children   = []
        @parameters = []

        parts.each_with_index do |object, i|
          case object
          when Journey::Format
            @children << i
          when Parameter
            @parameters << i
          end
        end
      end

      def evaluate(hash)
        parts = @parts.dup

        @parameters.each do |index|
          param = parts[index]
          value = hash[param.name]
          return "" if value.nil?
          parts[index] = param.escape value
        end

        @children.each { |index| parts[index] = parts[index].evaluate(hash) }

        parts.join
      end
    end

    module Visitors # :nodoc:
      class Visitor # :nodoc:
        DISPATCH_CACHE = {}

        def accept(node)
          visit(node)
        end

        private
          def visit(node)
            send(DISPATCH_CACHE[node.type], node)
          end

          def binary(node)
            visit(node.left)
            visit(node.right)
          end
          def visit_CAT(n); binary(n); end

          def nary(node)
            node.children.each { |c| visit(c) }
          end
          def visit_OR(n); nary(n); end

          def unary(node)
            visit(node.left)
          end
          def visit_GROUP(n); unary(n); end
          def visit_STAR(n); unary(n); end

          def terminal(node); end
          def visit_LITERAL(n); terminal(n); end
          def visit_SYMBOL(n);  terminal(n); end
          def visit_SLASH(n);   terminal(n); end
          def visit_DOT(n);     terminal(n); end

          private_instance_methods(false).each do |pim|
            next unless pim =~ /^visit_(.*)$/
            DISPATCH_CACHE[$1.to_sym] = pim
          end
      end

      class FunctionalVisitor # :nodoc:
        DISPATCH_CACHE = {}

        def accept(node, seed)
          visit(node, seed)
        end

        def visit(node, seed)
          send(DISPATCH_CACHE[node.type], node, seed)
        end

        def binary(node, seed)
          visit(node.right, visit(node.left, seed))
        end
        def visit_CAT(n, seed); binary(n, seed); end

        def nary(node, seed)
          node.children.inject(seed) { |s, c| visit(c, s) }
        end
        def visit_OR(n, seed); nary(n, seed); end

        def unary(node, seed)
          visit(node.left, seed)
        end
        def visit_GROUP(n, seed); unary(n, seed); end
        def visit_STAR(n, seed); unary(n, seed); end

        def terminal(node, seed);   seed; end
        def visit_LITERAL(n, seed); terminal(n, seed); end
        def visit_SYMBOL(n, seed);  terminal(n, seed); end
        def visit_SLASH(n, seed);   terminal(n, seed); end
        def visit_DOT(n, seed);     terminal(n, seed); end

        instance_methods(false).each do |pim|
          next unless pim.start_with?("visit_")
          DISPATCH_CACHE[pim.name.delete_prefix("visit_").to_sym] = pim
        end
      end

      class FormatBuilder < Visitor # :nodoc:
        def accept(node); Journey::Format.new(super); end
        def terminal(node); [node.left]; end

        def binary(node)
          visit(node.left) + visit(node.right)
        end

        def visit_GROUP(n); [Journey::Format.new(unary(n))]; end

        def visit_STAR(n)
          [Journey::Format.required_path(n.left.to_sym)]
        end

        def visit_SYMBOL(n)
          symbol = n.to_sym
          if symbol == :controller
            [Journey::Format.required_path(symbol)]
          else
            [Journey::Format.required_segment(symbol)]
          end
        end
      end

      # Loop through the requirements AST.
      class Each < FunctionalVisitor # :nodoc:
        def visit(node, block)
          block.call(node)
          super
        end

        INSTANCE = new
      end

      class String # :nodoc:
        def accept(node, seed)
          case node.type
          when :DOT
            seed << node.left
          when :LITERAL
            seed << node.left
          when :SYMBOL
            seed << node.left
          when :SLASH
            seed << node.left
          when :CAT
            accept(node.right, accept(node.left, seed))
          when :STAR
            accept(node.left, seed)
          when :OR
            last_child = node.children.last
            node.children.each do |c|
              accept(c, seed)
              seed << "|" unless last_child == c
            end
            seed
          when :GROUP
            accept(node.left, seed << "(") << ")"
          else
            raise "Unknown node type: #{node.type}"
          end
        end

        INSTANCE = new
      end
    end
  end

  module Http
    module URL
      IP_HOST_REGEXP  =/\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}$/
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

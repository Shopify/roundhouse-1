# frozen_string_literal: true

# ActiveSupport::Gzip, verbatim from the Rails Core runs, 8.2.0.alpha
# (activesupport/lib/active_support/gzip.rb, the pinned bundler/gems/rails
# checkout; Rails' doc comments dropped), for the CRuby and JRuby trees,
# carried where the app names the module (Core's
# `Webhooks::PayloadEnvelope#encode`/`.decode`). Each method follows that
# source; tests/active_support_utilities_runtime.rb holds it to native
# ActiveSupport.

require "zlib"
require "stringio"

module ActiveSupport
  module Gzip
    class Stream < StringIO
      def initialize(*)
        super
        set_encoding "BINARY"
      end
      def close; rewind; end
    end

    def self.decompress(source)
      Zlib::GzipReader.wrap(StringIO.new(source), &:read)
    end

    def self.compress(source, level = Zlib::DEFAULT_COMPRESSION, strategy = Zlib::DEFAULT_STRATEGY)
      output = Stream.new
      gz = Zlib::GzipWriter.new(output, level, strategy)
      gz.mtime = 0
      gz.write(source)
      gz.close
      output.string
    end
  end
end

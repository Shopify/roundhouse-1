//! Classes, modules and body statements written inside `class << self`.
//!
//! Ruby defines what such a body declares on the singleton class: Core's
//! `ElasticSearch::TaxCodeIndexing` keeps `TaxCodeSourceBuilder` in its
//! `class << self` and its class methods read it bare, so natively it is
//! `TaxCodeIndexing.singleton_class::TaxCodeSourceBuilder` and
//! `TaxCodeIndexing::TaxCodeSourceBuilder` is a NameError. A body
//! statement there (`%w[t translate].each { |m| define_method(m) … }`)
//! runs with the singleton class as `self`, so it defines class methods.
//!
//! The CRuby tree writes each back inside `class << self` and reads the
//! classes through `X.singleton_class`. CRuby running the authored source
//! is the oracle.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const SOURCE: &str = r##"module SingletonIndexing
  class << self
    def build(x) = SourceBuilder.call(x)
    def builder = SourceBuilder
    def inner = Builders::Inner.go
    def derived = Derived.new.kind

    module SourceBuilder
      class << self
        def call(x) = { id: x }
      end
    end

    module Builders
      class Inner
        def self.go = "inner"
      end
    end

    class Base
      def kind = "base"
    end

    class Derived < Base
    end
  end
end

class SingletonOwner
  class << self
    def helper = Helper.new.go

    class Helper
      def go = "go"
    end
  end
end

class SingletonTranslator
  class << self
    %w[t translate].each do |m|
      define_method(m) { |key| "#{m}:#{key}" }
    end
  end
end
"##;

/// Core's shape: a namespaced module whose `class << self` declares the
/// helper below a `private`, read bare by a private class method.
const NAMESPACED: &str = r#"module NsSearch
  module TaxIndexing
    ANALYZERS = ["a", "b"].freeze

    class << self
      def strategy
        document_builder.call(7)
      end

      private

      module TaxSourceBuilder
        class << self
          def call(id)
            { id: id, analyzers: ANALYZERS.size }
          end
        end
      end

      def document_builder
        @builder ||= TaxSourceBuilder
      end
    end
  end
end
"#;

const SCRIPT: &str = r#"p [SingletonIndexing.build(1), SingletonIndexing.builder.call(2), SingletonIndexing.inner, SingletonIndexing.derived]
p [SingletonIndexing.constants, SingletonIndexing.singleton_class.constants.sort]
p [(SingletonIndexing::SourceBuilder rescue :name_error), SingletonIndexing.singleton_class::Derived.superclass == SingletonIndexing.singleton_class::Base]
p [SingletonOwner.helper, SingletonOwner.constants, (SingletonOwner::Helper rescue :name_error)]
p [SingletonTranslator.t(:a), SingletonTranslator.translate(:b), SingletonTranslator.new.respond_to?(:t)]
p [NsSearch::TaxIndexing.strategy, NsSearch::TaxIndexing.singleton_class.constants, (NsSearch::TaxIndexing::TaxSourceBuilder rescue :name_error), (NsSearch::TaxIndexing.document_builder rescue :private)]
"#;

const EXPECTED: &str = "[{id: 1}, {id: 2}, \"inner\", \"base\"]\n\
[[], [:Base, :Builders, :Derived, :SourceBuilder]]\n\
[:name_error, true]\n\
[\"go\", [], :name_error]\n\
[\"t:a\", \"translate:b\", false]\n\
[{id: 7, analyzers: 2}, [:TaxSourceBuilder], :name_error, :private]\n";

#[test]
fn singleton_body_declarations_live_on_the_singleton_class_as_in_cruby() {
    let native = emit_and_run::ruby().arg("-e").arg(format!("{SOURCE}\n{NAMESPACED}\n{SCRIPT}")).output().expect("CRuby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    assert_eq!(String::from_utf8_lossy(&native.stdout), EXPECTED, "native control");

    let run = emit_and_run::real_blog()
        .write("app/services/singleton_indexing.rb", SOURCE)
        .write("app/services/ns_search/tax_indexing.rb", NAMESPACED)
        .run_ruby(SCRIPT);
    run.assert_passes();
    assert_eq!(run.stdout, EXPECTED, "{}", run.stderr);
}

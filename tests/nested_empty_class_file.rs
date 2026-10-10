//! Where an empty class declared in a namespace's own file is written.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const LAZY: &str = "Rails.application.configure do\n  config.eager_load = false\nend\n";

/// Core's checkouts/one/web/session/codec.rb: `class Session; module
/// Codec` declares `class UnsupportedCheckoutSessionError <
/// DecodingError; end` beside the codec table, which reads
/// `Session::CartNext`, a class session.rb defines at its end. Written
/// into session.rb's class body, the empty class's superclass read
/// loaded codec.rb while Session was still open, and the table raised
/// NameError for `Session::CartNext`.
#[test]
fn an_empty_class_in_a_namespace_file_stays_in_that_file() {
    let run = emit_and_run::empty_app()
        .write("config/environments/development.rb", LAZY)
        .write("app/models/web/base_session.rb", "module Web\n  class BaseSession\n    def kind = :base\n  end\nend\n")
        .write(
            "app/models/web/session.rb",
            "module Web\n  class Session < BaseSession\n    def self.decode(x) = Codec.decode(x)\n\n    def kind = :session\n\n    class CartNext < Session\n      def kind = :cart_next\n    end\n  end\nend\n",
        )
        .write(
            "app/models/web/session/codec.rb",
            "module Web\n  class Session\n    module Codec\n      class DecodingError < StandardError\n        def initialize(message = nil)\n          super(message)\n        end\n      end\n\n      class UnsupportedError < DecodingError; end\n      class EncodingError < StandardError; end\n\n      MAP = { cart_next: CartNextCodec }.freeze\n      CLASSES = { Session::CartNext => CartNextCodec }.freeze\n\n      def self.decode(x) = MAP.fetch(x).name\n    end\n  end\nend\n",
        )
        .write(
            "app/models/web/session/codec/cart_next_codec.rb",
            "module Web\n  class Session\n    module Codec\n      class CartNextCodec\n        def self.name = \"cart_next_codec\"\n      end\n    end\n  end\nend\n",
        )
        .run_ruby("p Web::Session.decode(:cart_next)\np Web::Session::Codec::UnsupportedError.superclass\np Web::Session::Codec::CLASSES.keys\n");
    assert!(run.success, "{}\n{}", run.stdout, run.stderr);
    assert_eq!(run.stdout, "\"cart_next_codec\"\nWeb::Session::Codec::DecodingError\n[Web::Session::CartNext]\n", "{}", run.stderr);
}

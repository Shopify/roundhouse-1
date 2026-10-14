//! A Concern included by a `Class.new(Base) { include Concern }` block
//! gives the class both halves of what ActiveSupport::Concern does: the
//! `included do class_attribute … end` body and an `extend` of the
//! Concern's `ClassMethods`
//! (`ingest::anonymous_class_concerns`; Shopify Core's
//! `GraphApi::UserError.generate_error_code` builds each error-code enum
//! this way and calls `error_enum.add_error_code`).

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const ERROR_CODES: &str = r##"module ErrorCodes
  extend ActiveSupport::Concern

  included do
    class_attribute :auto_generated, default: false
  end

  module ClassMethods
    def add_code(code)
      (codes << code).size
    end

    def codes = (@codes ||= [])
  end
end
"##;

const BUILDER: &str = r##"class Builder
  def self.build
    Class.new(Object) do
      include ErrorCodes
      self.auto_generated = true
    end
  end
end
"##;

const SCRIPT: &str = r##"enum = Builder.build
p enum.auto_generated
p enum.add_code(:a), enum.add_code(:b)
p enum.codes
p Builder.build.codes
"##;

#[test]
fn anonymous_class_includer_is_extended_with_concern_class_methods() {
    let run = emit_and_run::real_blog()
        .write("app/lib/error_codes.rb", ERROR_CODES)
        .write("app/lib/builder.rb", BUILDER)
        .run_ruby(SCRIPT);
    run.assert_passes();
    assert_eq!(run.stdout, "true\n1\n2\n[:a, :b]\n[]\n");
}

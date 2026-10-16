//! A Regexp constant interpolating other string constants of its module
//! (Core's identifications gem: `UTF8_REGEX = /[^[#{WORDS_REGEX}‍#{EMOJI_REGEX}]]+/u`)
//! is a real constant, not "constant not supported".
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const MODEL: &str = r##"module Handleish
  WORDS = '\p{Word}'
  EXTRA = '\u{00A9}\u{00AE}'
  PATTERN = /[^[#{WORDS}‍#{EXTRA}]]+/u

  def self.slug(input)
    input.gsub(PATTERN, "-")
  end
end
"##;

const TEST: &str = r##"require "test_helper"

class HandleishTest < ActiveSupport::TestCase
  test "interpolated regexp constant" do
    assert_equal "a-b", Handleish.slug("a!b")
    assert_equal "a©b", Handleish.slug("a©b")
  end
end
"##;

#[test]
fn interpolated_regexp_constant_is_supported() {
    emit_and_run::empty_app()
        .write("app/controllers/application_controller.rb", "class ApplicationController < ActionController::Base\nend\n")
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("db/schema.rb", "ActiveRecord::Schema.define do\nend\n")
        .write("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n")
        .write("app/models/handleish.rb", MODEL)
        .write("test/models/handleish_test.rb", TEST)
        .run_test("test/models/handleish_test.rb")
        .assert_passes();
}

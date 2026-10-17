//! `Mime::ALL`, the `*/*` wildcard actionpack defines beside the concrete
//! types (mime_type.rb:433-447), and `Type#html?` / `#all?`.
//!
//! Core's `RequestHelperMethods` (components/platform/essentials/lib/
//! patches/request.rb) loads with `HTML_FORMATS = [Mime[:html],
//! Mime::ALL].freeze #: [untyped, Mime::AllType]`; without the port the
//! constant raised at load and the whole boot stopped there. The expected
//! line is native actionpack 8.2.0.alpha's output for the same script.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const FORMATS: &str = r##"module RequestFormats
  HTML_FORMATS = [Mime[:html], Mime::ALL].freeze #: [untyped, Mime::AllType]

  def self.html?(format)
    HTML_FORMATS.include?(format)
  end
end
"##;

const SCRIPT: &str = r##"wild = Mime::Type.lookup("*/*")
p [Mime::ALL.to_s, Mime::ALL.all?, Mime::ALL.html?, Mime::AllType.instance.equal?(Mime::ALL),
   Mime::ALL.is_a?(Mime::Type), Mime[:html].html?, Mime[:json].html?, Mime[:html].all?,
   Mime::ALL == Mime[:html], Mime::ALL == wild,
   RequestFormats.html?(Mime[:html]), RequestFormats.html?(wild), RequestFormats.html?(Mime[:json])]
"##;

#[test]
fn mime_all_is_the_wildcard_type() {
    let out = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("test/test_helper.rb", "require \"active_support/test_case\"\n")
        .write("app/models/request_formats.rb", FORMATS)
        .run_ruby(SCRIPT);
    out.assert_passes();
    assert_eq!(
        out.stdout,
        "[\"*/*\", true, true, true, true, true, false, false, false, true, true, true, false]\n"
    );
}

/// actionpack's `Mime::Type#==` compares the type strings, so a String
/// equals the type it names in both directions: Core's
/// `request.media_type == Mime[:json]` is `String#==`, which defers to
/// the type's `to_str`. It was `other.is_a?(Mime::Type) && …`, a 415 on
/// every JSON post to a controller that checks the media type that way.
#[test]
fn a_string_equals_the_mime_type_it_names() {
    let out = emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("test/test_helper.rb", "require \"active_support/test_case\"\n")
        .run_ruby(
            "json = Mime[:json]\n\
             p [json == \"application/json\", \"application/json\" == json,\n\
             \x20  json == \"text/html\", \"text/html\" == json, json == nil, json == Mime[:json]]\n",
        );
    out.assert_passes();
    assert_eq!(out.stdout, "[true, true, false, false, false, true]\n");
}

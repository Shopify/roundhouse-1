//! A String `blank?`/`present?` read off a method call and used as a
//! guard (`return false unless s.user_id.present?`, `x if s.id.present?`)
//! is written as Rails' `blank?` on the CRuby tree like the local-variable
//! form, so a value that is not a String (an Integer id a test stores in
//! a typed column) answers as it does natively: present. The
//! `strip.empty?` form raised `undefined method 'strip' for an instance of
//! Integer` in Core's `LoginProtection#login_from_session`.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const LIB: &str = r##"class GuardProbe
  extend T::Sig

  attr_accessor :raw

  sig { returns(T.nilable(String)) }
  def user_id
    @raw
  end

  sig { returns(T.any(T::Boolean, String)) }
  def check
    return false unless user_id.present?

    "present"
  end

  sig { returns(T.nilable(String)) }
  def modifier
    user_id if user_id.present?
  end
end
"##;

fn app() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\n")
        .write("app/.keep", "")
        .write("test/test_helper.rb", "require \"active_support/test_case\"\n")
        .write("lib/guard_probe.rb", LIB)
}

#[test]
fn a_guard_on_a_method_call_blank_is_rails_blank() {
    let out = app().run_ruby(
        "g = GuardProbe.new\ng.raw = 12\np g.check\np g.modifier\ng.raw = \"\"\np g.check\np g.modifier\ng.raw = nil\np g.check\ng.raw = \"x\"\np g.check\n",
    );
    out.assert_passes();
    assert_eq!(out.stdout.trim(), "\"present\"\n12\nfalse\nnil\nfalse\n\"present\"", "{}", out.stdout);
}

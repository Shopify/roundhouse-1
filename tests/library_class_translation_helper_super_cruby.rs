//! A class that includes `ActionView::Helpers::TranslationHelper` and a
//! module whose `translate` wraps it with a bare `super`.
//!
//! Core's `Home::SafeTranslatable::Helper` does exactly that
//! (`ShopifyI18n::ActionViewSoftDegrade#translate` rescues missing
//! translations around `super`). Nothing is called on the class itself
//! for the view-helper include scan to see, so the include was dropped as
//! unused and the `super` raised `no superclass method 'translate'`.
//!
//! CRuby with actionview is the oracle.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const SOURCE: &str = r##"module SoftDegrade
  def translate(key, **options)
    super
  rescue I18n::MissingTranslationData
    "degraded #{key}"
  end

  alias_method :t, :translate
end

class SafeHelper
  include ActionView::Helpers::TranslationHelper
  include SoftDegrade
end
"##;

const SCRIPT: &str = r#"require "i18n"
I18n.enforce_available_locales = false
I18n.backend.store_translations(:en, hello: "Hi %{name}", note_html: "<b>x</b>", nested: { deep: "Deep" })
h = SafeHelper.new
p h.translate("hello", name: "Ada")
p h.t(:nested)
p h.t("nested.deep")
p h.t("note_html").html_safe?
p SafeHelper.ancestors.first(3).map(&:name)
ActionView::Helpers::TranslationHelper.raise_on_missing_translations = true
p h.t("nope")
"#;

const EXPECTED: &str = "\"Hi Ada\"\n{deep: \"Deep\"}\n\"Deep\"\ntrue\n[\"SafeHelper\", \"SoftDegrade\", \"ActionView::Helpers::TranslationHelper\"]\n\"degraded nope\"\n";

#[test]
fn a_super_into_an_included_translation_helper_resolves_as_in_cruby() {
    let native = emit_and_run::ruby()
        .arg("-e")
        .arg(format!("require \"action_view\"\n{SOURCE}\n{SCRIPT}"))
        .output()
        .expect("CRuby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    assert_eq!(String::from_utf8_lossy(&native.stdout), EXPECTED, "native control");

    let run = emit_and_run::real_blog().write("app/services/safe_helper.rb", SOURCE).run_ruby(SCRIPT);
    run.assert_passes();
    assert_eq!(run.stdout, EXPECTED, "{}", run.stderr);
}

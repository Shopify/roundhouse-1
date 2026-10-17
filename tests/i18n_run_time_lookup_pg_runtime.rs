//! `I18n.t` with a key or scope computed at run time, a `locale:`, or a
//! key that can reach more translations than the compile-time fold will
//! branch over (Core's `Inventory::AdjustmentPresenter`,
//! `StandardMetafieldDefinitionTemplate#name`,
//! `Launch::ShopConversion.platforms_for_locale`). Analyze refused each
//! ("I18n not supported (all targets)") and the emitted body raised, though
//! the PostgreSQL CRuby tree loads the i18n gem and the app's locale files.
//! The call now stays the gem's `I18n.t`.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;
#[path = "support/pg_probe.rs"]
mod pg_probe;

const DATABASE_YML: &str = r#"default: &default
  adapter: postgresql
  encoding: unicode

test:
  <<: *default
  database: rh_e2e_i18n_run_time_lookup
"#;

const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    concurrent-ruby (1.3.5)\n    i18n (1.15.2)\n      concurrent-ruby (~> 1.0)\n\nDEPENDENCIES\n  i18n\n";

const APPLICATION_RECORD: &str = r#"class ApplicationRecord < ActiveRecord::Base
  self.abstract_class = true
end
"#;

const EN: &str = r#"en:
  probe:
    templates:
      color:
        name: "Color"
      size:
        name: "Size"
    reasons:
      initial: "Initial inventory"
      manual: "Manually adjusted"
    platforms:
      shopify: "Shopify"
      other: "Other"
    events:
      shipped:
        by_staff:
          one: "%{author} shipped %{count} item"
          other: "%{author} shipped %{count} items"
"#;

const FR: &str = r#"fr:
  probe:
    platforms:
      shopify: "Shopify (fr)"
      other: "Autre"
"#;

/// 450 keys under one prefix: past the fold's reach limit.
fn wide_locale() -> String {
    let mut out = String::from("en:\n  probe:\n    wide:\n");
    for i in 0..450 {
        out.push_str(&format!("      k{i}: \"wide {i}\"\n"));
    }
    out
}

const MODEL: &str = r##"class Template
  attr_reader :namespace, :key

  def initialize(namespace, key)
    @namespace = namespace
    @key = key
  end

  # StandardMetafieldDefinitionTemplate#name: a key computed at run time.
  def name
    I18n.t("probe.templates.#{namespace}.#{key}.name")
  end

  # AuditTrail::Adjustment#label: a key that is a bare variable, and a
  # `default:` that is not a literal.
  def label(label_key)
    I18n.t(label_key, scope: "probe.reasons", default: I18n.t(label_key, scope: "probe.platforms"))
  end

  # AdjustmentPresenter: a scope computed at run time.
  def reason(code, scope)
    I18n.t(code, scope: scope)
  end

  # ShopConversion.platforms_for_locale: `locale:`.
  def self.platforms_for_locale(locale)
    [:shopify, :other].each_with_object({}) do |platform, hash|
      hash[platform] = I18n.t(platform, scope: "probe.platforms", locale: locale)
    end.freeze
  end

  # TransferShipmentItemQuantitiesUpdated#message: a key built from
  # run-time parts, `count:` and a trailing call.
  def shipped(action, who, count)
    I18n.t("events.#{action}.by_staff", scope: "probe", author: who, count: count).upcase_first
  end

  # A key that can reach more translations than the fold branches over.
  def wide(n)
    I18n.t("probe.wide.k#{n}")
  end

  # A literal key: the gem's call, so the locale at call time decides.
  def folded
    I18n.t("probe.reasons.initial")
  end

  def localized
    I18n.t("probe.platforms.other")
  end
end
"##;

const PROBE: &str = r##"class I18nProbe
  def self.lines
    # The native side boots no application, so it has no railtie to load
    # config/locales; the emitted tree's railtie already did (a no-op here).
    I18n.load_path |= Dir[File.join(Dir.pwd, "config/locales/**/*.yml")]
    t = Template.new("color", "")
    out = []
    out << "key: #{Template.new("size", "x").name rescue $!.class}"
    out << "key found: #{I18n.t("probe.templates.color.name")}"
    out << "label: #{t.label(:manual)} #{t.label(:shopify)}"
    out << "scope: #{Template.new("a", "b").reason(:initial, "probe.reasons")}"
    out << "scope array: #{Template.new("a", "b").reason(:manual, ["probe", "reasons"])}"
    out << "locale en: #{Template.platforms_for_locale(:en).inspect}"
    out << "locale fr: #{Template.platforms_for_locale(:fr).inspect}"
    out << "plural one: #{t.shipped("shipped", "Ana", 1)}"
    out << "plural many: #{t.shipped("shipped", "Ana", 3)}"
    out << "wide: #{t.wide(449)}"
    out << "folded: #{t.folded}"
    out << "with_locale fr: #{I18n.with_locale(:fr) { t.localized }} #{t.localized}"
    out << "missing: #{Template.new("nope", "none").name}"
    out
  end
end
"##;

const EXPECTED: [&str; 13] = [
    r#"key: Translation missing: en.probe.templates.size.x.name"#,
    r#"key found: Color"#,
    r#"label: Manually adjusted Shopify"#,
    r#"scope: Initial inventory"#,
    r#"scope array: Manually adjusted"#,
    r#"locale en: {shopify: "Shopify", other: "Other"}"#,
    r#"locale fr: {shopify: "Shopify (fr)", other: "Autre"}"#,
    r#"plural one: Ana shipped 1 item"#,
    r#"plural many: Ana shipped 3 items"#,
    r#"wide: wide 449"#,
    r#"folded: Initial inventory"#,
    r#"with_locale fr: Autre Other"#,
    r#"missing: Translation missing: en.probe.templates.nope.none.name"#,
];

fn files() -> Vec<(String, String)> {
    let mut files: Vec<(String, String)> = [
        ("roundhouse.yml", "database_adapter: postgresql\n"),
        ("Gemfile.lock", LOCK),
        ("config/database.yml", DATABASE_YML),
        ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
        ("db/structure.sql", "\n"),
        ("config/locales/en.yml", EN),
        ("config/locales/fr.yml", FR),
        ("app/models/application_record.rb", APPLICATION_RECORD),
        ("app/models/template.rb", MODEL),
        ("app/models/i18n_probe.rb", PROBE),
    ]
    .iter()
    .map(|(p, t)| (p.to_string(), t.to_string()))
    .collect();
    files.push(("config/locales/wide.en.yml".into(), wide_locale()));
    files
}

const LOADS: [&str; 3] = ["app/models/application_record.rb", "app/models/template.rb", "app/models/i18n_probe.rb"];

fn overlay() -> emit_and_run::Overlay {
    files().iter().fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text))
}

fn real_errors(errors: &[String]) -> Vec<&String> {
    errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect()
}

/// No refusal, and every call stays a call to the gem's `I18n.t`.
#[test]
fn an_unfoldable_translate_stays_the_gems_call() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    let model = std::fs::read_to_string(emitted.join("app/models/template.rb")).unwrap();
    assert!(!model.contains("I18n not supported"), "{model}");
    assert!(model.contains("I18n.t(label_key, scope: \"probe.reasons\", default: I18n.t(label_key, scope: \"probe.platforms\"))"), "{model}");
    assert!(model.contains("I18n.t(code, scope: scope)"), "{model}");
    assert!(model.contains("locale: locale"), "{model}");
    // Nothing folds on the gem tree: the locale at call time decides.
    assert!(model.contains("I18n.t(\"events.#{action}.by_staff\", scope: \"probe\", author: who, count: count)"), "{model}");
    assert!(model.contains("I18n.t(\"probe.wide.k#{n}\")"), "{model}");
    // A literal key stays the gem's call, so `I18n.with_locale(:fr) { I18n.t(k) }` answers in French.
    assert!(model.contains("I18n.t(\"probe.reasons.initial\")"), "{model}");
    assert!(!model.contains("\"Initial inventory\""), "{model}");
}

/// The gem tree's runtime/i18n_locale.rb loads the gem, then defines the
/// helpers a folded call reaches for (a missing key, a run-time key).
#[test]
fn the_gem_tree_carries_the_folded_call_helpers() {
    let (emitted, _) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    let anchor = std::fs::read_to_string(emitted.join("runtime/i18n_locale.rb")).unwrap();
    assert!(anchor.contains("require_relative \"active_support_i18n\""), "{anchor}");
    for helper in ["missing_raises", "normalize_translation_key", "subtree_translation", "missing_translation", "missing_translation_tag"] {
        assert!(anchor.contains(&format!("def self.{helper}")), "{helper}\n{anchor}");
    }
    assert!(!anchor.contains("def self.locale"), "{anchor}");
}

/// Without the i18n gem in the lockfile the call has no run-time
/// answer, so it stays refused by name.
#[test]
fn without_the_gem_it_is_still_refused_by_name() {
    let app = files().iter().filter(|(p, _)| p != "Gemfile.lock").fold(emit_and_run::empty_app(), |app, (p, t)| app.write(p, t));
    let (_, errors) = app.emit(roundhouse::project::BuildTarget::Ruby);
    assert!(errors.iter().any(|e| e.contains("I18n not supported") && e.contains("a key computed at run time")), "{}", errors.join("\n"));
}

/// The same app natively, on Rails 8.2, and emitted.
#[test]
#[ignore = "requires pg gem, Core's pinned Rails and gem home, and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn run_time_translations_answer_like_rails_on_postgresql() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    let all = files();
    let native_files: Vec<(&str, &str)> = all.iter().map(|(p, t)| (p.as_str(), t.as_str())).collect();
    let (native, emitted_lines) =
        pg_probe::native_and_emitted("i18n-run-time-lookup", &native_files, &emitted, "rh_e2e_i18n_run_time_lookup", "I18nProbe", &LOADS);
    assert_eq!(native, EXPECTED, "native");
    assert_eq!(emitted_lines, native, "emitted");
}

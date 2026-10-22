//! `numericality:` with `allow_nil:` / `allow_blank:` lowers to a rule
//! instead of refusing the whole `validates` call. The flags come from
//! the validator's own hash or from the call, and the hash wins.

use roundhouse::dialect::ValidationRule;

const MODEL: &str = r#"class Widget < ApplicationRecord
  validates :min_favs, numericality: { greater_than_or_equal_to: 1, allow_nil: true }
  validates :age, numericality: { only_integer: true }, allow_blank: true
  validates :rank, numericality: { allow_nil: false }, allow_nil: true
  validates :level, numericality: true, allow_nil: true
end
"#;

fn ingest() -> (roundhouse::Model, Vec<String>) {
    roundhouse::ingest::survey::activate();
    let (model, diags) = roundhouse::ingest::prism::scope(|| {
        roundhouse::ingest::ingest_model(MODEL.as_bytes(), "app/models/widget.rb", &roundhouse::schema::Schema::default(), &Default::default())
            .unwrap()
            .unwrap()
    });
    let _ = roundhouse::ingest::survey::drain();
    let warnings = diags
        .into_iter()
        .filter(|d| d.severity == roundhouse::diagnostic::Severity::Warning)
        .map(|d| d.message)
        .collect();
    (model, warnings)
}

fn numericality<'a>(model: &'a roundhouse::Model, attr: &str) -> Option<&'a ValidationRule> {
    model.body.iter().find_map(|item| match item {
        roundhouse::dialect::ModelBodyItem::Validation { validation, .. } if validation.attribute.as_str() == attr => {
            validation.rules.iter().find(|rule| matches!(rule, ValidationRule::Numericality { .. }))
        }
        _ => None,
    })
}

#[test]
fn allow_nil_and_allow_blank_lower_to_a_numericality_rule() {
    let (model, warnings) = ingest();
    assert!(warnings.is_empty(), "{warnings:?}");
    for attr in ["min_favs", "age", "rank", "level"] {
        assert!(numericality(&model, attr).is_some(), "{attr} lowers: {:?}", model.body);
    }
}

/// On an attribute the model does not type as a number (an `attr_accessor`
/// of an `ActiveModel::Model` form, fed request Strings), `allow_nil:` /
/// `allow_blank:` with `numericality:` stays the reported gap: the lowered
/// check does not parse a String number (#1158), so it would reject `"30"`
/// where Rails accepts it. The same options on an integer column lower.
#[test]
fn on_an_untyped_attribute_the_options_stay_a_gap() {
    use std::collections::HashMap;
    use std::path::PathBuf;
    let files: Vec<(&str, &str)> = vec![
        ("db/schema.rb", "ActiveRecord::Schema.define do\n  create_table \"policies\", force: :cascade do |t|\n    t.integer \"age\"\n  end\nend\n"),
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  primary_abstract_class\nend\n"),
        ("app/models/policy.rb", "class Policy < ApplicationRecord\n  validates :age, numericality: { only_integer: true }, allow_blank: true\nend\n"),
        (
            "app/models/settings_form.rb",
            "class SettingsForm\n  include ActiveModel::Model\n  attr_accessor :age, :limit, :count, :level\n  validates :age, numericality: { only_integer: true }, allow_blank: true\n  validates :limit, numericality: { allow_nil: true }\n  validates :count, presence: true, numericality: { allow_nil: true }\n  validates :level, presence: { if: :ready? }, numericality: { allow_nil: true }\n  def ready? = true\nend\n",
        ),
    ];
    let tree: HashMap<PathBuf, Vec<u8>> = files.into_iter().map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec())).collect();
    roundhouse::ingest::survey::activate();
    let app = roundhouse::ingest::ingest_app_from_tree(tree).expect("ingest");
    let gaps: Vec<String> = roundhouse::ingest::survey::drain().into_iter().map(|g| g.to_string()).collect();
    let has_numericality = |model: &str, attr: &str| {
        app.models.iter().find(|m| m.name.0.as_str() == model).is_some_and(|m| {
            m.body.iter().any(|item| matches!(item,
                roundhouse::dialect::ModelBodyItem::Validation { validation, .. }
                    if validation.attribute.as_str() == attr
                        && validation.rules.iter().any(|r| matches!(r, ValidationRule::Numericality { .. }))))
        })
    };
    assert!(has_numericality("Policy", "age"), "an integer column lowers: {gaps:?}");
    assert!(!has_numericality("SettingsForm", "age"), "{gaps:?}");
    assert!(!has_numericality("SettingsForm", "limit"), "{gaps:?}");
    for gap in ["validates :age, numericality: allow_blank:", "validates :limit, numericality: allow_nil:"] {
        assert!(gaps.iter().any(|g| g.contains(gap) && g.contains("settings_form.rb") && g.contains("#1158")), "{gap}: {gaps:?}");
    }
    assert!(!gaps.iter().any(|g| g.contains("policy.rb")), "{gaps:?}");
    // The whole call goes, as an ingest refusal does: `presence:` beside
    // the numericality is not kept, nor is the item a per-rule `if:` split
    // off the same call.
    let form = app.models.iter().find(|m| m.name.0.as_str() == "SettingsForm").expect("form");
    for attr in ["count", "level"] {
        assert!(
            !form.body.iter().any(|item| matches!(item,
                roundhouse::dialect::ModelBodyItem::Validation { validation, .. } if validation.attribute.as_str() == attr)),
            "no validation left for {attr}: {:?}",
            form.body
        );
    }
}

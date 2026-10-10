//! typed_store accessor synthesis (`lower::typed_store::
//! push_typed_store_methods`) — the shared model lowering synthesizes
//! per-attribute reader/predicate/writer methods routing through the
//! `TypedStore` runtime (YAML seam), for every target. Shape tests at
//! the IR level plus a ruby-emit render check.

use roundhouse::dialect::MethodReceiver;
use roundhouse::ingest::ingest_app_from_tree;
use roundhouse::lower::lower_model_to_library_class;

fn lobsters_like_app() -> roundhouse::App {
    let files: Vec<(&str, &str)> = vec![
        (
            "db/schema.rb",
            "ActiveRecord::Schema.define(version: 1) do\n  create_table :users do |t|\n    t.text :settings\n  end\nend\n",
        ),
        (
            "app/models/user.rb",
            r#"class User < ApplicationRecord
  typed_store :settings do |s|
    s.string :prefers_color_scheme, :default => "system"
    s.string :totp_secret
    s.boolean :email_notifications, :default => false
  end

  def totp_secret
    "custom"
  end
end
"#,
        ),
    ];
    let tree = files
        .into_iter()
        .map(|(p, c)| (std::path::PathBuf::from(p), c.as_bytes().to_vec()))
        .collect();
    ingest_app_from_tree(tree).expect("ingest tree")
}

#[test]
fn typed_store_attrs_synthesize_shared_accessors() {
    let app = lobsters_like_app();
    let user = app.models.iter().find(|m| m.name.0.as_str() == "User").expect("User model");
    let lc = lower_model_to_library_class(user, &app.schema);

    let find = |name: &str| {
        lc.methods
            .iter()
            .find(|m| m.name.as_str() == name && m.receiver == MethodReceiver::Instance)
    };

    // String attr with default: reader + writer, no predicate.
    let reader = find("prefers_color_scheme").expect("reader synthesized");
    let body = format!("{:?}", reader.body);
    assert!(
        body.contains("TypedStore") && body.contains("read"),
        "reader must route through TypedStore.read: {body}",
    );
    assert!(
        body.contains("system"),
        "reader must carry the declared default: {body}",
    );
    assert!(find("prefers_color_scheme=").is_some(), "writer synthesized");
    // The typed_store gem generates a `?` predicate for EVERY attr;
    // non-booleans get the typed nil-check (users/show's
    // keybase_signatures? drove the widening).
    let pred = find("prefers_color_scheme?").expect("predicate synthesized for non-bool attr");
    let pbody = format!("{:?}", pred.body);
    assert!(
        pbody.contains("nil?"),
        "non-bool predicate is the nil-check form:\n{pbody}"
    );

    // Boolean attr: reader + predicate + writer.
    assert!(find("email_notifications").is_some());
    assert!(
        find("email_notifications?").is_some(),
        "boolean attr must get the `?` predicate",
    );
    let writer = find("email_notifications=").expect("writer synthesized");
    let wbody = format!("{:?}", writer.body);
    assert!(
        wbody.contains("write"),
        "writer must route through TypedStore.write: {wbody}",
    );
    assert!(writer.mutates_self, "writer mutates the store column");

    // Custom method in the model body WINS — the synthesized reader
    // must yield (push_user_methods runs after synthesis and drops
    // collisions, so a synthesized duplicate would shadow the user's).
    let totp = find("totp_secret").expect("user method present");
    let tbody = format!("{:?}", totp.body);
    assert!(
        tbody.contains("custom") && !tbody.contains("TypedStore"),
        "user-defined totp_secret must win over synthesis: {tbody}",
    );
    // Its writer (no user def) still synthesizes.
    assert!(find("totp_secret=").is_some(), "writer for user-shadowed attr still synthesized");
}

#[test]
fn typed_store_accessors_render_on_the_ruby_tree() {
    let app = lobsters_like_app();
    let files = roundhouse::emit::ruby::emit_lowered_models(&app);
    let user_src = files
        .iter()
        .find(|f| f.path.to_string_lossy().ends_with("models/user.rb"))
        .map(|f| f.content.clone())
        .expect("user.rb emitted");
    assert!(
        user_src.contains(r#"TypedStore.read(@settings, "prefers_color_scheme", "system")"#),
        "reader body must render the overlay call: {user_src}",
    );
    assert!(
        user_src
            .contains(r#"@settings = TypedStore.write(@settings, "prefers_color_scheme", value)"#),
        "writer body must render the assign-back form: {user_src}",
    );
    // A BOOLEAN attr's writer is the coercion boundary: the settings
    // form assigns `"0"`/`"1"` strings, and the reader beside it is
    // declared `bool`. Storing the String verbatim made that signature
    // a lie — Rails casts here, and so do we.
    assert!(
        user_src.contains(
            r#"TypedStore.write(@settings, "email_notifications", value.to_s != "0" && value.to_s != "" && value.to_s != "false")"#
        ),
        "boolean writer must cast rather than store the form value verbatim: {user_src}",
    );
}

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

/// The store column's own reader answers the gem's Hash of the declared
/// attributes, at their defaults until written, and `[]=` writes back
/// into the column the record saves. Core's `Shop#plan_name` reads
/// `settings[:plan_name]` and its tests write
/// `shop.settings[:gift_cards_activated] = true`; the column reader
/// answered the serialized String: TypeError (no implicit conversion of
/// Symbol into Integer).
#[test]
fn the_store_column_reads_and_writes_as_a_hash() {
    emit_and_run::empty_app()
        .write(
            "db/schema.rb",
            "ActiveRecord::Schema.define do\n  create_table :shops do |t|\n    t.string :name\n    t.text :settings\n  end\nend\n",
        )
        .write(
            "app/models/application_record.rb",
            "class ApplicationRecord < ActiveRecord::Base\n  primary_abstract_class\nend\n",
        )
        .write(
            "app/models/shop.rb",
            r#"class Shop < ApplicationRecord
  DEFAULT_TIMEZONE = "Eastern Time (US & Canada)"

  typed_store :settings do |s|
    s.string :timezone, blank: false, default: DEFAULT_TIMEZONE
    s.string :plan_name
    s.boolean :gift_cards_activated, default: false
  end

  def plan_name
    settings[:plan_name] || "basic"
  end

  def zone
    settings[:timezone]
  end
end
"#,
        )
        .run_ruby(
            r#"
shop = Shop.new(name: "snow")
raise "default plan" unless shop.plan_name == "basic"
raise "declared default #{shop.zone.inspect}" unless shop.zone == "Eastern Time (US & Canada)"
raise "bool default" unless shop.settings[:gift_cards_activated] == false
raise "key? declared" unless shop.settings.key?(:plan_name) && shop.settings.key?("timezone")
shop.settings[:gift_cards_activated] = "1"
shop.settings["plan_name"] = "plus"
raise "write through #{shop.plan_name.inspect}" unless shop.plan_name == "plus"
raise "cast bool" unless shop.gift_cards_activated == true
raise "indifferent" unless shop.settings.fetch("gift_cards_activated") == true
shop.save!
again = Shop.find(shop.id)
raise "persisted #{again.settings.to_h.inspect}" unless again.plan_name == "plus" && again.gift_cards_activated
raise "to_h" unless again.settings.to_h["timezone"] == "Eastern Time (US & Canada)"
puts "PASS"
"#,
        )
        .assert_passes();
}

/// Block locals referenced by a default (Core's Shop does
/// `default_money_format = "${{amount}}"` then `s.string :money_format,
/// default: default_money_format`). The gem evaluates the default once
/// at declaration; the accessor must carry the value, not a bare call
/// to a local that does not exist inside the reader.
#[test]
fn typed_store_defaults_inline_block_locals() {
    let files: Vec<(&str, &str)> = vec![
        (
            "db/schema.rb",
            "ActiveRecord::Schema.define(version: 1) do\n  create_table :shops do |t|\n    t.text :settings\n  end\nend\n",
        ),
        (
            "app/models/shop.rb",
            r#"class Shop < ApplicationRecord
  typed_store :settings do |s|
    default_money_format = "${{amount}}"
    tz = "Eastern Time (US & Canada)"
    s.string :money_format, default: default_money_format
    s.string :timezone, default: tz
  end
end
"#,
        ),
    ];
    let tree = files
        .into_iter()
        .map(|(p, c)| (std::path::PathBuf::from(p), c.as_bytes().to_vec()))
        .collect();
    let app = ingest_app_from_tree(tree).expect("ingest tree");
    let src = roundhouse::emit::ruby::emit_lowered_models(&app)
        .into_iter()
        .find(|f| f.path.to_string_lossy().ends_with("models/shop.rb"))
        .map(|f| f.content)
        .expect("shop.rb emitted");
    assert!(
        src.contains(r#"TypedStore.read(@settings, "money_format", "${{amount}}")"#),
        "default must inline the block local's value: {src}",
    );
    assert!(
        src.contains(r#"TypedStore.read(@settings, "timezone", "Eastern Time (US & Canada)")"#),
        "default must inline the block local's value: {src}",
    );
    assert!(!src.contains("default_money_format"), "no bare local call left: {src}");
}

/// A store with no boolean attribute has nothing to cast: the cast
/// helper must still be Ruby (`case key else value end` does not parse;
/// Core's Dns::Record and GiftCardConfiguration).
#[test]
fn a_store_without_boolean_attributes_reads_as_a_hash() {
    emit_and_run::empty_app()
        .write(
            "db/schema.rb",
            "ActiveRecord::Schema.define do\n  create_table :records do |t|\n    t.text :data\n  end\nend\n",
        )
        .write(
            "app/models/application_record.rb",
            "class ApplicationRecord < ActiveRecord::Base\n  primary_abstract_class\nend\n",
        )
        .write(
            "app/models/record.rb",
            r#"class Record < ApplicationRecord
  typed_store :data do |s|
    s.string :value
  end

  def shown
    data[:value] || "none"
  end
end
"#,
        )
        .run_ruby(
            r#"
r = Record.new
raise "default #{r.shown.inspect}" unless r.shown == "none"
r.data[:value] = "1.2.3.4"
raise "write through #{r.shown.inspect}" unless r.shown == "1.2.3.4"
r.save!
raise "persisted" unless Record.find(r.id).shown == "1.2.3.4"
puts "PASS"
"#,
        )
        .assert_passes();
}

/// The typed_store gem declares its attributes through Rails'
/// `store_accessor`, which defines `<key>_changed?` and `<key>_was`
/// over the store column's pending change. Core's
/// `Shop#sanitize_analytics_account` (a before_validation) asks
/// `analytics_account_changed?`: NoMethodError. A change to ANOTHER
/// key of the same column is no change to this one.
#[test]
fn store_keys_answer_their_own_dirty_state() {
    emit_and_run::empty_app()
        .write(
            "db/schema.rb",
            "ActiveRecord::Schema.define do\n  create_table :shops do |t|\n    t.string :name\n    t.text :settings\n  end\nend\n",
        )
        .write(
            "app/models/application_record.rb",
            "class ApplicationRecord < ActiveRecord::Base\n  primary_abstract_class\nend\n",
        )
        .write(
            "app/models/shop.rb",
            r#"class Shop < ApplicationRecord
  typed_store :settings do |s|
    s.string :analytics_account
    s.string :plan_name
  end

  def report
    [analytics_account_changed?, analytics_account_was, plan_name_changed?]
  end
end
"#,
        )
        .run_ruby(
            r#"
shop = Shop.new(name: "snow")
shop.analytics_account = "UA-1"
shop.save!
fresh = Shop.find(shop.id)
raise "clean #{fresh.report.inspect}" unless fresh.report == [false, "UA-1", false]
fresh.plan_name = "plus"
raise "other key #{fresh.report.inspect}" unless fresh.report == [false, "UA-1", true]
fresh.analytics_account = "UA-2"
raise "changed #{fresh.report.inspect}" unless fresh.report == [true, "UA-1", true]
fresh.save!
raise "saved #{fresh.report.inspect}" unless fresh.report == [false, "UA-2", false]
puts "PASS"
"#,
        )
        .assert_passes();
}

/// Code the model body does not hold reads the store as Rails hands it
/// out: Core's `ShopBillingSetting::Extension` (an included concern)
/// keeps `@status_was = status_was` in a `before_save` and compares
/// `@status_was.values_at(*FIELDS)` with `self[:status].values_at(*FIELDS)`.
/// The column reader, `self[:col]` and `<col>_was` answered the
/// serialized String: NoMethodError `values_at`, and `"…"["plan_name"]`
/// a substring rather than the value.
#[test]
fn a_concern_reads_the_store_and_its_previous_value_as_hashes() {
    emit_and_run::empty_app()
        .write(
            "db/schema.rb",
            "ActiveRecord::Schema.define do\n  create_table :billings do |t|\n    t.text :status\n  end\nend\n",
        )
        .write(
            "app/models/application_record.rb",
            "class ApplicationRecord < ActiveRecord::Base\n  primary_abstract_class\nend\n",
        )
        .write(
            "app/models/billing/extension.rb",
            r#"module Billing::Extension
  extend ActiveSupport::Concern

  FIELDS = ["plan_name", "plan_period"]

  included do
    before_save :compare_plan
  end

  def plan_changed
    @plan_changed
  end

  def compare_plan
    @plan_changed = @status_changed && @status_was.values_at(*FIELDS) != self[:status].values_at(*FIELDS)
  end

  def previous_plan
    @status_was["plan_name"]
  end

  def status_summary
    self[:status].slice(*FIELDS).symbolize_keys.compact
  end
end
"#,
        )
        .write(
            "app/models/billing.rb",
            r#"class Billing < ApplicationRecord
  before_save :store_dirty
  include Billing::Extension

  typed_store :status do |s|
    s.string :plan_name
    s.string :plan_period, default: "monthly"
    s.boolean :customized, default: false
  end

  def store_dirty
    @status_changed = status_changed?
    @status_was = status_was
  end
end
"#,
        )
        .run_ruby(
            r#"
b = Billing.new
b.plan_name = "basic"
b.save!
raise "view #{b[:status].inspect}" unless b[:status]["plan_name"] == "basic" && b[:status][:plan_period] == "monthly"
raise "summary #{b.status_summary.inspect}" unless b.status_summary == { plan_name: "basic", plan_period: "monthly" }
b = Billing.find(b.id)
b.plan_name = "plus"
raise "was #{b.status_was.inspect}" unless b.status_was["plan_name"] == "basic" && b.status_was[:plan_period] == "monthly"
b.save!
raise "previous #{b.previous_plan.inspect}" unless b.previous_plan == "basic"
raise "changed" unless b.plan_changed == true
b.customized = true
b.save!
raise "unchanged plan" unless b.plan_changed == false
puts "PASS"
"#,
        )
        .assert_passes();
}

//! An STI hierarchy whose stored type is the app's own `sti_name`,
//! against native Rails 8.2 on the same local PostgreSQL database.
//! Core's `CustomerOauthApplication.sti_name` is `name.demodulize` and
//! its `sti_class_for` resolves the stored value in its namespace;
//! `StiLookupTable` maps classes to arbitrary values. Rails writes
//! `sti_name` (`ensure_proper_type`), filters on it (`type_condition`
//! maps `([self] + descendants)` through it) and instantiates through
//! `find_sti_class`, which asks `sti_class_for`. The compiler carries
//! `<Class>.sti_name` for such a hierarchy (`sti_scope::runtime_sti_named`)
//! and hydrates through the runtime `find_sti_class`.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;
#[path = "support/pg_probe.rs"]
mod pg_probe;

const STRUCTURE: &str = r#"CREATE TABLE oauth_apps (
    id bigserial NOT NULL,
    type character varying,
    name character varying,
    CONSTRAINT oauth_apps_pkey PRIMARY KEY (id)
);
CREATE TABLE holds (
    id bigserial NOT NULL,
    type character varying,
    name character varying,
    CONSTRAINT holds_pkey PRIMARY KEY (id)
);
"#;

const DATABASE_YML: &str = r#"default: &default
  adapter: postgresql
  encoding: unicode

test:
  <<: *default
  database: rh_e2e_sti_name_override
"#;

const APPLICATION_RECORD: &str = r#"class ApplicationRecord < ActiveRecord::Base
  self.abstract_class = true
end
"#;

/// Core's `CustomerOauthApplication` shape.
const OAUTH_APP: &str = r#"module Auth
  class OauthApp < ApplicationRecord
    self.table_name = :oauth_apps

    class << self
      def sti_name
        name.demodulize
      end

      def sti_class_for(type_name)
        Object.const_get("Auth::#{type_name}")
      end
    end

    def self.named(n)
      where(name: n).first
    end
  end
end
"#;

const SHOPIFY_APP: &str = r#"module Auth
  class ShopifyApp < OauthApp
  end
end
"#;

const MOBILE_APP: &str = r#"module Auth
  class MobileApp < OauthApp
  end
end
"#;

const WEB_APP: &str = r#"module Auth
  class WebApp < MobileApp
  end
end
"#;

/// Core's fulfillment hold shape: each subclass names its own value.
const HOLD: &str = r#"class Hold < ApplicationRecord
  class << self
    def sti_class_for(type_name)
      case type_name
      when "user_hold" then UserHold
      when "system_hold" then SystemHold
      else super
      end
    end
  end
end
"#;

const USER_HOLD: &str = r#"class UserHold < Hold
  class << self
    def sti_name
      "user_hold"
    end
  end
end
"#;

const SYSTEM_HOLD: &str = r#"class SystemHold < Hold
  class << self
    def sti_name
      "system_hold"
    end
  end
end
"#;

const PROBE: &str = r##"class StiNameOverrideProbe
  def self.lines
    out = []
    Auth::ShopifyApp.create!(name: "s")
    Auth::MobileApp.create!(name: "m")
    Auth::WebApp.create!(name: "w")
    Auth::OauthApp.create!(name: "o")
    out << "types: #{Auth::OauthApp.order(:id).pluck(:type).inspect}"
    out << "base: #{Auth::OauthApp.order(:id).map { |a| a.class.name }.join(",")}"
    out << "sub: #{Auth::ShopifyApp.count} #{Auth::MobileApp.order(:id).map(&:name).join(",")} #{Auth::WebApp.all.map(&:name).join(",")}"
    s = Auth::ShopifyApp.first
    out << "find: #{Auth::ShopifyApp.find(s.id).class.name} #{Auth::OauthApp.find(s.id).class.name} #{Auth::MobileApp.where(name: "w").first.class.name}"
    out << "named: #{Auth::ShopifyApp.named("s")&.name.inspect} #{Auth::ShopifyApp.named("m").inspect} #{Auth::MobileApp.named("w").class.name}"
    out << "is_a: #{s.is_a?(Auth::ShopifyApp)} #{Auth::OauthApp.find_by(name: "w").is_a?(Auth::MobileApp)} #{s.is_a?(Auth::MobileApp)}"
    out << "new: #{Auth::WebApp.new.type.inspect} #{Auth::MobileApp.new(name: "x").type.inspect}"
    begin
      Auth::ShopifyApp.find(Auth::MobileApp.first.id)
      out << "cross find: found"
    rescue ActiveRecord::RecordNotFound
      out << "cross find: not found"
    end
    UserHold.create!(name: "u")
    SystemHold.create!(name: "y")
    out << "holds: #{Hold.order(:id).pluck(:type).inspect} #{Hold.order(:id).map { |h| h.class.name }.join(",")} #{UserHold.count} #{SystemHold.first.name}"
    out
  end
end
"##;

/// What native Rails 8.2 answers (checked on the task PostgreSQL), so two
/// identical wrong answers still fail.
const EXPECTED: [&str; 9] = [
    "types: [\"ShopifyApp\", \"MobileApp\", \"WebApp\", nil]",
    "base: Auth::ShopifyApp,Auth::MobileApp,Auth::WebApp,Auth::OauthApp",
    "sub: 1 m,w w",
    "find: Auth::ShopifyApp Auth::ShopifyApp Auth::WebApp",
    "named: \"s\" nil Auth::WebApp",
    "is_a: true true false",
    "new: \"WebApp\" \"MobileApp\"",
    "cross find: not found",
    "holds: [\"user_hold\", \"system_hold\"] UserHold,SystemHold 1 y",
];

const LOADS: [&str; 9] = [
    "app/models/application_record",
    "app/models/auth/oauth_app",
    "app/models/auth/shopify_app",
    "app/models/auth/mobile_app",
    "app/models/auth/web_app",
    "app/models/hold",
    "app/models/user_hold",
    "app/models/system_hold",
    "app/models/sti_name_override_probe",
];

const FILES: [(&str, &str); 13] = [
    ("roundhouse.yml", "database_adapter: postgresql\n"),
    ("config/database.yml", DATABASE_YML),
    ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
    ("db/structure.sql", STRUCTURE),
    ("app/models/application_record.rb", APPLICATION_RECORD),
    ("app/models/auth/oauth_app.rb", OAUTH_APP),
    ("app/models/auth/shopify_app.rb", SHOPIFY_APP),
    ("app/models/auth/mobile_app.rb", MOBILE_APP),
    ("app/models/auth/web_app.rb", WEB_APP),
    ("app/models/hold.rb", HOLD),
    ("app/models/user_hold.rb", USER_HOLD),
    ("app/models/system_hold.rb", SYSTEM_HOLD),
    ("app/models/sti_name_override_probe.rb", PROBE),
];

fn overlay() -> emit_and_run::Overlay {
    FILES.iter().fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text))
}

fn real_errors(errors: &[String]) -> Vec<&String> {
    errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect()
}

/// The type condition, the constructor stamp and the hydration carry
/// the runtime `sti_name` / `find_sti_class`, never the class name.
#[test]
fn sti_name_override_compiles_to_runtime_names() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    let read = |p: &str| std::fs::read_to_string(emitted.join(p)).unwrap();
    let mobile = read("app/models/auth/mobile_app.rb");
    assert!(
        mobile.contains("relation.where!(type: [Auth::MobileApp.sti_name, Auth::WebApp.sti_name])"),
        "{mobile}"
    );
    assert!(mobile.contains("self.type = Auth::MobileApp.sti_name if"), "{mobile}");
    assert!(!mobile.contains("\"Auth::MobileApp\""), "{mobile}");
    let base = read("app/models/auth/oauth_app.rb");
    assert!(base.contains("find_sti_class(row.type"), "{base}");
    let hold = read("app/models/user_hold.rb");
    assert!(hold.contains("relation.where!(type: UserHold.sti_name)"), "{hold}");
}

#[test]
#[ignore = "requires pg gem, Core's pinned Rails and ROUNDHOUSE_PG_TEST_URL for a disposable local PostgreSQL server"]
fn sti_name_override_matches_native_rails_on_postgresql() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    let (native, emitted_lines) = pg_probe::native_and_emitted(
        "sti-name-override",
        &FILES,
        &emitted,
        "rh_e2e_sti_name_override",
        "StiNameOverrideProbe",
        &LOADS,
    );
    assert_eq!(native, EXPECTED, "native");
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    assert_eq!(emitted_lines, native, "emitted");
}

//! A subclass controller's own call of a class macro its superclass's
//! Concern defines, against native Rails 8.2 (Core's pinned copy).
//!
//! ```ruby
//! class Admin::AccountsController < AdminAreaController
//!   allow_same_origin_frames
//! end
//! ```
//!
//! `allow_same_origin_frames` is a `ClickjackingProtection::ClassMethods`
//! macro that appends to the class-level `clickjacking_rules`. The
//! controller lowering dropped the call (it is neither a filter nor a
//! recognized declaration), so every subclass kept its parent's settings.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::process::Command;

const STRUCTURE: &str = "CREATE TABLE shops (\n    id bigserial NOT NULL,\n    name character varying,\n    CONSTRAINT shops_pkey PRIMARY KEY (id)\n);\n";

const APPLICATION_CONTROLLER: &str = "class ApplicationController < ActionController::Base\n  include FrameProtection\nend\n";

const ADMIN_AREA: &str = "class AdminAreaController < ApplicationController\nend\n";

const ACCOUNTS: &str = r#"module Admin
  class AccountsController < AdminAreaController
    allow_same_origin_frames only: [:index]
    throttle_cost 5, only: :index

    def index = head(:ok)
  end
end
"#;

const ORDERS: &str = r#"module Admin
  class OrdersController < AdminAreaController
    allow_cross_domain_frames

    def index = head(:ok)
  end
end
"#;

/// `ClickjackingProtection`'s shape: a block that seeds a class attribute,
/// and `ClassMethods` macros that append to it.
const FRAME_PROTECTION: &str = r#"module FrameProtection
  extend ActiveSupport::Concern

  included do
    class_attribute :frame_rules, instance_writer: false, instance_predicate: false
    self.frame_rules = []

    deny_frames
  end

  module ClassMethods
    def deny_frames(**options)
      self.frame_rules += [[:deny, options]]
    end

    def allow_same_origin_frames(**options)
      self.frame_rules += [[:sameorigin, options]]
    end

    def allow_cross_domain_frames(**options)
      self.frame_rules += [[:not_protected, options]]
    end

    def throttle_cost(cost, **options)
      @throttle_costs ||= []
      @throttle_costs << [cost, options]
    end

    def throttle_costs = @throttle_costs
  end
end
"#;

const FILES: [(&str, &str); 8] = [
    ("roundhouse.yml", "database_adapter: postgresql\n"),
    ("config/routes.rb", "Rails.application.routes.draw do\n  get \"accounts\", to: \"admin/accounts#index\"\n  get \"orders\", to: \"admin/orders#index\"\nend\n"),
    ("db/structure.sql", STRUCTURE),
    ("app/controllers/application_controller.rb", APPLICATION_CONTROLLER),
    ("app/controllers/admin_area_controller.rb", ADMIN_AREA),
    ("app/controllers/admin/accounts_controller.rb", ACCOUNTS),
    ("app/controllers/admin/orders_controller.rb", ORDERS),
    ("app/controllers/concerns/frame_protection.rb", FRAME_PROTECTION),
];

fn overlay() -> emit_and_run::Overlay {
    FILES.iter().fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text))
}

fn real_errors(errors: &[String]) -> Vec<&String> {
    errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect()
}

const PROBE: &str = r#"p ApplicationController.frame_rules
p AdminAreaController.frame_rules
p Admin::AccountsController.frame_rules
p Admin::OrdersController.frame_rules
p Admin::AccountsController.throttle_costs
p Admin::OrdersController.throttle_costs
"#;

const EXPECTED: [&str; 6] = [
    "[[:deny, {}]]",
    "[[:deny, {}]]",
    "[[:deny, {}], [:sameorigin, {only: [:index]}]]",
    "[[:deny, {}], [:not_protected, {}]]",
    "[[5, {only: :index}]]",
    "nil",
];

const NATIVE_BOOT: &str = r#"require ENV.fetch("RH_ORACLE")
RailsOracle.activate("activesupport", "actionpack")
require "action_controller"
%w[app/controllers/concerns/frame_protection app/controllers/application_controller
   app/controllers/admin_area_controller app/controllers/admin/accounts_controller
   app/controllers/admin/orders_controller].each { |f| require File.expand_path(f) }
"#;

fn ruby(root: &std::path::Path, script: &str, env: &[(&str, String)]) -> Vec<String> {
    let gem_home = std::env::var("CORE_GEM_HOME")
        .unwrap_or_else(|_| "/nix/store/d958gj4a8pmld3fxa7fhyqasz61cafch-ruby-gems-dev/lib/ruby/gems/4.0.0".into());
    let jemalloc = std::env::var("RH_JEMALLOC").unwrap_or_else(|_| {
        "/nix/store/c04j2i0fkin1bw5fab35gx896558knsd-jemalloc-5.3.0-unstable-2025-09-12/lib/libjemalloc.so.2".into()
    });
    // A file, not `-e`: the oracle re-executes `$PROGRAM_NAME`.
    let file = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("controller-sub-{}-{}.rb", std::process::id(), root.file_name().unwrap().to_string_lossy()));
    std::fs::write(&file, script).unwrap();
    let output = Command::new("ruby")
        .arg(&file)
        .current_dir(root)
        .env("GEM_HOME", &gem_home)
        .env("GEM_PATH", &gem_home)
        .env("LD_PRELOAD", &jemalloc)
        .envs(env.iter().map(|(k, v)| (*k, v.as_str())))
        .output()
        .expect("ruby");
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8_lossy(&output.stdout).lines().map(str::to_string).collect()
}

#[test]
#[ignore = "requires pg gem and Core's pinned Rails (actionpack) from the Core gem home"]
fn a_subclass_controllers_concern_macro_calls_run_like_native_rails() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));

    let native_root = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("controller-sub-native-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&native_root);
    for (path, text) in FILES {
        let full = native_root.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, text).unwrap();
    }
    let oracle = format!("{}/tests/support/rails_oracle", env!("CARGO_MANIFEST_DIR"));
    let native = ruby(&native_root, &format!("{NATIVE_BOOT}\n{PROBE}"), &[("RH_ORACLE", oracle)]);
    assert_eq!(native, EXPECTED, "native");

    let boot = format!("require File.expand_path(\"main\", Dir.pwd)\n{PROBE}");
    let url = "host=/nonexistent port=1 dbname=none user=none".to_string();
    let lines = ruby(&emitted, &boot, &[("RAILS_ENV", "test".into()), ("ROUNDHOUSE_DATABASE_URL", url)]);
    assert_eq!(lines, native, "emitted");
    let _ = std::fs::remove_dir_all(&native_root);
}

/// The emitted subclass writes its own macro calls, after the inherited
/// concern's include has run on the superclass.
#[test]
fn the_subclass_writes_its_own_macro_calls() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    let read = |rel: &str| std::fs::read_to_string(emitted.join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"));
    let accounts = read("app/controllers/admin/accounts_controller.rb");
    assert!(accounts.contains("allow_same_origin_frames(only: [:index])") || accounts.contains("allow_same_origin_frames only: [:index]"), "{accounts}");
    assert!(accounts.contains("throttle_cost"), "{accounts}");
    let orders = read("app/controllers/admin/orders_controller.rb");
    assert!(orders.contains("allow_cross_domain_frames"), "{orders}");
}

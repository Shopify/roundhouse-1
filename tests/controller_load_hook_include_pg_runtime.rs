//! A controller wired through `ActiveSupport.on_load(:admin_area_controller)
//! { include … }`, against native Rails 8.2 (Core's pinned copy).
//!
//! Core's `AdminAreaController` ends its body with
//! `ActiveSupport.run_load_hooks(:admin_area_controller, self)`, and
//! components/access_and_auth/config/initializers/admin_area_controllers.rb
//! includes `IdentityAwareController` there. Its `included do` block
//! includes a module whose block-included `ClassMethods` define
//! `all_authorized_clients`, which the concern's own `ClassMethods#inherited`
//! calls on every subclass: with the include run only as a boot-time hook,
//! the controller lowering never wrote it, the block was dropped, and
//! `class Admin::OrdersController < AdminAreaController` raised
//! NoMethodError.
//!
//! Ingest now lifts the hook's includes to the end of the controller body
//! (`load_hook_includes`) and replays each concern's `ClassMethods` and the
//! class macros its `included` block calls
//! (`controller_concern_blocks`). A block holding a `require` stays at the
//! hook, `require` included; a `prepend` is another hook and stays too.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::process::Command;

const STRUCTURE: &str = "CREATE TABLE shops (\n    id bigserial NOT NULL,\n    name character varying,\n    CONSTRAINT shops_pkey PRIMARY KEY (id)\n);\n";

const APPLICATION_CONTROLLER: &str = "class ApplicationController < ActionController::Base\nend\n";

const ADMIN_AREA: &str = r#"class AdminAreaController < ApplicationController
  before_action :require_credentials

  def require_credentials = true

  ActiveSupport.run_load_hooks(:admin_area_controller, self)
end
"#;

const ORDERS: &str = r#"module Admin
  class OrdersController < AdminAreaController
    def index = head(:ok)
  end
end
"#;

const CLIENT_REGISTRY: &str = r#"module ClientRegistry
  extend ActiveSupport::Concern

  module ClassMethods
    def all_clients
      @all_clients ||= []
    end

    def client(name)
      all_clients << name
    end
  end

  def registry_note = "registry"
end
"#;

/// `IdentityAwareController`'s shape: a block that includes the registry
/// and calls its macro, and a `ClassMethods#inherited` that needs both.
const IDENTITY_AWARE: &str = r#"module IdentityAware
  extend ActiveSupport::Concern

  included do
    include ClientRegistry

    client :unified
    skip_before_action :require_credentials

    def identity_note = "identity"
  end

  module ClassMethods
    def identity_aware?
      true
    end

    def inherited(subclass)
      subclass.all_clients.push(*@all_clients)
      super
    end
  end
end
"#;

const AUDIT_TRAIL: &str = "module AuditTrail\n  def audit_note = \"audit\"\nend\n";

const PREPENDED: &str = "module Prepended\n  def prepended_note = \"prepended\"\nend\n";

/// Core's block: a concern, a `require`, a second include.
const INCLUDING: &str = r#"ActiveSupport.on_load(:admin_area_controller) do
  #: self as singleton(AdminAreaController)

  include IdentityAware
  require "audit_trail"
  include AuditTrail
end
"#;

const PREPENDING: &str = "ActiveSupport.on_load(:admin_area_controller) do\n  prepend Prepended\nend\n";

/// Only includes, so the whole block is lifted.
const INCLUDING_ONLY: &str = "ActiveSupport.on_load(:admin_area_controller) do\n  include IdentityAware\nend\n";

const FILES: [(&str, &str); 11] = [
    ("roundhouse.yml", "database_adapter: postgresql\n"),
    ("config/routes.rb", "Rails.application.routes.draw do\n  get \"orders\", to: \"admin/orders#index\"\nend\n"),
    ("db/structure.sql", STRUCTURE),
    ("app/controllers/application_controller.rb", APPLICATION_CONTROLLER),
    ("app/controllers/admin_area_controller.rb", ADMIN_AREA),
    ("app/controllers/admin/orders_controller.rb", ORDERS),
    ("app/controllers/concerns/client_registry.rb", CLIENT_REGISTRY),
    ("app/controllers/concerns/identity_aware.rb", IDENTITY_AWARE),
    ("app/controllers/concerns/prepended.rb", PREPENDED),
    ("lib/audit_trail.rb", AUDIT_TRAIL),
    ("config/initializers/admin_area_controllers.rb", INCLUDING),
];

fn overlay() -> emit_and_run::Overlay {
    FILES
        .iter()
        .fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text))
        .write("config/initializers/admin_area_prepend.rb", PREPENDING)
}

fn real_errors(errors: &[String]) -> Vec<&String> {
    errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect()
}

const PROBE: &str = r#"p Admin::OrdersController.all_clients
p AdminAreaController.all_clients
p Admin::OrdersController.identity_aware?
p Admin::OrdersController.new.identity_note
p AdminAreaController.include?(ClientRegistry)
p AdminAreaController.ancestors.first(3)
p Admin::OrdersController.new.audit_note
"#;

/// What native Rails 8.2 answers (checked), so two identical wrong
/// answers still fail.
const EXPECTED: [&str; 7] = [
    "[:unified]",
    "[:unified]",
    "true",
    "\"identity\"",
    "true",
    "[Prepended, AdminAreaController, AuditTrail]",
    "\"audit\"",
];

const NATIVE_BOOT: &str = r#"require ENV.fetch("RH_ORACLE")
RailsOracle.activate("activesupport", "actionpack")
require "action_controller"
$LOAD_PATH.unshift(File.join(Dir.pwd, "lib"))
Dir["config/initializers/*.rb"].sort.each { |f| load f }
%w[app/controllers/application_controller app/controllers/concerns/client_registry
   app/controllers/concerns/identity_aware app/controllers/concerns/prepended
   app/controllers/admin_area_controller app/controllers/admin/orders_controller].each { |f| require File.expand_path(f) }
"#;

fn ruby(root: &std::path::Path, script: &str, env: &[(&str, String)]) -> Vec<String> {
    let gem_home = std::env::var("CORE_GEM_HOME")
        .unwrap_or_else(|_| "/nix/store/d958gj4a8pmld3fxa7fhyqasz61cafch-ruby-gems-dev/lib/ruby/gems/4.0.0".into());
    let jemalloc = std::env::var("RH_JEMALLOC").unwrap_or_else(|_| {
        "/nix/store/c04j2i0fkin1bw5fab35gx896558knsd-jemalloc-5.3.0-unstable-2025-09-12/lib/libjemalloc.so.2".into()
    });
    // A file, not `-e`: the oracle re-executes `$PROGRAM_NAME`.
    let file = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("controller-hook-{}-{}.rb", std::process::id(), root.file_name().unwrap().to_string_lossy()));
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
fn the_hook_included_concern_runs_on_the_controller_like_native_rails() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));

    // Native: the app's initializers, then its files.
    let native_root = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("controller-hook-native-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&native_root);
    for (path, text) in FILES.iter().copied().chain([("config/initializers/admin_area_prepend.rb", PREPENDING)]) {
        let full = native_root.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, text).unwrap();
    }
    let oracle = format!("{}/tests/support/rails_oracle", env!("CARGO_MANIFEST_DIR"));
    let native = ruby(&native_root, &format!("{NATIVE_BOOT}\n{PROBE}"), &[("RH_ORACLE", oracle)]);
    assert_eq!(native, EXPECTED, "native");

    // Emitted: boot reads the database configuration, no connection is made.
    let boot = format!("require File.expand_path(\"main\", Dir.pwd)\n{PROBE}");
    let url = "host=/nonexistent port=1 dbname=none user=none".to_string();
    let lines = ruby(&emitted, &boot, &[("RAILS_ENV", "test".into()), ("ROUNDHOUSE_DATABASE_URL", url)]);
    assert_eq!(lines, native, "emitted");
    let _ = std::fs::remove_dir_all(&native_root);
}

/// The controller writes the include and the class side of the concern
/// where Ruby runs them; the `require` block is left at its hook, with
/// the `prepend` hook beside it.
#[test]
fn the_controller_writes_the_include_and_the_class_side() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    let read = |rel: &str| std::fs::read_to_string(emitted.join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"));
    let controller = read("app/controllers/admin_area_controller.rb");
    let ordered = [
        "include IdentityAware",
        "extend IdentityAware::ClassMethods",
        "include ClientRegistry",
        "extend ClientRegistry::ClassMethods",
        "client :unified",
    ];
    let mut at = 0;
    for line in ordered {
        let found = controller[at..].find(line).unwrap_or_else(|| panic!("`{line}` in order\n{controller}"));
        at += found + line.len();
    }
    // Held in the concern's block, not written twice.
    assert!(!controller.contains("skip_before_action"), "{controller}");
    let hook = read("app/models/roundhouse_initializer_config_initializers_admin_area_controllers.rb");
    assert!(hook.contains("ActiveSupport.on_load(:admin_area_controller)") && hook.contains("include AuditTrail"), "{hook}");
    let prepend = read("app/models/roundhouse_initializer_config_initializers_admin_area_prepend.rb");
    assert!(prepend.contains("prepend Prepended"), "{prepend}");
}

/// A block of includes only is consumed whole: no initializer holder
/// remains for it.
#[test]
fn a_block_of_only_includes_leaves_no_hook_behind() {
    let (emitted, errors) = overlay().write("config/initializers/admin_area_controllers.rb", INCLUDING_ONLY).emit(roundhouse::project::BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    let controller = std::fs::read_to_string(emitted.join("app/controllers/admin_area_controller.rb")).unwrap();
    assert!(controller.contains("extend IdentityAware::ClassMethods"), "{controller}");
    assert!(
        !emitted.join("app/models/roundhouse_initializer_config_initializers_admin_area_controllers.rb").exists(),
        "the lifted block was also kept as a hook"
    );
}

//! A Concern with a `ClassMethods` module and no `included do` block, which
//! a controller includes (rooted, in parentheses) and then calls a macro of,
//! against native Rails 8.2 (Core's pinned copy).
//!
//! ```ruby
//! module Cors
//!   extend ActiveSupport::Concern
//!   module ClassMethods
//!     def allow_insecure_cross_origin_access(...) = ...
//!   end
//! end
//!
//! class GatewayController < ApiAreaController
//!   include(::Cors)
//!   allow_insecure_cross_origin_access(...)
//! end
//! ```
//!
//! `Concern#append_features` extends the includer with `ClassMethods` with
//! or without a block. The controller lowering wrote neither the include
//! nor the extend when the Concern had nothing else to run, and the
//! subclass-macro replay wrote the call anyway: NoMethodError at class
//! creation.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::process::Command;

const STRUCTURE: &str = "CREATE TABLE shops (\n    id bigserial NOT NULL,\n    name character varying,\n    CONSTRAINT shops_pkey PRIMARY KEY (id)\n);\n";

const APPLICATION_CONTROLLER: &str = "class ApplicationController < ActionController::Base\nend\n";

const API_AREA: &str = "class ApiAreaController < ApplicationController\nend\n";

const GRAPHQL: &str = r#"module Checkouts
  class GatewayController < ApiAreaController
    include(::Cors)
    include Wrapper

    allow_insecure_cross_origin_access(:post, "*", max_age: 10)
    tag_with :wrapped

    def index = head(:ok)
  end
end
"#;

// The subclass includes nothing: its parent's extends reach it.
const ONE: &str = r#"module Checkouts
  class OneController < GatewayController
    allow_insecure_cross_origin_access(:get, "https://one.example")

    def index = head(:ok)
  end
end
"#;

/// A library class and a model take the same rule.
const REPORTER: &str = "class Reporter\n  include Cors\n  allow_insecure_cross_origin_access(:put, \"*\")\nend\n";

const SHOP: &str = "class Shop < ApplicationRecord\n  include Cors\n  allow_insecure_cross_origin_access(:delete, \"*\")\nend\n";

const APPLICATION_RECORD: &str = "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n";

/// No `included do`: only the nested `ClassMethods`.
const CORS: &str = r#"module Cors
  extend ActiveSupport::Concern

  ALLOWED = ["x"].freeze

  module ClassMethods
    def allow_insecure_cross_origin_access(methods, origins, options = {})
      (@cors ||= []) << [methods, origins, options]
    end

    def cors = @cors
  end
end
"#;

/// A Concern that includes another one: Rails defers `Inner` to the
/// includer, which `Inner`'s `ClassMethods` extends.
const WRAPPER: &str = r#"module Wrapper
  extend ActiveSupport::Concern
  include Inner
end
"#;

const INNER: &str = r#"module Inner
  extend ActiveSupport::Concern

  module ClassMethods
    def tag_with(tag)
      (@tags ||= []) << tag
    end

    def tags = @tags
  end
end
"#;

const FILES: [(&str, &str); 12] = [
    ("roundhouse.yml", "database_adapter: postgresql\n"),
    ("config/routes.rb", "Rails.application.routes.draw do\n  get \"gateway\", to: \"checkouts/gateway#index\"\n  get \"one\", to: \"checkouts/one#index\"\nend\n"),
    ("db/structure.sql", STRUCTURE),
    ("app/controllers/application_controller.rb", APPLICATION_CONTROLLER),
    ("app/controllers/api_area_controller.rb", API_AREA),
    ("app/controllers/checkouts/gateway_controller.rb", GRAPHQL),
    ("app/controllers/checkouts/one_controller.rb", ONE),
    ("app/controllers/concerns/cors.rb", CORS),
    ("app/controllers/concerns/wrapper.rb", WRAPPER),
    ("app/controllers/concerns/inner.rb", INNER),
    ("app/models/application_record.rb", APPLICATION_RECORD),
    ("app/models/shop.rb", SHOP),
];

fn overlay() -> emit_and_run::Overlay {
    FILES
        .iter()
        .fold(emit_and_run::empty_app(), |app, (path, text)| app.write(path, text))
        .write("app/models/reporter.rb", REPORTER)
}

fn real_errors(errors: &[String]) -> Vec<&String> {
    errors.iter().filter(|e| !e.contains("database_adapter not supported")).collect()
}

const PROBE: &str = r#"p Checkouts::GatewayController.cors
p Checkouts::OneController.cors
p Checkouts::GatewayController.tags
p Checkouts::OneController.tags
p Reporter.cors
p Shop.cors
"#;

const EXPECTED: [&str; 6] = [
    "[[:post, \"*\", {max_age: 10}]]",
    "[[:get, \"https://one.example\", {}]]",
    "[:wrapped]",
    "nil",
    "[[:put, \"*\", {}]]",
    "[[:delete, \"*\", {}]]",
];

const NATIVE_BOOT: &str = r#"require ENV.fetch("RH_ORACLE")
RailsOracle.activate("activesupport", "actionpack", "activerecord")
require "action_controller"
require "active_record"
%w[app/controllers/concerns/cors app/controllers/concerns/inner app/controllers/concerns/wrapper
   app/controllers/application_controller app/controllers/api_area_controller
   app/controllers/checkouts/gateway_controller app/controllers/checkouts/one_controller
   app/models/reporter app/models/application_record app/models/shop].each { |f| require File.expand_path(f) }
"#;

fn ruby(root: &std::path::Path, script: &str, env: &[(&str, String)]) -> Vec<String> {
    let gem_home = std::env::var("CORE_GEM_HOME")
        .unwrap_or_else(|_| "/nix/store/d958gj4a8pmld3fxa7fhyqasz61cafch-ruby-gems-dev/lib/ruby/gems/4.0.0".into());
    let jemalloc = std::env::var("RH_JEMALLOC").unwrap_or_else(|_| {
        "/nix/store/c04j2i0fkin1bw5fab35gx896558knsd-jemalloc-5.3.0-unstable-2025-09-12/lib/libjemalloc.so.2".into()
    });
    // A file, not `-e`: the oracle re-executes `$PROGRAM_NAME`.
    let file = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("concern-cm-only-{}-{}.rb", std::process::id(), root.file_name().unwrap().to_string_lossy()));
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
fn a_concern_with_only_class_methods_extends_its_includers_like_native_rails() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));

    let native_root = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("concern-cm-only-native-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&native_root);
    for (path, text) in FILES.iter().chain([("app/models/reporter.rb", REPORTER)].iter()) {
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

/// The emitted controller writes the include and the extend of every
/// concern it includes, directly or through another Concern, and requires
/// the carrier before the macro call that needs it.
#[test]
fn the_controller_extends_every_included_concerns_class_methods() {
    let (emitted, errors) = overlay().emit(roundhouse::project::BuildTarget::Ruby);
    assert!(real_errors(&errors).is_empty(), "{}", errors.join("\n"));
    let read = |rel: &str| std::fs::read_to_string(emitted.join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"));
    let graphql = read("app/controllers/checkouts/gateway_controller.rb");
    for needle in ["include Cors", "extend Cors::ClassMethods", "extend Inner::ClassMethods", "cors/class_methods\"", "inner/class_methods\""] {
        assert!(graphql.contains(needle), "missing `{needle}`:\n{graphql}");
    }
    let extend_at = graphql.find("extend Cors::ClassMethods").unwrap();
    let call_at = graphql.find("allow_insecure_cross_origin_access(").unwrap();
    assert!(extend_at < call_at, "extend after the macro call:\n{graphql}");
    // A subclass whose parent includes the concern is extended by it.
    let one = read("app/controllers/checkouts/one_controller.rb");
    assert!(one.contains("allow_insecure_cross_origin_access("), "{one}");
    assert!(!one.contains("extend Cors::ClassMethods"), "{one}");
}

//! `include ::A::B` names the top-level `A::B`.
//!
//! Ingest keeps the root as an empty first path segment, and the
//! model's include list joined it whole as `::A::B`, which no registry
//! entry carries. Core's `ApiClient` (`include ::ApiVersioning::ApiClient`)
//! was refused as an unresolved include, and the gem's
//! `mixes_in_class_methods(ClassMethods)` never gave it `current`.

use std::process::Command;

const API_CLIENT: &str = "module Fakeav\n  module ApiClient\n    extend T::Sig\n    extend T::Helpers\n\n    module ClassMethods\n      extend T::Sig\n\n      sig { returns(Integer) }\n      def current = 1\n    end\n\n    sig { returns(T::Boolean) }\n    def internal_access? = false\n\n    mixes_in_class_methods(ClassMethods)\n  end\nend\n";

fn check(name: &str, include: &str) -> String {
    let root = std::env::temp_dir().join(format!("rh_rooted_include_typing_{}_{name}", std::process::id()));
    let account = format!("class Account < ApplicationRecord\n  include {include}\n\n  #: -> untyped\n  def self.use = [Account.current.zzz, new.internal_access?.yyy]\nend\n");
    let files = [
        ("roundhouse.yml".to_string(), "test_paths:\n  - test\ngem_sources:\n  fakeav: vendor/gems/fakeav-1.0\n".to_string()),
        ("db/structure.sql".to_string(), "CREATE TABLE core.accounts (\n    id bigint NOT NULL\n);\n".to_string()),
        ("app/models/application_record.rb".to_string(), "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n".to_string()),
        ("vendor/gems/fakeav-1.0/lib/fakeav.rb".to_string(), "require \"fakeav/api_client\"\n".to_string()),
        ("vendor/gems/fakeav-1.0/lib/fakeav/api_client.rb".to_string(), API_CLIENT.to_string()),
        ("app/models/account.rb".to_string(), account),
    ];
    for (path, text) in files {
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    let output = Command::new(env!("CARGO_BIN_EXE_roundhouse")).arg("check").arg(&root).output().unwrap();
    std::fs::remove_dir_all(&root).unwrap();
    format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
}

#[test]
fn a_rooted_include_reaches_both_sides() {
    for (name, include) in [("rooted", "::Fakeav::ApiClient"), ("relative", "Fakeav::ApiClient")] {
        let text = check(name, include);
        assert!(!text.contains("includes unresolved"), "{name}: {text}");
        assert!(text.contains("no known method `zzz` on Integer"), "{name}: {text}");
        assert!(text.contains("no known method `yyy` on bool"), "{name}: {text}");
    }
}

//! Constants and declared types Core's unit sample refused.
//!
//! - `Types::DryTypes::Array`, where `DryTypes = Dry.Types(...)`: the
//!   constant lives in the gem's computed value, so the read is
//!   qualified and emitted for the CRuby tree to evaluate.
//! - `is_a?(ActionController::Metal)`: Base < Metal, as in Rails.
//! - `extend ::EnumAttr` in a model: a rooted top-level mixin was
//!   skipped, so its macro (`enum_attr`) never ran.
//! - `#: ::ActionDispatch::TestRequest` and
//!   `#: ActionDispatch::Http::Headers?` declared types.
//! - `#: ActivityTypes::Type`, a `T.type_alias { T.any(Basic, Hero) }`
//!   whose members are read in the alias's own scope.

use std::path::Path;
use std::process::Command;

const FILES: &[(&str, &str)] = &[
    ("roundhouse.yml", "test_paths:\n  - test\nextensions:\n  - ext\n"),
    ("ext/gems.yml", "lock_gated_requires:\n  - {require: dry-types, gem: dry-types}\n"),
    ("Gemfile.lock", "GEM\n  remote: https://rubygems.org/\n  specs:\n    dry-types (1.8.2)\n\nDEPENDENCIES\n  dry-types\n"),
    (
        "sorbet/rbi/gems/dry-types@1.8.2.rbi",
        "# typed: true\nmodule Dry\n  class << self\n    def Types(*namespaces, default: T.unsafe(nil), **aliases); end\n  end\nend\nmodule Dry::Types; end\n",
    ),
    ("db/schema.rb", "ActiveRecord::Schema[8.0].define(version: 1) do\n  create_table \"cards\", force: :cascade do |t|\n    t.string \"code\"\n  end\nend\n"),
    ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n"),
    ("app/models/concerns/enum_attr.rb", "module EnumAttr\n  def enum_attr(column)\n    define_method(\"#{column}_label\") { \"label\" }\n  end\nend\n"),
    ("app/models/card.rb", "class Card < ApplicationRecord\n  extend ::EnumAttr\n\n  enum_attr :code\nend\n"),
    (
        "app/models/comp/schema.rb",
        "module Comp\n  class Schema\n    module Types\n      DryTypes = Dry.Types(default: :nominal)\n    end\n\n    def self.type_for(t)\n      if t.is_a?(Array)\n        Types::DryTypes::Array.of(type_for(t[0]))\n      else\n        Types::DryTypes::Coercible::Integer\n      end\n    end\n  end\nend\n",
    ),
    (
        "app/models/pubex/metadata.rb",
        "module Pubex\n  class Metadata\n    def initialize(source_object)\n      @is_controller = source_object.is_a?(ActionController::Metal)\n    end\n\n    def controller? = @is_controller\n  end\nend\n",
    ),
    (
        "app/models/req_probe.rb",
        "class ReqProbe\n  def build\n    @a = ActionDispatch::TestRequest.create({}) #: ::ActionDispatch::TestRequest\n    @d = nil #: ActionDispatch::Http::Headers?\n    [@a, @d]\n  end\nend\n",
    ),
    ("app/models/home/activity_types/basic.rb", "module Home\n  module ActivityTypes\n    class Basic\n      def poll? = false\n    end\n  end\nend\n"),
    ("app/models/home/activity_types/hero.rb", "module Home\n  module ActivityTypes\n    class Hero\n      def poll? = true\n    end\n  end\nend\n"),
    ("app/models/home/activity_types/type.rb", "module Home\n  module ActivityTypes\n    Type = T.type_alias do\n      T.any(\n        Basic,\n        Hero,\n      )\n    end\n  end\nend\n"),
    (
        "app/models/home/module_types/user.rb",
        "module Home\n  module ModuleTypes\n    class User\n      def build\n        a = ActivityTypes::Hero.new #: ActivityTypes::Type\n        a.poll?\n      end\n    end\n  end\nend\n",
    ),
];

fn emit() -> (std::path::PathBuf, String) {
    let root = std::env::temp_dir().join(format!("rh_constants_and_declared_types_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for (path, text) in FILES {
        let file = root.join("app").join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    let output = Command::new(env!("CARGO_BIN_EXE_roundhouse"))
        .args(["--target", "ruby"])
        .arg(root.join("app"))
        .arg("-o")
        .arg(root.join("out"))
        .arg("--allow-unsupported")
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    (root, String::from_utf8_lossy(&output.stderr).into_owned())
}

fn read(root: &Path, path: &str) -> String {
    std::fs::read_to_string(root.join("out").join(path)).unwrap_or_else(|e| panic!("{path}: {e}"))
}

#[test]
fn refused_constants_and_declared_types_emit_their_reads() {
    let (root, stderr) = emit();
    let schema = read(&root, "app/models/comp/schema.rb");
    let card = read(&root, "app/models/card.rb");
    let metadata = read(&root, "app/models/pubex/metadata.rb");
    let request = read(&root, "app/models/req_probe.rb");
    let user = read(&root, "app/models/home/module_types/user.rb");
    let base = read(&root, "runtime/action_controller/base.rb");
    std::fs::remove_dir_all(&root).unwrap();

    for text in [&schema, &card, &metadata, &request, &user] {
        assert!(!text.contains("roundhouse: "), "{text}\n{stderr}");
    }
    assert!(schema.contains("Comp::Schema::Types::DryTypes::Array.of(type_for(t[0]))"), "{schema}");
    assert!(schema.contains("Comp::Schema::Types::DryTypes::Coercible::Integer"), "{schema}");
    assert!(stderr.contains("is looked up in the value of Comp::Schema::Types::DryTypes"), "{stderr}");
    assert!(card.contains("def code_label"), "{card}");
    assert!(metadata.contains("source_object.is_a?(ActionController::Metal)"), "{metadata}");
    assert!(base.contains("class Base < ActionController::Metal"), "{base}");
    assert!(request.contains("@a = ActionDispatch::TestRequest.create({})"), "{request}");
    assert!(user.contains("a = Home::ActivityTypes::Hero.new"), "{user}");
    assert!(!stderr.contains("Basic, Hero has no indexed declaration"), "{stderr}");
}

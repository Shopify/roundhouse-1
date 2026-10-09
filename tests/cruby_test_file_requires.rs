//! A test file's top-level loads (`require "support/helpers/…"`,
//! `require_relative`) run in the emitted test before its class, as
//! natively. Core's `delivery_macro_generator_test.rb` requires
//! `support/helpers/delivery/checkout_one/mock_factory`, a support file
//! whose program (a module, then `require`s of its own) nothing else
//! loads: its test class constant `DeliveryMockFactory =
//! ::Delivery::CheckoutOne::MockFactory` was a NameError.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;
use serde_json::Value;
use std::collections::BTreeMap;

const MOCK_FACTORY: &str = r##"# frozen_string_literal: true

module Delivery
  module CheckoutOne
    module MockFactory
      def self.build_time
        :built
      end
    end
  end
end

require "support/helpers/checkouts/one/other_factory"
"##;

const OTHER_FACTORY: &str = "module Checkouts\n  module One\n    module OtherFactory\n      def self.other\n        :other\n      end\n    end\n  end\nend\n";

const RELATIVE_HELPER: &str = "module RelativeHelper\n  def self.kind\n    :relative\n  end\nend\n";

const MACRO_TEST_BODY: &str = r##"
module Checkouts
  module One
    class DeliveryMacroTest < ActiveSupport::TestCase
      DeliveryMockFactory = ::Delivery::CheckoutOne::MockFactory

      test "reads the support module" do
        assert_equal :built, DeliveryMockFactory.build_time
        assert_equal :other, Checkouts::One::OtherFactory.other
      end

      test "reads the relative helper and the gem" do
        assert_equal :relative, RelativeHelper.kind
        assert_equal 32, SecureRandom.hex(16).length
      end
    end
  end
end
"##;

fn minitest_version() -> String {
    include_str!("../runtime/spinel/scaffold/Gemfile.lock")
        .lines()
        .find_map(|line| line.strip_prefix("    minitest (")?.strip_suffix(')'))
        .expect("scaffold Minitest lock")
        .to_string()
}

fn rows(text: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for line in text.lines().filter(|l| !l.is_empty()) {
        let row: Value = serde_json::from_str(line).expect("JSON row");
        let id = row["id"].as_str().unwrap().to_string();
        assert!(map.insert(id.clone(), row["status"].as_str().unwrap().to_string()).is_none(), "{id} reported twice");
    }
    map
}

/// `id => status` for every test Minitest runs natively, the test file
/// loaded from the sources' directory with its test load path.
fn native_ids(root: &std::path::Path, test: &str, class: &str) -> BTreeMap<String, String> {
    let oracle = format!(
        r##"gem "minitest", "={}"
require "minitest/test"
Minitest.seed = 4242
require "json"
require "active_support"
require "active_support/test_case"
$LOAD_PATH.unshift(File.join(Dir.pwd, "test"))
$LOADED_FEATURES << File.join(Dir.pwd, "test/test_helper.rb")
load {test:?}
{class}.runnable_methods.sort.each do |name|
  result = {class}.new(name).run
  status = result.passed? ? "pass" : (result.skipped? ? "skip" : (result.error? ? "error" : "fail"))
  puts JSON.generate(id: "{class}##{{name}}", status: status)
end
"##,
        minitest_version()
    );
    let out = emit_and_run::ruby().args(["-e", &oracle]).current_dir(root).output().expect("native Minitest");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    rows(&String::from_utf8_lossy(&out.stdout))
}

/// Every emitted test file of `test/roundhouse_tests.json`, one process
/// each as the target runner does, in the test environment; `id =>
/// status`, and the stderr.
fn emitted_ids(emitted: &std::path::Path) -> (BTreeMap<String, String>, String) {
    let json: Value =
        serde_json::from_str(&std::fs::read_to_string(emitted.join("test/roundhouse_tests.json")).expect("test index"))
            .unwrap();
    let mut files: Vec<String> = json["sources"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|s| s["classes"].as_array().unwrap().iter().map(|c| c["file"].as_str().unwrap().to_string()))
        .collect();
    files.sort();
    files.dedup();
    let mut ran = BTreeMap::new();
    let mut stderr = String::new();
    for file in files {
        let results = emitted.join(format!("{}.results.jsonl", file.replace('/', "_")));
        let out = emit_and_run::ruby()
            .args(["-Itest", "-e", &format!("gem 'minitest', '={}'; load ARGV.fetch(0)", minitest_version()), &file])
            .current_dir(emitted)
            .env("ROUNDHOUSE_TEST_RESULTS", &results)
            .env("BLOG_DB", ":memory:")
            // As the unit-1000 lane runs it (`bin/run-target`): the test
            // load path's features answer in the test environment.
            .env("RAILS_ENV", "test")
            .output()
            .expect("emitted CRuby");
        let log = format!(
            "{file}\nstdout={}\nstderr={}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.status.success(), "{log}");
        stderr.push_str(&String::from_utf8_lossy(&out.stderr));
        let text = std::fs::read_to_string(&results).unwrap_or_else(|e| panic!("no results: {e}\n{log}"));
        for (id, status) in rows(&text) {
            assert!(ran.insert(id.clone(), status).is_none(), "{id} ran twice");
        }
    }
    (ran, stderr)
}

#[test]
fn a_test_files_top_level_requires_load_its_support_files() {
    let test = format!(
        "# frozen_string_literal: true\n\nrequire \"test_helper\"\nrequire \"securerandom\"\nrequire \"support/helpers/delivery/checkout_one/mock_factory\"\nrequire_relative \"../../support/relative_helper\"\n{MACRO_TEST_BODY}"
    );
    let sources = [
        ("test/support/helpers/delivery/checkout_one/mock_factory.rb", MOCK_FACTORY),
        ("test/support/helpers/checkouts/one/other_factory.rb", OTHER_FACTORY),
        ("test/support/relative_helper.rb", RELATIVE_HELPER),
        ("test/models/checkouts/delivery_macro_test.rb", test.as_str()),
    ];
    let native_root = std::env::temp_dir().join(format!("rh-file-requires-native-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&native_root);
    for (path, content) in sources {
        let full = native_root.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, content).unwrap();
    }
    let class = "Checkouts::One::DeliveryMacroTest";
    let native = native_ids(&native_root, "test/models/checkouts/delivery_macro_test.rb", class);
    let _ = std::fs::remove_dir_all(&native_root);
    assert_eq!(native.len(), 2, "native oracle: {native:#?}");
    assert!(native.values().all(|s| s == "pass"), "native outcomes: {native:#?}");

    let mut app = emit_and_run::empty_app()
        .write("app/controllers/application_controller.rb", "class ApplicationController < ActionController::Base\nend\n")
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("roundhouse.yml", "test_paths:\n  - test/support/helpers\n  - test/support\n");
    for (path, content) in sources {
        app = app.write(path, content);
    }
    let (emitted, errors) = app.emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let index = std::fs::read_to_string(emitted.join("test/roundhouse_tests.json")).unwrap();
    let file: Value = serde_json::from_str(&index).unwrap();
    let emitted_test = file["sources"][0]["classes"][0]["file"].as_str().unwrap().to_string();
    let content = std::fs::read_to_string(emitted.join(&emitted_test)).unwrap();
    // In source order, each where it can fail by name; the helper's own
    // `require "test_helper"` is the preamble's.
    let site = "test/models/checkouts/delivery_macro_test.rb";
    let gem = format!("RoundhouseAutoload.file_statement(\"{site}:4\") {{ require \"securerandom\" }}");
    let support = format!(
        "RoundhouseAutoload.file_statement(\"{site}:5\") {{ require \"support/helpers/delivery/checkout_one/mock_factory\" }}"
    );
    let relative = format!("RoundhouseAutoload.file_statement(\"{site}:6\") {{ require \"support/relative_helper\" }}");
    let at = |line: &str| content.find(line).unwrap_or_else(|| panic!("no `{line}` in\n{content}"));
    assert!(at(&gem) < at(&support) && at(&support) < at(&relative), "{content}");
    assert!(!content.contains("require \"test_helper\""), "{content}");
    let (ran, stderr) = emitted_ids(&emitted);
    assert_eq!(ran, native, "emitted results vs native\n{stderr}");
    assert!(!stderr.contains("file_statement_not_run"), "{stderr}");
}

#[test]
fn a_load_of_no_literal_file_is_named_where_it_runs() {
    let test = "require \"test_helper\"\nrequire_relative \"../missing_helper\"\nrequire File.join(__dir__, \"x\")\n\nclass LoadsTest < ActiveSupport::TestCase\n  test \"runs\" do\n    assert true\n  end\nend\n";
    let (emitted, errors) = emit_and_run::empty_app()
        .write("app/controllers/application_controller.rb", "class ApplicationController < ActionController::Base\nend\n")
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("test/models/loads_test.rb", test)
        .emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let (ran, stderr) = emitted_ids(&emitted);
    assert_eq!(ran.values().collect::<Vec<_>>(), vec!["pass"], "{ran:#?}\n{stderr}");
    assert!(
        stderr.contains("the file-level statement at test/models/loads_test.rb:2 did not run: LoadError: cannot load such file -- missing_helper"),
        "{stderr}"
    );
    assert!(
        stderr.contains("the file-level statement at test/models/loads_test.rb:3 did not run: NotImplementedError: roundhouse: `require File.join(__dir__, \"x\")` is not supported (all targets): it loads no literal file"),
        "{stderr}"
    );
}

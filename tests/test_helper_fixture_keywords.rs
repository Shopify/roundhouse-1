//! A fixture record passed by keyword to a test class's keyword helper.
//!
//! Core's `cart_transforms_test.rb` includes `Merchandising::
//! CartTransformTestHelper`, which includes a module whose `included do
//! setup … end` declares for its includers, so the test-ingest splice
//! (`ingest::test_mixin`) copies the helper's methods onto the test
//! class. A call to one of the class's own keyword helpers is respelled
//! to marked positionals (`lower::helper_kwargs`), and the fixture
//! accessor rewrite (`lower::rewrite_fixture_calls`) then rebuilt the
//! `api_clients(:translation_app)` value without its keyword mark:
//!
//! ```text
//! build_cart_transforms(shop: @shop, api_client: api_clients(:translation_app))  # Core
//! build_cart_transforms(shop: @shop, ApiClientsFixtures.translation_app)        # emitted
//! ```
//!
//! which does not parse (unit-1000's one `emitted_ruby_syntax` error). The
//! same drop wrote a call whose first keyword was a fixture as a
//! positional argument (`label(PostsFixtures.two, tag: "c")`), which
//! parses and raises `ArgumentError` at run time, and it happened for a
//! helper the class defines itself too, not only a spliced one.
//!
//! The native oracle is Core's pinned activerecord on sqlite3 with real
//! `ActiveRecord::TestFixtures`; the emitted CRuby tree must list and run
//! the same ids with the same outcomes and assertion counts.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;
use serde_json::Value;
use std::collections::BTreeMap;
use std::process::Command;

const SCHEMA: &str = r#"ActiveRecord::Schema.define do
  create_table "posts", force: :cascade do |t|
    t.string "title", null: false
  end
end
"#;

const FIXTURES: &str = "one:\n  title: One\n\ntwo:\n  title: Two\n";

/// Declares a setup for its includers, so a module including it is a
/// shared test module the splice copies onto the test class.
const SETUP_TRACKING: &str = r##"module SetupTracking
  extend ActiveSupport::Concern

  included do
    setup do
      @tracked = true
    end
  end
end
"##;

/// `CartTransformTestHelper`'s shape: optional keywords defaulting to
/// nil, a keyword reassigned when nil, a shorthand `suffix:` forward to a
/// private helper with required keywords.
const POST_TEST_HELPER: &str = r##"module PostTestHelper
  extend ActiveSupport::Concern
  include SetupTracking

  #: (?title: String?, ?post: Post?, ?suffix: String) -> String
  def describe_post(title: nil, post: nil, suffix: "!")
    post = Post.find_by!(title: "One") if post.nil?
    label_for(title: title || "untitled", post: post, suffix:)
  end

  #: (posts: Array[Post], ?joiner: String) -> String
  def join_titles(posts:, joiner: "+")
    posts.map(&:title).join(joiner)
  end

  private

  #: (title: String, post: Post, suffix: String) -> String
  def label_for(title:, post:, suffix:)
    "#{title}:#{post.title}#{suffix}"
  end
end
"##;

const POST_TEST: &str = r##"module Blog
  module Admin
    class PostTest < ActiveSupport::TestCase
      include PostTestHelper

      setup do
        @title = "a"
      end

      test "a fixture record as a spliced helper's keyword" do
        assert @tracked
        assert_equal "a:Two!", describe_post(title: @title, post: posts(:two))
      end

      test "a fixture record after a skipped keyword" do
        assert_equal "untitled:Two?", describe_post(post: posts(:two), suffix: "?")
      end

      test "the keyword left to its default" do
        assert_equal "b:One!", describe_post(title: "b")
      end

      test "several fixture labels as one keyword" do
        assert_equal "One+Two", join_titles(posts: posts(:one, :two))
      end

      test "a fixture record as the class's own helper's keyword" do
        assert_equal "Two/c", own_label(post: posts(:two), tag: "c")
      end

      private

      def own_label(post: nil, tag: "x")
        "#{post.title}/#{tag}"
      end
    end
  end
end
"##;

const MODELS: &str = "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n\nclass Post < ApplicationRecord\nend\n";

fn minitest_version() -> String {
    include_str!("../runtime/spinel/scaffold/Gemfile.lock")
        .lines()
        .find_map(|line| line.strip_prefix("    minitest (")?.strip_suffix(')'))
        .expect("scaffold Minitest lock")
        .to_string()
}

/// `id => (status, assertions)`, one row per line.
fn rows(text: &str) -> BTreeMap<String, (String, u64)> {
    let mut map = BTreeMap::new();
    for line in text.lines().filter(|l| l.starts_with('{')) {
        let row: Value = serde_json::from_str(line).expect("JSON row");
        let id = row["id"].as_str().unwrap().to_string();
        let outcome = (row["status"].as_str().unwrap().to_string(), row["assertions"].as_u64().unwrap());
        assert!(map.insert(id.clone(), outcome).is_none(), "{id} reported twice");
    }
    map
}

/// The test class run by native Minitest on Core's pinned Rails, with
/// `ActiveRecord::TestFixtures` loading the same YAML.
fn native() -> BTreeMap<String, (String, u64)> {
    let oracle = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/rails_oracle.rb");
    let dir = std::env::temp_dir().join(format!("rh_test_helper_fixture_keywords_native_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for (path, text) in [
        ("schema.rb", SCHEMA),
        ("models.rb", MODELS),
        ("fixtures/posts.yml", FIXTURES),
        ("setup_tracking.rb", SETUP_TRACKING),
        ("post_test_helper.rb", POST_TEST_HELPER),
        ("post_test.rb", POST_TEST),
    ] {
        let file = dir.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    let program = format!(
        r##"require {oracle:?}
RailsOracle.activate("activerecord", "activesupport", sqlite3: "2.9.2", minitest: "5.27.0")
require "minitest"
require "json"
Minitest.seed = 4242
require "active_support/all"
require "active_support/test_case"
require "active_record"
require "active_record/fixtures"
require "active_record/test_fixtures"
abort "native: activerecord is not Core's" unless Gem.loaded_specs["activerecord"].full_gem_path.start_with?(RailsOracle::CORE_GEM_HOME)
ActiveRecord::Base.establish_connection(adapter: "sqlite3", database: ":memory:")
ActiveRecord::Migration.verbose = false
load "schema.rb"
load "models.rb"
class ActiveSupport::TestCase
  include ActiveRecord::TestFixtures
  self.fixture_paths = [File.expand_path("fixtures")]
  fixtures :all
end
load "setup_tracking.rb"
load "post_test_helper.rb"
load "post_test.rb"
Blog::Admin::PostTest.runnable_methods.each do |name|
  result = Blog::Admin::PostTest.new(name).run
  status = result.passed? ? "pass" : (result.skipped? ? "skip" : (result.error? ? "error" : "fail"))
  puts JSON.generate(id: "Blog::Admin::PostTest##{{name}}", status: status, assertions: result.assertions)
end
"##
    );
    std::fs::write(dir.join("native.rb"), program).unwrap();
    let out = Command::new("ruby")
        .arg("native.rb")
        .current_dir(&dir)
        .env("GLIBC_TUNABLES", "glibc.rtld.optional_static_tls=262144")
        .output()
        .expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "native run failed:\n{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    std::fs::remove_dir_all(&dir).unwrap();
    rows(&stdout)
}

#[test]
fn fixture_records_passed_by_keyword_keep_their_keywords() {
    let native = native();
    let want = [
        ("Blog::Admin::PostTest#test_a_fixture_record_after_a_skipped_keyword", 1),
        ("Blog::Admin::PostTest#test_a_fixture_record_as_a_spliced_helper's_keyword", 2),
        ("Blog::Admin::PostTest#test_a_fixture_record_as_the_class's_own_helper's_keyword", 1),
        ("Blog::Admin::PostTest#test_several_fixture_labels_as_one_keyword", 1),
        ("Blog::Admin::PostTest#test_the_keyword_left_to_its_default", 1),
    ];
    let expected: BTreeMap<String, (String, u64)> =
        want.iter().map(|(id, n)| (id.to_string(), ("pass".to_string(), *n))).collect();
    assert_eq!(native, expected, "native oracle");

    let (emitted, errors) = emit_and_run::empty_app()
        .write("app/controllers/application_controller.rb", "class ApplicationController < ActionController::Base\nend\n")
        .write("config/routes.rb", "Rails.application.routes.draw do\nend\n")
        .write("db/schema.rb", SCHEMA)
        .write("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n")
        .write("app/models/post.rb", "class Post < ApplicationRecord\nend\n")
        .write("test/fixtures/posts.yml", FIXTURES)
        // As the Core slice lists the directories of required test files.
        .write("roundhouse.yml", "test_paths:\n  - test/support\n")
        .write("test/support/setup_tracking.rb", SETUP_TRACKING)
        .write("test/support/post_test_helper.rb", POST_TEST_HELPER)
        .write("test/models/post_test.rb", POST_TEST)
        .emit(BuildTarget::Ruby);
    // An `emitted_ruby_syntax` error is how the dropped keyword surfaced.
    assert!(errors.is_empty(), "{errors:#?}");

    let file = "test/models/blog_admin_post_test.rb";
    let source = std::fs::read_to_string(emitted.join(file)).expect("emitted test file");
    // The helper is spliced onto the class (the shape Core's file takes),
    // and each fixture value keeps the keyword the source wrote.
    assert!(source.contains("def describe_post("), "{source}");
    assert!(!source.contains("include PostTestHelper"), "{source}");
    for call in [
        "describe_post(title: @title, post: PostsFixtures.two)",
        "describe_post(post: PostsFixtures.two, suffix: \"?\")",
        "own_label(post: PostsFixtures.two, tag: \"c\")",
    ] {
        assert!(source.contains(call), "`{call}` in:\n{source}");
    }

    let index: Value = serde_json::from_str(&std::fs::read_to_string(emitted.join("test/roundhouse_tests.json")).expect("test index")).unwrap();
    let mut indexed: Vec<String> = index["sources"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|s| s["classes"].as_array().unwrap().iter())
        .filter(|c| c["file"] == file)
        .flat_map(|c| {
            let class = c["class"].as_str().unwrap().to_string();
            c["tests"].as_array().unwrap().iter().map(move |t| format!("{class}#{}", t.as_str().unwrap()))
        })
        .collect();
    indexed.sort();
    assert_eq!(indexed, native.keys().cloned().collect::<Vec<_>>(), "index lists exactly the native ids");

    let results = emitted.join("post_test.results.jsonl");
    let out = emit_and_run::ruby()
        .args(["-Itest", "-e", &format!("gem 'minitest', '={}'; load ARGV.fetch(0)", minitest_version()), file])
        .current_dir(&emitted)
        .env("ROUNDHOUSE_TEST_RESULTS", &results)
        .env("BLOG_DB", ":memory:")
        .output()
        .expect("emitted CRuby");
    let log = format!("stdout={}\nstderr={}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success(), "{log}");
    let text = std::fs::read_to_string(&results).unwrap_or_else(|e| panic!("no results: {e}\n{log}"));
    assert_eq!(rows(&text), native, "emitted results vs native");
}

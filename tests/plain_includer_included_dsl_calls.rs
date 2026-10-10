//! A Concern's `included do` that calls the includer's own class-side DSL
//! runs on a plain (non-model) includer.
//!
//! Core's Sales::Query::Concerns::SalesAggregateValuesBatchLoading
//! declares `field(:sales_aggregate, ...)` (QueryLayer::Response's class
//! method) and a `class_attribute :sale_aggregate_mapping, default: {}`
//! in its block; Pagination::TaggableQuery declares
//! `filter_param :tag, ... do |f| ... end`, which an includer's body then
//! extends with `extend_filter_param :tag`. Ingest modeled the
//! class_attribute but dropped the DSL calls (refusing them by name), so
//! the includer's body found no `:tag` filter and the response class no
//! `:sales_aggregate` field. Ruby class_evals the block on the includer:
//! each call runs there, as written, right after the `include`.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;
use std::process::Command;

const RESPONSE: &str = r#"module QueryLayer
  class Response
    class_attribute :fields, default: {}

    def self.field(name, depends_on: [], batch_load: false)
      self.fields = fields.merge(name => depends_on)
    end
  end
end
"#;

const SALES_AGGREGATE: &str = r#"module SalesAggregate
  extend ActiveSupport::Concern
  extend T::Helpers

  abstract!

  requires_ancestor { QueryLayer::Response }

  included do
    field(
      :sales_aggregate,
      depends_on: [:id, :record],
      batch_load: true
    )

    class_attribute(
      :sale_aggregate_mapping,
      instance_accessor: false,
      instance_predicate: false,
      instance_writer: false,
      instance_reader: true,
      default: {}
    )
  end

  class_methods do
    def sales_aggregate_field(field_name, sale_fields:, depends_on: [])
      field(field_name, depends_on: [:sales_aggregate, *depends_on])
      klass = self
      klass.sale_aggregate_mapping[field_name] = Array.wrap(sale_fields)
    end
  end
end
"#;

const LINE_ITEM_RESPONSE: &str = r#"class LineItemResponse < QueryLayer::Response
  include SalesAggregate

  sales_aggregate_field :total, sale_fields: "amount"
end
"#;

const BASE_QUERY: &str = r#"class BaseQuery
  class_attribute :filters, default: {}

  def self.filter_param(name, type, description: nil, example_use: [], &block)
    self.filters = filters.merge(name => [type, description, block.call(name)])
  end

  def self.extend_filter_param(name)
    raise ArgumentError, "no filter #{name}" unless filters.key?(name)
    self.filters = filters.merge(name => filters[name] + [:extended])
  end
end
"#;

const TAGGABLE_QUERY: &str = r##"module TaggableQuery
  extend T::Helpers
  requires_ancestor { Kernel }

  extend ActiveSupport::Concern

  def tag_name
    raise NotImplementedError
  end

  included do
    filter_param :tag,
      :string,
      description: "Filter objects by the `tag` field.",
      example_use: ["tag:my_tag"] do |f|
      "#{f}:#{name}"
    end
  end
end
"##;

const ARTICLE_QUERY: &str = r#"class ArticleQuery < BaseQuery
  include TaggableQuery

  extend_filter_param :tag
end
"#;

const OBSERVE: &str = r#"p LineItemResponse.fields
p LineItemResponse.sale_aggregate_mapping
p LineItemResponse.new.sale_aggregate_mapping
p QueryLayer::Response.fields
p ArticleQuery.filters
p BaseQuery.filters
"#;

const FILES: &[(&str, &str)] = &[
    ("app/models/query_layer/response.rb", RESPONSE),
    ("app/models/sales_aggregate.rb", SALES_AGGREGATE),
    ("app/models/line_item_response.rb", LINE_ITEM_RESPONSE),
    ("app/models/base_query.rb", BASE_QUERY),
    ("app/models/taggable_query.rb", TAGGABLE_QUERY),
    ("app/models/article_query.rb", ARTICLE_QUERY),
];

fn requires() -> String {
    FILES.iter().map(|(path, _)| format!("require_relative {:?}\n", path.trim_end_matches(".rb"))).collect()
}

/// The same files on Core's pinned activesupport with sorbet-runtime.
fn native() -> String {
    let oracle = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/rails_oracle.rb");
    let dir = std::env::temp_dir().join(format!("rh_plain_includer_dsl_native_{}", std::process::id()));
    for (path, text) in FILES {
        let file = dir.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    let program = format!(
        "require {oracle:?}\nRailsOracle.activate(\"activesupport\")\n\
         require \"sorbet-runtime\"\nrequire \"active_support\"\nrequire \"active_support/core_ext\"\n\
         {}{OBSERVE}",
        requires()
    );
    std::fs::write(dir.join("native.rb"), program).unwrap();
    let out = Command::new("ruby")
        .args(["-W0", "native.rb"])
        .current_dir(&dir)
        .env("GLIBC_TUNABLES", "glibc.rtld.optional_static_tls=262144")
        .output()
        .expect("ruby is on PATH");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "native run failed:\n{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    std::fs::remove_dir_all(&dir).unwrap();
    stdout
}

#[test]
fn included_dsl_calls_run_on_a_plain_includer_as_native() {
    let mut app = emit_and_run::empty_app().write("roundhouse.yml", "test_paths:\n  - test\n");
    for (path, text) in FILES {
        app = app.write(path, text);
    }
    let (dir, errors) = app.emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let expected = native();
    let script = format!("require File.expand_path('main', Dir.pwd)\n{}{OBSERVE}", requires());
    let out = emit_and_run::ruby()
        .current_dir(&*dir)
        .args(["-W0", "-e", &script])
        .env("BLOG_DB", ":memory:")
        .output()
        .expect("ruby");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let emitted = |path: &str| std::fs::read_to_string(dir.join(path)).unwrap_or_default();
    assert!(
        out.status.success(),
        "emitted run failed in {}:\n{stdout}\n{}\n{}\n{}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr),
        emitted("app/models/line_item_response.rb"),
        emitted("app/models/article_query.rb"),
    );
    assert_eq!(stdout, expected, "emitted vs native activesupport");
}

/// The block's calls and its class_attribute seeds run in the block's
/// order: `configure` writes the inherited attribute, then the block's
/// own `class_attribute :limit, default: 1` resets it, as in Ruby.
#[test]
fn a_call_and_a_class_attribute_seed_keep_the_block_order() {
    let concern = "module Seeding\n  extend ActiveSupport::Concern\n\n  included do\n    configure(2)\n    class_attribute :limit, default: 1\n    configure_more(3)\n  end\nend\n";
    let base = "class Configurable\n  class_attribute :limit, default: 0\n  class_attribute :more, default: 0\n\n  def self.configure(value)\n    self.limit = value\n  end\n\n  def self.configure_more(value)\n    self.more = limit + value\n  end\nend\n";
    let includer = "class Configured < Configurable\n  include Seeding\nend\n";
    let files = [
        ("app/models/configurable.rb", base),
        ("app/models/seeding.rb", concern),
        ("app/models/configured.rb", includer),
    ];
    let mut app = emit_and_run::empty_app().write("roundhouse.yml", "test_paths:\n  - test\n");
    for (path, text) in files {
        app = app.write(path, text);
    }
    let (dir, errors) = app.emit(BuildTarget::Ruby);
    assert!(errors.is_empty(), "{errors:#?}");
    let script = "require File.expand_path('main', Dir.pwd)\nrequire_relative 'app/models/configurable'\nrequire_relative 'app/models/seeding'\nrequire_relative 'app/models/configured'\np [Configured.limit, Configured.more, Configurable.limit, Configurable.more]\n";
    let out = emit_and_run::ruby()
        .current_dir(&*dir)
        .args(["-W0", "-e", script])
        .env("BLOG_DB", ":memory:")
        .output()
        .expect("ruby");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let emitted = std::fs::read_to_string(dir.join("app/models/configured.rb")).unwrap_or_default();
    assert!(out.status.success(), "{stdout}\n{}\n{emitted}", String::from_utf8_lossy(&out.stderr));
    // Native: configure(2) sets 2, the block's class_attribute resets
    // limit to 1, configure_more(3) reads it: more = 4.
    assert_eq!(stdout, "[1, 4, 0, 0]\n", "{emitted}");
}

/// A call naming no class-side method of the app (a framework DSL a
/// plain class may not have on this runtime) is still refused by name.
#[test]
fn a_call_to_no_app_class_method_stays_refused() {
    let concern = "module Tracking\n  extend ActiveSupport::Concern\n\n  included do\n    track_everything :x\n  end\nend\n";
    let includer = "class Tracked\n  include Tracking\nend\n";
    let tree = [
        ("app/models/tracking.rb", concern),
        ("app/models/tracked.rb", includer),
        ("roundhouse.yml", "test_paths:\n  - test\n"),
    ]
    .iter()
    .map(|(path, text)| (std::path::PathBuf::from(path), text.as_bytes().to_vec()))
    .collect();
    let error = roundhouse::ingest::ingest_app_from_tree(tree).expect_err("refused at ingest");
    let message = format!("{error:?}");
    assert!(
        message.contains("`track_everything` in `included do` of Tracking is not modeled on a plain (non-model) includer"),
        "{message}"
    );
}

/// Core's shape: the includer's superclass is the query_layer gem's
/// `QueryLayer::Response`, whose RBI declares the class method `field`.
/// The emitted tree loads the gem, so the call is replayed on the
/// includer after its `include`, not dropped.
#[test]
fn a_call_a_gem_superclass_answers_is_replayed() {
    const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    activesupport (8.0.2)\n    query_layer (0.1.27)\n\nDEPENDENCIES\n  activesupport\n  query_layer (= 0.1.27)\n";
    const RBI: &str = "# typed: true\n\nmodule QueryLayer; end\n\nclass QueryLayer::Response\n  class << self\n    def field(name, batch_load: T.unsafe(nil), delegate_subfields: T.unsafe(nil), **kwargs); end\n  end\nend\n";
    let concern = "module Aggregating\n  extend ActiveSupport::Concern\n\n  included do\n    field(:sales_aggregate, depends_on: [:id], batch_load: true)\n  end\nend\n";
    let includer = "class LineItemResponse < QueryLayer::Response\n  include Aggregating\nend\n";
    let tree = [
        ("Gemfile.lock", LOCK),
        ("sorbet/rbi/gems/query_layer@0.1.27.rbi", RBI),
        ("app/models/aggregating.rb", concern),
        ("app/models/line_item_response.rb", includer),
        ("roundhouse.yml", "test_paths:\n  - test\nextensions:\n  - ext\n"),
        ("ext/gems.yml", "lock_gated_requires:\n  - {require: query_layer, gem: query_layer}\n"),
    ]
    .iter()
    .map(|(path, text)| (std::path::PathBuf::from(path), text.as_bytes().to_vec()))
    .collect();
    let app = roundhouse::ingest::ingest_app_from_tree(tree).expect("the block's call is replayed, not refused");
    let statements = app
        .included_block_statements
        .get(&roundhouse::ident::ClassId(roundhouse::ident::Symbol::from("LineItemResponse")))
        .expect("LineItemResponse carries the block's statement");
    let text: Vec<String> = statements.iter().map(|(after, e)| format!("{} {}", after.0, roundhouse::emit::ruby::emit_expr(e))).collect();
    assert_eq!(text, vec!["Aggregating field(:sales_aggregate, depends_on: [:id], batch_load: true)".to_string()]);
}

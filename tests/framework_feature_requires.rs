// A native gem's Rails feature requires on the PostgreSQL CRuby tree
// (project.rs::FRAMEWORK_FEATURE_PORTS, runtime/features), against the
// Rails native Core runs (Shopify/rails 5813559f4aed;
// tests/support/rails_oracle.rb). tests/framework_feature_requires.rb
// loads one gem natively, after Core's framework railties, and on the
// emitted tree, and requires every observation equal.
//
// Core's boot died in each of these gems (`file_statement_not_run`:
// "cannot load such file -- active_job", job-iteration; "--
// action_controller", responders and typed_parameters; "--
// active_support/multibyte/unicode", identifications): the tree had no
// file for the feature, where natively Rails had loaded it before any
// gem. The features it does not port fail by name instead
// (FRAMEWORK_FEATURES_NOT_PORTED).
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use roundhouse::project::BuildTarget;
use std::path::Path;

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

const LOCK: &str = "GEM\n  remote: https://rubygems.org/\n  specs:\n    concurrent-ruby (1.3.5)\n    i18n (1.15.2)\n      concurrent-ruby (~> 1.0)\n\n\
DEPENDENCIES\n  i18n\n";

// What a gem in the gem home does as it loads, and what it then reads.
const PROBE_GEM: &str = r#"require "active_support"
require "active_model"
require "active_record"
require "action_controller"
require "action_mailer"
require "active_job"
require "active_support/core_ext/object/blank"
require "active_support/multibyte/unicode"
require "active_support/core_ext/module/attribute_accessors_per_thread"
# softer_delete's, polaris', typed_parameters' and Core's component
# gems' sub-feature requires.
require "active_support/concern"
require "active_support/core_ext/object/try"
require "active_support/core_ext/hash/keys"
require "active_support/lazy_load_hooks"
require "active_support/notifications"
require "active_support/logger"
require "action_controller/metal/exceptions"
require "action_view"
require "active_support/core_ext/string/strip"

require "rails/engine"

module ProbeGem
  ROUTES_RAN = []

  # graphql 2.6.11's GraphQL::Dashboard shape: an engine whose routes
  # are appended at class load and drawn only when the routes load.
  class Dashboard < Rails::Engine
    engine_name "probe_gem_dashboard"
    isolate_namespace(ProbeGem::Dashboard)

    routes do
      ROUTES_RAN << :drawn
      root "landings#show"
    end
  end

  # turbo-rails 2.x Turbo::Current's shape.
  class Current
    thread_mattr_accessor :request_id
    thread_mattr_accessor :level, default: :info
  end

  # tailwindcss-rails 4.1.0's and sassc-rails 2.1.2's generator defaults.
  class Railtie < Rails::Railtie
    config.app_generators do |g|
      g.template_engine :tailwindcss
    end
    config.app_generators.stylesheet_engine :sass
    config.app_generators.test_unit fixture: false, aliases: { f: :fixture }
  end

  # softer_delete's shape: a concern with an `included` block, class
  # methods and a concern it depends on.
  module Stamped
    extend ActiveSupport::Concern

    included do
      attr_accessor :stamp
    end

    class_methods do
      def stamped? = true
    end
  end

  module SoftDeletable
    extend ActiveSupport::Concern
    include Stamped

    module ClassMethods
      def deleted_scope = (name.split("::").last.downcase + "_deleted").to_sym
    end

    included do
      self.deletable_columns = [:deleted_at]
    end

    prepended do
      @prepended_into = true
    end
  end

  class Record
    class << self
      attr_accessor :deletable_columns
    end
    include SoftDeletable
  end

  class PrependedRecord
    class << self
      attr_accessor :deletable_columns
    end
    prepend SoftDeletable
  end

  ActiveSupport.on_load(:probe_gem_hook) { HOOKS << [:ran, self] }
  HOOKS = []

  def self.concern
    twice = begin
      Module.new do
        extend ActiveSupport::Concern
        included { 1 }
        included { 2 }
      end
    rescue ActiveSupport::Concern::MultipleIncludedBlocks => e
      e.message
    end
    r = Record.new
    r.stamp = :s
    [Record.ancestors.take(3).map(&:name), Record.stamped?, Record.deleted_scope, Record.deletable_columns, r.stamp,
     PrependedRecord.ancestors.take(3).map(&:name), PrependedRecord.deleted_scope, PrependedRecord.instance_variable_get(:@prepended_into), twice]
  end

  def self.try
    [nil.try(:length), "abc".try(:length), "abc".try(:nope), "abc".try { |s| s.upcase }, nil.try!(:length),
     begin; "abc".try!(:nope); rescue NoMethodError => e; e.name; end]
  end

  def self.hash_keys
    h = { a: { b: [{ c: 1 }] }, "d" => 2, 3 => :x }
    bang = { a: 1, "b" => 2 }
    bang.stringify_keys!
    invalid = begin
      { a: 1, z: 2 }.assert_valid_keys(:a, [:b])
    rescue ArgumentError => e
      e.message
    end
    [h.stringify_keys, h.symbolize_keys, h.deep_stringify_keys, h.deep_symbolize_keys, h.to_options, bang,
     h.deep_transform_keys(&:to_s), { a: 1 }.assert_valid_keys(:a, :b), invalid]
  end

  # typed_parameters' deprecation warning shape.
  def self.strip
    usage = <<-EOM
      This command does such and such.

      Supported options are:
        -h         This message
    EOM
    frozen = "  a\n    b\n".freeze
    [usage.strip_heredoc, "x\n  y".strip_heredoc, "".strip_heredoc, "  a\n\n  b\r\n".strip_heredoc,
     frozen.strip_heredoc, frozen.strip_heredoc.frozen?, "  a".dup.strip_heredoc.frozen?, "\t\tq\n\t r".strip_heredoc,
     "  \n    z".strip_heredoc]
  end

  def self.notifications
    events = []
    subscriber = ActiveSupport::Notifications.subscribe("probe_gem.ran") { |event| events << [event.name, event.payload] }
    result = ActiveSupport::Notifications.instrument("probe_gem.ran", id: 7) { :done }
    ActiveSupport::Notifications.unsubscribe(subscriber)
    ActiveSupport::Notifications.instrument("probe_gem.ran", id: 8)
    [result, events]
  end

  def self.logger
    io = StringIO.new
    logger = ActiveSupport::Logger.new(io)
    logger.info("hello")
    logger.debug { "lazy" }
    logger.level = :warn
    logger.info("dropped")
    logger.silence { logger.error("silenced") }
    logger.warn("kept")
    [ActiveSupport::Logger.ancestors.include?(::Logger), io.string]
  end

  def self.controller_errors
    ac = ActionController
    chain = ->(klass) { klass.ancestors.take_while { |k| k != StandardError }.map(&:name) }
    routing = ac::RoutingError.new("No route", [:x])
    [chain[ac::BadRequest], chain[ac::RoutingError], chain[ac::UnknownFormat], chain[ac::MissingExactTemplate], chain[ac::NotImplemented],
     ac::BadRequest.new("Invalid parameter: x").message, ac::MethodNotAllowed.new(:get, :post, :put).message,
     ac::SessionOverflowError.new.message.split(".").first, ac::RespondToMismatchError.new.message.split(".").first,
     [routing.message, routing.failures], ac::MissingExactTemplate.new("m", :c, :a).then { |e| [e.message, e.controller, e.action_name] },
     ac::UrlGenerationError.new("u", nil, :r, :m).then { |e| [e.routes, e.route_name, e.method_name] },
     [ac::RenderError, ac::MissingFile, ac::UnknownHttpMethod, ac::TooManyRequests].map { |k| k.superclass.name },
     ActionView::MissingTemplate < StandardError]
  end

  def self.report
    unicode = ActiveSupport::Multibyte::Unicode
    latin1 = "caf\xE9".dup.force_encoding(Encoding::UTF_8)
    {
      frameworks: [ActiveRecord::Base, ActionController::Base, ActionMailer::Base, ActiveJob::Base, ActiveModel::Type::Value].map(&:name),
      blank: [nil.blank?, "  ".blank?, [].present?, { a: 1 }.present?],
      unicode_version: unicode::UNICODE_VERSION,
      tidy_bytes: unicode.tidy_bytes(latin1),
      tidy_bytes_valid: unicode.tidy_bytes("café"),
      tidy_bytes_forced: unicode.tidy_bytes("caf\xE9".b, true),
      compose: unicode.compose("é".codepoints),
      decompose: unicode.decompose(:canonical, "é".codepoints),
      decompose_compatibility: unicode.decompose(:compatibility, "ﬁ".codepoints),
      transliterate: ActiveSupport::Inflector.transliterate(latin1),
      parameterize: "Ærøskøbing Straße".parameterize,
      per_thread: per_thread,
      app_generators: generators,
      engine: engine,
      concern: concern,
      try: try,
      hash_keys: hash_keys,
      strip: strip,
      load_hooks: (ActiveSupport.run_load_hooks(:probe_gem_hook, :base); HOOKS),
      notifications: notifications,
      logger: logger,
      controller_errors: controller_errors,
    }
  end

  def self.engine
    d = Dashboard
    isolated = [:railtie_namespace, :table_name_prefix, :use_relative_model_naming?, :railtie_helpers_paths, :railtie_routes_url_helpers]
    [d.engine_name, d.isolated?, d.instance.engine_name, isolated.map { |m| d.respond_to?(m) }, d.routes.equal?(d.instance.routes), d.instance.routes?.nil?, ROUTES_RAN]
  end

  def self.generators
    g = Rails::Railtie::Configuration.new.app_generators
    [g.class.name, g.options[:rails].slice(:template_engine, :stylesheet_engine), g.options[:test_unit], g.aliases[:test_unit], g.template_engine]
  end

  def self.per_thread
    Current.request_id = "main"
    other = Thread.new { [Current.request_id, Current.level, (Current.new.request_id = "t"), Current.request_id] }.value
    invalid = begin
      Class.new { thread_mattr_accessor :"bad-name" }
    rescue NameError => e
      e.message
    end
    [Current.request_id, Current.new.request_id, Current.level, other, invalid]
  end
end
"#;

fn probe() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("roundhouse.yml", "test_paths:\n  - test\ndatabase_adapter: postgresql\n")
        .write("Gemfile", "source \"https://rubygems.org\"\ngem \"rails\"\ngem \"i18n\"\n")
        .write("Gemfile.lock", LOCK)
        .write("config.ru", "")
        .write("config/environments/test.rb", "Rails.application.configure do\n  config.eager_load = false\nend\n")
        .write("config/database.yml", "test:\n  adapter: postgresql\n  database: rh_framework_features_unused\n")
        .write("db/schema.rb", "ActiveRecord::Schema.define do\n  create_table \"probes\" do |t|\n    t.string \"title\"\n  end\nend\n")
        .write("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n")
        .write("app/models/probe.rb", "class Probe < ApplicationRecord\nend\n")
}

fn run(mode: &str, dir: &Path, gem: Option<&Path>) -> String {
    let mut cmd = emit_and_run::ruby();
    cmd.arg(root().join("tests/framework_feature_requires.rb")).arg("run").arg(mode).arg(dir);
    if let Some(gem) = gem {
        cmd.arg(gem);
    }
    let out = cmd.output().expect("ruby");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "{mode} failed\n=== stdout ===\n{stdout}\n=== stderr ===\n{}", String::from_utf8_lossy(&out.stderr));
    stdout
}

#[test]
fn a_gems_rails_feature_requires_answer_from_the_ports_as_natively_from_the_loaded_frameworks() {
    let (tree, _) = probe().emit(BuildTarget::Ruby);
    let scratch = tree.parent().expect("scratch").to_path_buf();
    let app = scratch.join("app");
    let gem = scratch.join("probe_gem/lib");
    std::fs::create_dir_all(&gem).unwrap();
    std::fs::write(gem.join("probe_gem.rb"), PROBE_GEM).unwrap();
    let native = run("native", &app, Some(&gem));
    let emitted = run("emitted", &tree, Some(&gem));
    assert_eq!(emitted, native, "the tree's ports answered the gem differently from Rails");
    // Not vacuous: the gem ran and read each framework.
    for expected in [
        "frameworks: [\"ActiveRecord::Base\", \"ActionController::Base\", \"ActionMailer::Base\", \"ActiveJob::Base\", \"ActiveModel::Type::Value\"]",
        "tidy_bytes: \"café\"",
        "compose: [233]",
        "decompose_compatibility: [102, 105]",
        "transliterate: \"cafe\"",
        // String#parameterize through the Inflector (not the ASCII overlay).
        "parameterize: \"aeroskobing-strasse\"",
        "app_generators: [\"Rails::Configuration::Generators\", {template_engine: :tailwindcss, stylesheet_engine: :sass}, {fixture: false}, {f: :fixture}, :tailwindcss]",
        // The route block recorded, not run; isolate_namespace's methods.
        "engine: [\"probe_gem_dashboard\", true, \"probe_gem_dashboard\", [true, false, false, false, false], true, false, []]",

        // ActiveSupport::Concern: dependencies first, ClassMethods,
        // `included`/`prepended` blocks, a second `included` refused.
        "concern: [[\"ProbeGem::Record\", \"ProbeGem::SoftDeletable\", \"ProbeGem::Stamped\"], true, :record_deleted, [:deleted_at], :s, [\"ProbeGem::SoftDeletable\", \"ProbeGem::Stamped\", \"ProbeGem::PrependedRecord\"], :prependedrecord_deleted, true, \"Cannot define multiple 'included' blocks for a Concern\"]",
        "try: [nil, 3, nil, \"ABC\", nil, :nope]",
        "strip: [\"This command does such and such.\\n\\nSupported options are:\\n  -h         This message\\n\", \"x\\n  y\"",
        "hash_keys: [{\"a\" => {b: [{c: 1}]}, \"d\" => 2, \"3\" => :x}",
        "Unknown key: :z. Valid keys are: :a, :b",
        "load_hooks: [[:ran, :base]]",
        "logger: [true, \"hello\\nlazy\\nsilenced\\nkept\\n\"]",
        "notifications: [:done, [[\"probe_gem.ran\", {id: 7}]]]",
        "controller_errors: [[\"ActionController::BadRequest\", \"ActionController::ActionControllerError\"]",
        "\"Only get, post, and put requests are allowed.\"",
        // Per thread, with the default seeded in each thread.
        "per_thread: [\"main\", \"main\", :info, [nil, :info, \"t\", \"t\"], \"invalid attribute name: bad-name\"]",
    ] {
        assert!(native.contains(expected), "missing {expected:?}:\n{native}");
    }
}

/// The features the tree does not port fail by name, never loading the
/// gem home's activesupport.
#[test]
fn a_rails_feature_the_tree_does_not_port_is_a_named_load_error() {
    let (tree, _) = probe().emit(BuildTarget::Ruby);
    let out = run("refusals", &tree, None);
    for feature in ["active_support/all", "active_support/core_ext/hash"] {
        let line = format!("{feature}: LoadError: roundhouse: error[framework_feature_not_ported]: `require {feature:?}`");
        assert!(out.contains(&line), "missing {line:?}:\n{out}");
    }
    // Every other Rails feature with no port (polaris' Numeric
    // extensions, the whole core_ext set): by name, not as a plain
    // `cannot load such file`, which is left to everything else.
    for feature in ["active_support/core_ext/numeric", "active_support/core_ext", "action_dispatch/testing/integration", "rails/no_such_feature.rb"] {
        let line = format!(
            "{feature}: LoadError: roundhouse: error[framework_feature_not_ported]: `require {feature:?}`: this tree has no port of this Rails feature (project.rs::FRAMEWORK_FEATURE_PORTS), and never loads the gem home's Rails; not loaded\n"
        );
        assert!(out.contains(&line), "missing {line:?}:\n{out}");
    }
    for line in [
        "active_model_serializers_probe: LoadError: cannot load such file -- active_model_serializers_probe\n",
        "probe_missing/active_support: LoadError: cannot load such file -- probe_missing/active_support\n",
        // A framework feature's file that loads but itself requires what
        // is missing keeps the inner LoadError.
        "active_support/probe_inner: LoadError: cannot load such file -- probe_gem_inner_dependency\n",
    ] {
        assert!(out.contains(line), "missing {line:?}:\n{out}");
    }
    assert_eq!(out.matches("framework_feature_not_ported").count(), 6, "{out}");
    assert!(out.contains("routes: NotImplementedError: error[route_set_not_drawn]: the routes block at "), "{out}");
    for method in ["url_helpers", "recognize_path"] {
        let line = format!("routes: NotImplementedError: error[route_set_not_drawn]: ActionDispatch::Routing::RouteSet#{method} needs a drawn route set");
        assert!(out.contains(&line), "missing {line:?}:\n{out}");
    }
    assert_eq!(out.matches("RouteSet#url_helpers needs a drawn route set").count(), 2, "{out}");
    assert_eq!(out.matches("routes: ").count(), 4, "{out}");
    let features = tree.join("runtime/features");
    let read = |feature: &str| std::fs::read_to_string(features.join(format!("{feature}.rb"))).unwrap_or_else(|e| panic!("{feature}: {e}"));
    assert!(read("active_job").contains("require_relative \"../active_job\"\n"), "{}", read("active_job"));
    assert!(read("active_support/multibyte/unicode").contains("require_relative \"../../../active_support_multibyte_unicode\"\n"));
    let facades = std::fs::read_to_string(tree.join("runtime/gem_facades.rb")).unwrap();
    assert!(facades.contains("require_relative \"active_support_inflector\"\nrequire_relative \"active_support_multibyte_unicode\"\n"), "{facades}");
}

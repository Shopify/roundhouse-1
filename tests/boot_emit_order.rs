//! Declarations load in the order Ruby evaluates them. Boot blockers
//! from Core's e2e boot (r16-pgb6), each checked here against native
//! Ruby running the same file.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

fn native(files: &[(&str, &str)], script: &str) -> String {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!("rh_boot_emit_order_{}_{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for (path, text) in files {
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    let output = emit_and_run::ruby().arg("-I").arg(root.join("lib")).arg("-e").arg(script).output().unwrap();
    std::fs::remove_dir_all(&root).unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Core's oid_service.rb declares `module Validation` above
/// `class Local`, which does `extend(Validation)`. The file's
/// declarations were loaded classes first at each depth, so Local's
/// body ran before Validation existed (NameError at boot).
const OID_SERVICE: &str = "require \"socket\"\n\nmodule OidService\n  class Error < StandardError; end\n\n  SCHEMA_NAME = \"core\"\n\n  module Validation\n    MAX_COUNT = 65535\n\n    def check(n) = n <= MAX_COUNT\n  end\n\n  class Local\n    extend(Validation)\n\n    def self.ok = check(3)\n  end\nend\n";

#[test]
fn a_module_declared_above_a_class_loads_before_it() {
    let script = "p OidService::Local.ok\np OidService::Error.superclass\n";
    let expected = native(&[("lib/oid_service.rb", OID_SERVICE)], &format!("require \"oid_service\"\n{script}"));
    assert_eq!(expected, "true\nStandardError\n");
    let run = emit_and_run::empty_app().write("lib/oid_service.rb", OID_SERVICE).run_ruby(script);
    assert!(run.success, "{}\n{}", run.stdout, run.stderr);
    assert_eq!(run.stdout, expected, "{}", run.stderr);
}

/// Core's gems/component: `Component::Config.inherited` autoloads
/// `Config::Test` from `config/test.rb` beside the file that defined the
/// subclass (`Object.const_source_location`). merchant_workflows writes
/// `class Test < Config` in that file of its own and an empty
/// `class Config < ::Component::Config` body. Spliced into the
/// subclass's file, `class Test` ran right after `inherited` had
/// registered the autoload, and Ruby loaded the autoload's path for
/// it, which the emitted tree did not have (LoadError at boot).
const COMPONENT_CONFIG: &str = "module Component\n  class Config\n    class << self\n      def inherited(config)\n        return super unless config.superclass == Config\n\n        if (class_name = config.name) &&\n            (config_location = Object.const_source_location(class_name)) &&\n            (parent_dirname = File.dirname(config_location[0]))\n          config_dirname = File.join(parent_dirname, \"config\")\n          config.autoload(:Test, File.join(config_dirname, \"test.rb\"))\n        end\n\n        super\n      end\n\n      def label = \"workflows\"\n\n      def for(env)\n        const_get(env.capitalize)\n      end\n    end\n  end\nend\n";
const WORKFLOWS_CONFIG: &str = "require \"component/config\"\n\nmodule MerchantWorkflowsComponent\n  class Config < ::Component::Config\n  end\nend\n";
const WORKFLOWS_CONFIG_TEST: &str = "module MerchantWorkflowsComponent\n  class Config\n    class Test < Config\n    end\n  end\nend\n";

#[test]
fn a_nested_class_written_in_its_own_file_keeps_the_file_its_autoload_names() {
    let files = [
        ("lib/component/config.rb", COMPONENT_CONFIG),
        ("lib/merchant_workflows_component/config.rb", WORKFLOWS_CONFIG),
        ("lib/merchant_workflows_component/config/test.rb", WORKFLOWS_CONFIG_TEST),
    ];
    // The emitted boot loads eagerly, which resolves the autoload; one
    // still pending names a file that exists, lazily (native) or not.
    let script = "c = MerchantWorkflowsComponent::Config\npending = c.autoload?(:Test)\np pending.nil? || File.exist?(pending)\nt = c.for(\"test\")\np t, t.superclass, t.label\n";
    let expected = native(&files, &format!("require \"merchant_workflows_component/config\"\np File.basename(MerchantWorkflowsComponent::Config.autoload?(:Test))\n{script}"));
    assert_eq!(expected, "\"test.rb\"\ntrue\nMerchantWorkflowsComponent::Config::Test\nMerchantWorkflowsComponent::Config\n\"workflows\"\n");
    let expected = expected.strip_prefix("\"test.rb\"\n").unwrap();
    let mut app = emit_and_run::empty_app();
    for (path, text) in files {
        app = app.write(path, text);
    }
    let run = app.run_ruby(script);
    assert!(run.success, "{}\n{}", run.stdout, run.stderr);
    assert_eq!(run.stdout, expected, "{}", run.stderr);
}

/// monorepo_slices' slices_file_contract.rb: the contract's constants,
/// then its validator classes, one of which reads a constant above it
/// as its body loads (`/\A#{SLICE_LABEL_PREFIX}.+\z/`), then a constant
/// built from a validator. Each nested class was spliced in ahead of
/// all of its parent's constants, so the read raised NameError at boot.
const SLICES_FILE_CONTRACT: &str = "module MonorepoSlices\n  class SlicesFileContract\n    MAX_GITHUB_LABEL_LENGTH = 50\n    SLICE_LABEL_PREFIX = \"Slice: \"\n    SLICE_LABEL_LENGTH = SLICE_LABEL_PREFIX.length\n\n    def self.validators = [DEFAULT_VALIDATOR]\n\n    class BaseValidator\n      def ok?(label) = label.length <= MAX_GITHUB_LABEL_LENGTH\n    end\n\n    class IssueTrackingLabelValidator < BaseValidator\n      ISSUE_LABEL_REGEX = /\\A#{SLICE_LABEL_PREFIX}.+\\z/\n\n      def ok?(label) = super && ISSUE_LABEL_REGEX.match?(label)\n    end\n\n    DEFAULT_VALIDATOR = IssueTrackingLabelValidator.new\n  end\nend\n";

#[test]
fn a_nested_class_loads_after_the_parent_constants_above_it() {
    let files = [("lib/monorepo_slices/slices_file_contract.rb", SLICES_FILE_CONTRACT)];
    let script = "c = MonorepoSlices::SlicesFileContract\np c::SLICE_LABEL_LENGTH, c.validators.map { |v| [v.class, v.ok?(\"Slice: x\"), v.ok?(\"x\")] }, c.constants.sort\n";
    let expected = native(&files, &format!("require \"monorepo_slices/slices_file_contract\"\n{script}"));
    assert_eq!(expected, "7\n[[MonorepoSlices::SlicesFileContract::IssueTrackingLabelValidator, true, false]]\n[:BaseValidator, :DEFAULT_VALIDATOR, :IssueTrackingLabelValidator, :MAX_GITHUB_LABEL_LENGTH, :SLICE_LABEL_LENGTH, :SLICE_LABEL_PREFIX]\n");
    let run = emit_and_run::empty_app().write(files[0].0, files[0].1).run_ruby(script);
    assert!(run.success, "{}\n{}", run.stdout, run.stderr);
    assert_eq!(run.stdout, expected, "{}", run.stderr);
}

/// `files` as the lib of a vendored gem `name` (a lockfile spec and a
/// `gem_sources` entry, as Core carries graphql-client), `entry` required
/// from config/application.rb; `script` must print what it prints natively.
fn gem_same_as_native(name: &str, files: &[(&str, &str)], entry: &str, script: &str, expected: &str) -> emit_and_run::Run {
    let lib: Vec<(String, &str)> = files.iter().map(|(path, text)| (format!("lib/{path}"), *text)).collect();
    let lib: Vec<(&str, &str)> = lib.iter().map(|(path, text)| (path.as_str(), *text)).collect();
    assert_eq!(native(&lib, &format!("require {entry:?}\n{script}")), expected, "native control");
    let mut overlay = emit_and_run::empty_app()
        .write("roundhouse.yml", &format!("gem_sources:\n  {name}: vendor/gems/{name}-1.0.0\n"))
        .write("Gemfile.lock", &format!("GEM\n  remote: https://rubygems.org/\n  specs:\n    {name} (1.0.0)\n\nDEPENDENCIES\n  {name}\n"))
        .write("config/application.rb", &format!("require {entry:?}\n"));
    for (path, text) in files {
        overlay = overlay.write(&format!("vendor/gems/{name}-1.0.0/lib/{path}"), text);
    }
    let run = overlay.run_ruby(script);
    assert!(run.success, "emitted tree failed in {}\n{}\n{}", run.emitted.display(), run.stdout, run.stderr);
    assert!(!run.stderr.contains("file_require_not_carried"), "{}", run.stderr);
    assert_eq!(run.stdout, expected, "{}", run.stderr);
    run
}

/// graphql-client 0.25.0: collocated_enforcement.rb reopens `Client`,
/// writes `WHITELISTED_GEM_NAMES` and then `module CollocatedEnforcement`;
/// client.rb requires it and then runs `extend CollocatedEnforcement` in
/// `Client`'s body. The module goes into Client's file below that
/// constant, and it was written after all of Client's constants, so after
/// client.rb's `extend`, which raised NameError at boot.
const COLLOCATED: &[(&str, &str)] = &[
    ("gqlc/error.rb", "module Gqlc\n  class Client\n    class Error < StandardError; end\n  end\nend\n"),
    (
        "gqlc/collocated.rb",
        "require \"gqlc/error\"\n\nmodule Gqlc\n  class Client\n    NAMES = %w{pry byebug}\n\n    class CallerError < Error; end\n\n    module Collocated\n      def collocated = [:collocated, NAMES.size]\n    end\n  end\nend\n",
    ),
    (
        "gqlc/client.rb",
        "require \"gqlc/collocated\"\n\nmodule Gqlc\n  class Client\n    class DynamicError < Error; end\n\n    extend Collocated\n\n    DOC = collocated\n  end\nend\n",
    ),
];

#[test]
fn a_nested_module_from_another_file_loads_before_the_body_that_extends_it() {
    gem_same_as_native(
        "gqlc",
        COLLOCATED,
        "gqlc/client",
        "c = Gqlc::Client\np c::DOC, c.collocated, c::CallerError.superclass, c::DynamicError.superclass, c.constants.sort\n",
        "[:collocated, 2]\n[:collocated, 2]\nGqlc::Client::Error\nGqlc::Client::Error\n[:CallerError, :Collocated, :DOC, :DynamicError, :Error, :NAMES]\n",
    );
}

/// graphql-client 0.25.0's operation_definition.rb:
/// `alias operation_name definition_name` in `OperationDefinition <
/// Definition` names the superclass's method. The ingest refused the
/// file, so graphql/client.rb's `require "graphql/client/operation_definition"`
/// named a file the tree did not carry (file_require_not_carried at boot).
const GQLC: &[(&str, &str)] = &[
    ("gqlc.rb", "require \"gqlc/operation_definition\"\n\nmodule Gqlc\n  def self.name_of(definition) = definition.operation_name\nend\n"),
    ("gqlc/definition.rb", "module Gqlc\n  class Definition\n    def initialize(name)\n      @name = name\n    end\n\n    def definition_name = @name\n\n    class << self\n      def build(name) = new(name)\n    end\n  end\nend\n"),
    (
        "gqlc/operation_definition.rb",
        "require \"gqlc/definition\"\n\nmodule Gqlc\n  class OperationDefinition < Definition\n    # Public: Alias for definition name.\n    alias operation_name definition_name\n\n    class << self\n      alias create build\n    end\n  end\nend\n",
    ),
];

#[test]
fn an_alias_of_an_inherited_method_is_carried() {
    gem_same_as_native(
        "gqlc",
        GQLC,
        "gqlc",
        "d = Gqlc::OperationDefinition.new(\"Q\")\np d.operation_name, Gqlc.name_of(d), Gqlc::OperationDefinition.create(\"R\").operation_name\np Gqlc::OperationDefinition.instance_method(:operation_name).original_name\n",
        "\"Q\"\n\"Q\"\n\"R\"\n:definition_name\n",
    );
}

/// activemerchant 1.137.0's billing/compatibility.rb: `class Errors <
/// Hash` does `alias count size`, naming a method of its core
/// superclass (`Hash#size`, `alias size length` in its RBS). Only app
/// ancestors were looked up, so the file was refused ("alias names a
/// method this body has not defined") and `Compatibility::Model` was
/// undefined where billing/model.rb includes it (NameError at boot).
/// Plain `Plainer` aliases Object's `inspect`, through no written
/// superclass.
const CORE_ALIAS: &[(&str, &str)] = &[
    ("amlike.rb", "require \"amlike/errors\"\n"),
    (
        "amlike/errors.rb",
        "module Amlike\n  class Errors < Hash\n    def initialize\n      super() { |h, k| h[k] = [] }\n    end\n\n    alias count size\n\n    def [](key)\n      super(key.to_s)\n    end\n  end\n\n  class Plainer\n    alias described inspect\n  end\nend\n",
    ),
];

#[test]
fn an_alias_of_a_core_superclass_method_is_carried() {
    gem_same_as_native(
        "amlike",
        CORE_ALIAS,
        "amlike",
        "e = Amlike::Errors.new\ne[:card] << \"bad\"\ne[\"name\"] << \"blank\"\np e.count, e[\"card\"], Amlike::Errors.instance_method(:count).original_name\np Amlike::Plainer.new.described.start_with?(\"#<Amlike::Plainer\")\n",
        "2\n[\"bad\"]\n:size\ntrue\n",
    );
}

/// graphql-client 0.25.0's query_typename.rb declares
/// `InsertTypenameVisitor` in `module QueryTypename` only in the branch
/// of `if GraphQL::Language::Nodes::AbstractNode.method_defined?(:merge)`
/// that finds graphql-ruby's node mutation API. The conditional is now
/// carried with the class inside its arm (tests/conditional_class_declarations.rs
/// runs both branches), so the file is no longer refused. A class
/// declared in a conditional inside `class << self` is still named as
/// the gap.
const QUERY_TYPENAME: &str = "module Gqlc\n  module QueryTypename\n    if ENV[\"GQLC_LEGACY\"].nil?\n      class InsertVisitor\n        def visit = :visitor\n      end\n\n      def self.insert = InsertVisitor.new.visit\n    else\n      def self.insert = :legacy\n    end\n\n    def self.flatten = 1\n  end\nend\n";

const SINGLETON_CONDITIONAL: &str = "module Gqlc\n  module Singleton\n    def self.x = 1\n\n    class << self\n      if ENV[\"GQLC_LEGACY\"].nil?\n        class Inner\n          def visit = :visitor\n        end\n      end\n    end\n  end\nend\n";

#[test]
fn a_class_declared_in_a_class_body_conditional_is_carried() {
    let files = [("lib/gqlc/query_typename.rb", QUERY_TYPENAME)];
    let script = "require \"gqlc/query_typename\"\np Gqlc::QueryTypename.insert, defined?(Gqlc::QueryTypename::InsertVisitor)\n";
    assert_eq!(native(&files, script), ":visitor\n\"constant\"\n");
    assert_eq!(native(&files, &format!("ENV[\"GQLC_LEGACY\"] = \"1\"\n{script}")), ":legacy\nnil\n");

    let text = check_gem_tree("carried", &files);
    assert!(!text.contains("query_typename.rb"), "{text}");

    let text = check_gem_tree("singleton", &[("lib/gqlc/singleton.rb", SINGLETON_CONDITIONAL)]);
    let row = text.lines().find(|l| l.contains("singleton.rb:7:9:")).unwrap_or_else(|| panic!("{text}"));
    assert!(
        row.contains("a class or module declared in a load-time conditional inside `class << self` is not modeled"),
        "{row}"
    );
}

/// `roundhouse check` on a tree vendoring the gem `gqlc` with `files`
/// (paths in the gem): its rows.
fn check_gem_tree(tag: &str, files: &[(&str, &str)]) -> String {
    let root = std::env::temp_dir().join(format!("rh_boot_emit_order_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let gem: Vec<(String, &str)> = files.iter().map(|(path, text)| (format!("vendor/gems/gqlc-1.0.0/{path}"), *text)).collect();
    for (path, text) in [
        ("app/models/application_record.rb", "class ApplicationRecord < ActiveRecord::Base\n  self.abstract_class = true\nend\n"),
        ("config/routes.rb", "Rails.application.routes.draw do\nend\n"),
        ("roundhouse.yml", "gem_sources:\n  gqlc: vendor/gems/gqlc-1.0.0\n"),
        ("Gemfile.lock", "GEM\n  remote: https://rubygems.org/\n  specs:\n    gqlc (1.0.0)\n\nDEPENDENCIES\n  gqlc\n"),
    ]
    .into_iter()
    .chain(gem.iter().map(|(path, text)| (path.as_str(), *text)))
    {
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_roundhouse")).arg("check").arg(&root).output().unwrap();
    std::fs::remove_dir_all(&root).unwrap();
    format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
}

const UNDEFINED_ALIAS: &[(&str, &str)] = &[
    ("lib/gqlc/base.rb", "module Gqlc\n  class Base\n    def name = \"base\"\n  end\nend\n"),
    ("lib/gqlc/node.rb", "require \"gqlc/base\"\n\nmodule Gqlc\n  class Node < Base\n    alias label gem_label\n  end\nend\n"),
];

/// An `alias` whose original no ancestor in the tree defines is not
/// captured: native Ruby raises loading the file, and the file is still
/// refused at the alias, as before.
#[test]
fn an_alias_no_ancestor_defines_still_refuses_its_file() {
    let script = "begin\n  require \"gqlc/node\"\nrescue NameError => e\n  p e.name\nend\n";
    assert_eq!(native(UNDEFINED_ALIAS, script), ":gem_label\n");
    let text = check_gem_tree("undefined_alias", UNDEFINED_ALIAS);
    let row = text.lines().find(|l| l.contains("node.rb:5:5:")).unwrap_or_else(|| panic!("{text}"));
    assert!(row.contains("alias names a method this body has not defined"), "{row}");
}

/// A gem file of declarations only, required by config/application.rb.
/// The boot list held file programs only, so a file with none loaded
/// nothing, and a plain gem's constants have no autoload: `Gqlc::Plain`
/// was undefined (NameError). Each class it declares now loads at that
/// boot require, in the file's order (`Shapes` before `Plain`, whose
/// body reads it).
const PLAIN_DECLARATIONS: &[(&str, &str)] = &[(
    "gqlc/plain.rb",
    "module Gqlc\n  module Shapes\n    ALL = %i[a b c].freeze\n  end\n\n  class Plain\n    SIZE = Shapes::ALL.size\n\n    def self.x = SIZE\n  end\nend\n",
)];

#[test]
fn a_gem_file_of_declarations_only_required_by_boot_is_loaded() {
    let run = gem_same_as_native(
        "gqlc",
        PLAIN_DECLARATIONS,
        "gqlc/plain",
        "p Gqlc::Plain.x, Gqlc::Shapes::ALL.last\n",
        "3\n:c\n",
    );
    let models = std::fs::read_to_string(run.emitted.join("app/models.rb")).unwrap();
    let shapes = models.find("  [\"models/gqlc/shapes\", nil], # config/application.rb:1\n");
    let plain = models.find("  [\"models/gqlc/plain\", nil], # config/application.rb:1\n");
    assert!(shapes.is_some() && plain.is_some() && shapes < plain, "{models}");
}

/// A class declared inside `class << self` of a gem module, once with no
/// method of the module's own and once beside a `def self.x`. Ruby puts
/// it on the singleton class, where the singleton's methods read it bare;
/// the ingest hoists it into the module (`util::hoisted_singleton_body`),
/// as for Core's ConnectionSerializer. Its file was emitted, but nothing
/// loaded it: a gem file of declarations only that boot requires
/// (above). Divergence kept: the hoisted `Singleton::Inner` also resolves
/// from outside, where native Ruby raises NameError, and
/// `singleton_class.constants` is empty.
const SINGLETON_ONLY: &[(&str, &str)] = &[(
    "gqlc/singleton.rb",
    "module Gqlc\n  module Singleton\n    class << self\n      class Inner\n        def visit = :visitor\n      end\n\n      def make = Inner.new.visit\n    end\n  end\nend\n",
)];

const SINGLETON_BESIDE_DEF: &[(&str, &str)] = &[(
    "gqlc/singleton.rb",
    "module Gqlc\n  module Singleton\n    def self.x = 1\n\n    class << self\n      class Inner\n        def visit = :visitor\n      end\n\n      def make = [Inner.new.visit, x]\n    end\n  end\nend\n",
)];

#[test]
fn a_class_declared_in_class_less_less_self_of_a_gem_module_is_loaded() {
    gem_same_as_native("gqlc", SINGLETON_ONLY, "gqlc/singleton", "p Gqlc::Singleton.make\n", ":visitor\n");
    gem_same_as_native("gqlc", SINGLETON_BESIDE_DEF, "gqlc/singleton", "p Gqlc::Singleton.make, Gqlc::Singleton.x\n", "[:visitor, 1]\n1\n");
}

//! A plain gem's files load in the order its requires give, and each
//! file's declarations are defined where the file runs.
//!
//! Three shapes from Core's r16-pre1 boot (emit at ec8afa59a), each
//! checked against CRuby on the same files with circular-require
//! warnings raising (Core's StrictWarning):
//!
//! - paquito 1.0.2: paquito.rb requires paquito/types, which requires
//!   paquito/errors, a file of declarations only that reopens
//!   `module Paquito`. Its require loaded each declaration, including
//!   `Paquito`'s own file, through paquito.rb's program: the in-flight
//!   root, a circular require.
//! - rollouts_mutex: whole_rollout_mutex/policy.rb defines
//!   `Declaration = Data.define(...)` and then `Policy::DEFAULT =
//!   Policy.new(...)`, which builds Declarations. DEFAULT is emitted in
//!   the namespace's file, and it loaded ahead of Declaration's.
//! - essentials' patches/money_ext.rb: `module MoneyOverride`, then
//!   `class Money; prepend MoneyOverride; end`, reopening the
//!   shopify-money gem's class. The prepend is written in the gem's
//!   class file, which did not load MoneyOverride's.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

/// Core's StrictWarning: a circular-require warning raises.
const STRICT: &str = "$VERBOSE = true\n\nmodule Strict\n  def warn(msg, category: nil, **)\n    raise \"strict: #{msg}\" if msg.include?(\"circular require\")\n\n    super\n  end\nend\nWarning.extend(Strict)\n";

/// Each gem: (name, its lib files).
type Gem<'a> = (&'a str, &'a [(&'a str, &'a str)]);

/// Run `script` natively (each gem's lib on the load path, `entry`
/// required) and in the emitted tree (the gems carried as gem sources,
/// `entry` required from config/application.rb); both must print
/// `expected`.
fn same_as_native(case: &str, gems: &[Gem], entry: &str, script: &str, expected: &str) -> emit_and_run::Run {
    same_as_native_from(case, gems, entry, script, expected, false, &[])
}

/// [`same_as_native`], the gems carried in the repository instead: each
/// under `gems/<name>`, a lockfile `PATH` source whose spec Core's
/// Gemfile names `in_repo-<name>` (`in_repo_gem("graphql-cardinal")`).
fn same_as_native_in_repo(case: &str, gems: &[Gem], entry: &str, script: &str, expected: &str) -> emit_and_run::Run {
    same_as_native_from(case, gems, entry, script, expected, true, &[])
}

/// `extra`: more app files for the emitted tree only (a gem RBI).
fn same_as_native_from(
    case: &str,
    gems: &[Gem],
    entry: &str,
    script: &str,
    expected: &str,
    in_repo: bool,
    extra: &[(&str, &str)],
) -> emit_and_run::Run {
    let root = std::env::temp_dir().join(format!("roundhouse-plain-gem-load-order-{case}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let mut native = emit_and_run::ruby();
    native.arg("-W");
    for (name, files) in gems {
        for (path, text) in *files {
            let file = root.join(format!("{name}/lib/{path}"));
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, text).unwrap();
        }
        native.arg("-I").arg(root.join(format!("{name}/lib")));
    }
    std::fs::write(root.join("strict.rb"), STRICT).unwrap();
    let native = native.arg("-r").arg(root.join("strict.rb")).arg("-r").arg(entry).args(["-e", script]).output().expect("CRuby control");
    let _ = std::fs::remove_dir_all(&root);
    assert!(native.status.success(), "native control failed:\n{}", String::from_utf8_lossy(&native.stderr));
    assert_eq!(String::from_utf8_lossy(&native.stdout), expected, "native control");

    let mut overlay = if in_repo {
        let paths: String = gems
            .iter()
            .map(|(name, _)| format!("PATH\n  remote: gems/{name}\n  specs:\n    in_repo-{name} (1.0.0)\n\n"))
            .collect();
        let deps: String = gems.iter().map(|(name, _)| format!("  in_repo-{name}!\n")).collect();
        emit_and_run::empty_app()
            .write("Gemfile.lock", &format!("{paths}GEM\n  remote: https://rubygems.org/\n  specs:\n\nDEPENDENCIES\n{deps}"))
    } else {
        let specs: String = gems.iter().map(|(name, _)| format!("    {name} (1.0.0)\n")).collect();
        let deps: String = gems.iter().map(|(name, _)| format!("  {name}\n")).collect();
        let sources: String = gems.iter().map(|(name, _)| format!("  {name}: vendor/gems/{name}-1.0.0\n")).collect();
        emit_and_run::empty_app()
            .write("roundhouse.yml", &format!("gem_sources:\n{sources}"))
            .write("Gemfile.lock", &format!("GEM\n  remote: https://rubygems.org/\n  specs:\n{specs}\nDEPENDENCIES\n{deps}"))
    };
    overlay = overlay
        .write("config/boot.rb", "require_relative \"../lib/strict\"\n")
        .write("config/application.rb", &format!("require_relative \"boot\"\nrequire {entry:?}\n"))
        .write("lib/strict.rb", STRICT);
    for (path, text) in extra {
        overlay = overlay.write(path, text);
    }
    for (name, files) in gems {
        for (path, text) in *files {
            let at = if in_repo { format!("gems/{name}/lib/{path}") } else { format!("vendor/gems/{name}-1.0.0/lib/{path}") };
            overlay = overlay.write(&at, text);
        }
    }
    let run = overlay.run_ruby(script);
    assert!(
        run.success,
        "emitted tree failed in {}\n=== stdout ===\n{}\n=== stderr ===\n{}",
        run.emitted.display(),
        run.stdout,
        run.stderr
    );
    assert!(!run.stderr.contains("circular require"), "{}", run.stderr);
    assert_eq!(run.stdout, expected, "{}", run.stderr);
    run
}

#[test]
fn a_declarations_file_reopening_the_gem_root_does_not_require_the_root() {
    const PQ: &[(&str, &str)] = &[
        ("pq.rb", "require \"pq/types\"\n\nmodule Pq\n  DEFAULT = 1\n\n  def self.default\n    DEFAULT\n  end\nend\n"),
        (
            "pq/types.rb",
            "require \"pq/errors\"\n\nmodule Pq\n  module Types\n    def self.kinds\n      [Error.name, PackError.name, UnpackError.name]\n    end\n  end\nend\n",
        ),
        (
            "pq/errors.rb",
            "module Pq\n  Error = Class.new(StandardError)\n\n  class PackError < Error\n    def initialize(msg)\n      super(\"pack: #{msg}\")\n    end\n  end\n\n  UnpackError = Class.new(Error)\nend\n",
        ),
    ];
    let _ = same_as_native(
        "root",
        &[("pq", PQ)],
        "pq",
        "puts Pq.default\nputs Pq::Types.kinds.inspect\nputs Pq::PackError.new(\"x\").message\nputs Pq::UnpackError.superclass\n",
        "1\n[\"Pq::Error\", \"Pq::PackError\", \"Pq::UnpackError\"]\npack: x\nPq::Error\n",
    );
}

#[test]
fn a_files_split_declarations_load_in_its_order() {
    const RM: &[(&str, &str)] = &[
        ("rm.rb", "require_relative \"rm/version\"\nrequire_relative \"rm/mutex\"\n"),
        ("rm/version.rb", "module Rm\n  module Mutex\n    VERSION = \"0.1\"\n  end\nend\n"),
        (
            "rm/mutex.rb",
            "require_relative \"mutex/policy\"\n\nmodule Rm\n  module Mutex\n    SPACE = 10\n\n    class Input\n      def initialize(policy: Policy.default)\n        @policy = policy\n      end\n\n      def families\n        @policy.declarations.map(&:family)\n      end\n    end\n  end\nend\n",
        ),
        (
            "rm/mutex/policy.rb",
            r#"module Rm
  module Mutex
    default_resources = { "theme" => [:shared], "checkout" => [:exclusive] }

    Declaration = Data.define(:family, :mode) do
      def initialize(family:, mode:)
        raise ArgumentError, "bad mode" unless [:shared, :exclusive].include?(mode)

        super(family: family.dup.freeze, mode: mode)
      end
    end

    class Policy
      attr_reader :declarations

      def self.default
        DEFAULT
      end

      def initialize(resources:)
        @declarations = resources.flat_map do |family, modes|
          modes.map { |mode| Declaration.new(family: family, mode: mode) }
        end
      end
    end

    Policy::DEFAULT = Policy.new(resources: default_resources)
  end
end
"#,
        ),
    ];
    let _ = same_as_native(
        "split",
        &[("rm", RM)],
        "rm",
        "p Rm::Mutex::Input.new.families\nputs Rm::Mutex::VERSION\nputs Rm::Mutex::SPACE\np Rm::Mutex::Policy::DEFAULT.declarations.last.mode\n",
        "[\"theme\", \"checkout\"]\n0.1\n10\n:exclusive\n",
    );
}

#[test]
fn a_patch_files_prepend_on_another_gems_class_finds_its_module() {
    const COIN: &[(&str, &str)] =
        &[("coin.rb", "class Coin\n  def initialize(value)\n    @value = value\n  end\n\n  def value\n    @value\n  end\nend\n")];
    const ESS: &[(&str, &str)] = &[
        ("ess.rb", "require \"coin\"\nrequire_relative \"patches/coin_ext\"\n"),
        (
            "patches/coin_ext.rb",
            "module CoinOverride\n  def value\n    super * 2\n  end\nend\n\nmodule CoinFromCents\n  def from_cents(cents)\n    new(cents / 100.0)\n  end\nend\n\nclass Coin\n  extend CoinFromCents\n  prepend CoinOverride\nend\n",
        ),
    ];
    let _ = same_as_native(
        "prepend",
        &[("coin", COIN), ("ess", ESS)],
        "ess",
        "puts Coin.new(3).value\nputs Coin.from_cents(250).value\np Coin.ancestors.first(2)\n",
        "6\n5.0\n[CoinOverride, Coin]\n",
    );
}

/// verdict 19.34.0: verdict.rb defines `module Verdict`, then requires
/// verdict/decide, which requires verdict/instrumentation, whose program
/// runs `Verdict::Instrumentation::Subscriber.subscribe_all`. That
/// statement's load-time need of `Verdict` was required through
/// verdict.rb's program, which is the program loading: a circular
/// require.
#[test]
fn a_statement_needing_the_loading_gem_root_does_not_require_it_again() {
    const VD: &[(&str, &str)] = &[
        ("vd.rb", "module Vd\n  def self.ready\n    :yes\n  end\nend\n\nrequire \"vd/decide\"\n"),
        (
            "vd/decide.rb",
            "require \"vd/instrumentation\"\n\nmodule Vd\n  module Decide\n    def self.subscribed\n      Instrumentation::Subscriber.subscribed\n    end\n  end\nend\n",
        ),
        (
            "vd/instrumentation.rb",
            "require_relative \"instrumentation/subscriber\"\n\nVd::Instrumentation::Subscriber.subscribe_all\n",
        ),
        (
            "vd/instrumentation/subscriber.rb",
            "module Vd\n  module Instrumentation\n    class Subscriber\n      def self.subscribe_all\n        @subscribed = Vd.ready\n      end\n\n      def self.subscribed\n        @subscribed\n      end\n    end\n  end\nend\n",
        ),
    ];
    let _ = same_as_native("inflight", &[("vd", VD)], "vd", "p Vd::Decide.subscribed\n", ":yes\n");
}

/// verdict 19.34.0 sets up its own Zeitwerk loader for its `lib/`
/// (verdict/zeitwerk_loader.rb's `Zeitwerk::Loader.new` and `push_dir`,
/// in a memoized class method; `Verdict.loader.setup` at the end of
/// verdict.rb). Its other files are autoloaded: subject_type/shop.rb
/// reads `PiiLevel` at load, which no require loads.
#[test]
fn a_gem_that_sets_up_its_own_zeitwerk_loader_keeps_autoloading_its_files() {
    const ZW: &[(&str, &str)] = &[
        (
            "zw.rb",
            "require_relative \"zw/zeitwerk_loader\"\n\nmodule Zw\n  extend self\n\n  def loader\n    @loader ||= ZeitwerkLoader.loader\n  end\nend\n\nZw.loader.setup\nrequire \"zw/decide\"\n",
        ),
        (
            "zw/zeitwerk_loader.rb",
            r##"require "zeitwerk"

module Zw
  class ZeitwerkLoader
    class << self
      def loader
        @loader ||= configure_loader
      end

      private

      def configure_loader
        loader = Zeitwerk::Loader.new
        lib_path = File.expand_path("..", __dir__)
        loader.push_dir(lib_path)
        loader.inflector.inflect("pii_level" => "PiiLevel")
        loader.ignore(__FILE__)
        loader.ignore("#{lib_path}/zw.rb")
        loader
      end
    end
  end
end
"##,
        ),
        ("zw/decide.rb", "module Zw\n  module Decide\n    def self.levels\n      [SubjectType::Shop::LEVEL, Flag.new.configs.size]\n    end\n  end\nend\n"),
        ("zw/pii_level.rb", "module Zw\n  module PiiLevel\n    HIGH = 3\n  end\nend\n"),
        ("zw/subject_type/shop.rb", "module Zw\n  module SubjectType\n    class Shop\n      LEVEL = PiiLevel::HIGH\n    end\n  end\nend\n"),
        ("zw/flag.rb", "module Zw\n  class Flag\n    def configs\n      RecordConfigurations.all\n    end\n  end\nend\n"),
        ("zw/record_configurations.rb", "module Zw\n  class RecordConfigurations\n    def self.all\n      [1, 2]\n    end\n  end\nend\n"),
    ];
    let _ = same_as_native("zeitwerk", &[("zw", ZW)], "zw", "p Zw::Decide.levels\n", "[3, 2]\n");
}

/// graphql-client 0.25.0's client.rb requires
/// graphql/client/query_typename.rb, whose module ingest leaves out (a
/// load-time conditional holding more than `def`s). The require was a
/// plain `require` at boot, which failed with a LoadError (the emitted
/// tree does not load a carried gem's files from the gem home). Now
/// check reports it at the require, naming the left-out file and why,
/// and the boot names it instead of raising; the rest of the file runs.
#[test]
fn a_require_of_a_file_ingest_left_out_is_reported_at_the_require() {
    const GQ: &[(&str, &str)] = &[
        (
            "gq.rb",
            "require \"gq/typename\"\nrequire \"gq/response\"\n\nmodule Gq\n  def self.ok\n    Response.new.ok\n  end\nend\n",
        ),
        (
            "gq/typename.rb",
            "module Gq\n  module Typename\n    if \"\".respond_to?(:upcase)\n      VISITOR = :upcase\n\n      def self.kind\n        VISITOR\n      end\n    end\n  end\nend\n",
        ),
        ("gq/response.rb", "module Gq\n  class Response\n    def ok\n      :ok\n    end\n  end\nend\n"),
    ];
    let run = same_as_native("left-out", &[("gq", GQ)], "gq", "p Gq.ok\n", ":ok\n");
    let reported: Vec<&String> = run.errors.iter().filter(|e| e.contains("require \\\"gq/typename\\\"") || e.contains("require \"gq/typename\"")).collect();
    assert_eq!(reported.len(), 1, "{:#?}", run.errors);
    assert!(
        reported[0].contains("vendor/gems/gq-1.0.0/lib/gq/typename.rb, whose classes, modules and constants ingest left out"),
        "{}",
        reported[0]
    );
    assert!(run.stderr.contains("error[file_require_not_carried]"), "{}", run.stderr);
    assert!(!run.stderr.contains("LoadError"), "{}", run.stderr);
}

/// kafka-shopify 5.2.3's environment.rb requires kafka-shopify/writers,
/// a file of `autoload` calls in `module Writers`. The tree registers
/// those autoloads and has no file for it; the require stayed a plain
/// `require` and failed with a LoadError. Natively the require defines
/// `KafkaShopify::Writers`, under which the autoloads hang.
#[test]
fn a_require_of_a_file_of_autoloads_opens_its_modules() {
    const KS: &[(&str, &str)] = &[
        (
            "ks.rb",
            "require_relative \"ks/writers\"\nrequire \"ks/errors\"\n\nmodule Ks\n  def self.kinds\n    [Writers::LogWriter.new.kind, Errors::ConfigError.name]\n  end\nend\n",
        ),
        ("ks/writers.rb", "module Ks\n  module Writers\n    autoload(:LogWriter, \"ks/writers/log_writer\")\n  end\nend\n"),
        ("ks/writers/log_writer.rb", "module Ks\n  module Writers\n    class LogWriter\n      def kind\n        :log\n      end\n    end\n  end\nend\n"),
        ("ks/errors.rb", "module Ks\n  module Errors\n    autoload :ConfigError, \"ks/errors/config_error\"\n  end\nend\n"),
        ("ks/errors/config_error.rb", "module Ks\n  module Errors\n    class ConfigError < StandardError\n    end\n  end\nend\n"),
    ];
    let run = same_as_native("autoloads", &[("ks", KS)], "ks", "p Ks.kinds\n", "[:log, \"Ks::Errors::ConfigError\"]\n");
    assert!(!run.stderr.contains("LoadError") && !run.stderr.contains("file_require_not_carried"), "{}", run.stderr);
}

/// graphql-cardinal 1.5.9, an in-repo gem (`gems/graphql-cardinal`,
/// locked as `in_repo-graphql-cardinal`): cardinal.rb opens `class
/// Executor; class ExecutionField; end; end` and later requires
/// cardinal/executor.rb, whose first line requires executor/hot_paths.rb,
/// which reopens `class Executor` to define `HotPaths`. The gem was not
/// read as a plain gem (its directory is not its spec's name), so
/// `Executor` autoloaded executor.rb's program; the merged declarations,
/// loaded from cardinal.rb's program, required hot_paths.rb, whose
/// `class Executor` ran that autoload back into hot_paths.rb (a circular
/// require).
#[test]
fn an_in_repo_gems_files_load_where_its_requires_give() {
    const CD: &[(&str, &str)] = &[
        (
            "cd.rb",
            "module Cd\n  class Executor\n    class Field; end\n  end\nend\n\nrequire_relative \"cd/executor\"\n",
        ),
        (
            "cd/executor.rb",
            "require_relative \"executor/hot_paths\"\n\nmodule Cd\n  class Executor\n    include HotPaths\n\n    def run\n      [hot, Field.name]\n    end\n  end\nend\n",
        ),
        ("cd/executor/hot_paths.rb", "module Cd\n  class Executor\n    module HotPaths\n      def hot\n        :hot\n      end\n    end\n  end\nend\n"),
    ];
    let _ = same_as_native_in_repo("in-repo", &[("cd", CD)], "cd", "p Cd::Executor.new.run\n", "[:hot, \"Cd::Executor::Field\"]\n");
}

/// kafka-shopify 5.2.3's types/google/protobuf/abstract_message_public.rb
/// reopens google-protobuf's `module Google; module Protobuf` and writes
/// `AbstractMessagePublic = AbstractMessage`: google-protobuf 3.25.8's
/// message_exts.rb declares `AbstractMessage` and makes it
/// `private_constant`. Lexical lookup reaches a private constant, a
/// scoped path does not. The read resolved to the gem's declaration and
/// was emitted `Google::Protobuf::AbstractMessage`, which raised
/// NameError (private constant referenced) at boot. Ruby's timeout gem
/// has the same shape: `Timeout::State` is private.
#[test]
fn a_bare_read_of_a_gems_private_constant_stays_lexical() {
    const TP: &[(&str, &str)] = &[
        ("tp.rb", "require \"tp/timeout_state\"\n\nmodule Tp\n  def self.state\n    Timeout::StatePublic.name\n  end\nend\n"),
        ("tp/timeout_state.rb", "require \"timeout\"\n\nmodule Timeout\n  StatePublic = State\nend\n"),
    ];
    // The installed timeout gem, as the gem home the tree names has it
    // (only what the read needs: the declaration and its privacy).
    let ruby = emit_and_run::ruby().args(["-e", "print Gem::Specification.find_by_name('timeout').version"]).output().expect("ruby");
    let version = String::from_utf8_lossy(&ruby.stdout).to_string();
    assert!(!version.is_empty(), "the timeout gem is installed");
    let home = std::env::temp_dir().join(format!("roundhouse-plain-gem-home-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join(format!("gems/timeout-{version}/lib"))).unwrap();
    std::fs::create_dir_all(home.join("specifications")).unwrap();
    std::fs::write(
        home.join(format!("gems/timeout-{version}/lib/timeout.rb")),
        "module Timeout\n  class State\n  end\n  private_constant :State\nend\n",
    )
    .unwrap();
    std::fs::write(
        home.join(format!("specifications/timeout-{version}.gemspec")),
        format!("Gem::Specification.new do |s|\n  s.name = \"timeout\".freeze\n  s.version = \"{version}\".freeze\n  s.require_paths = [\"lib\".freeze]\nend\n"),
    )
    .unwrap();
    let config = format!("gem_home: {}\ngem_sources:\n  tp: vendor/gems/tp-1.0.0\n", home.display());
    let lock = format!(
        "GEM\n  remote: https://rubygems.org/\n  specs:\n    timeout ({version})\n    tp (1.0.0)\n      timeout\n\nDEPENDENCIES\n  timeout\n  tp\n"
    );
    let extra = [("roundhouse.yml", config.as_str()), ("Gemfile.lock", lock.as_str())];
    let _ = same_as_native_from("lexical", &[("tp", TP)], "tp", "p Tp.state\n", "\"Timeout::State\"\n", false, &extra);
    let _ = std::fs::remove_dir_all(&home);
}

/// shopify_spanner 4.9.0's catalog/store.rb reads `File.read(File.join
/// (__dir__, "hydrate_search_query.sql"))` in its class body, and
/// shopify-i18n 5.6.0's locales.rb `YAML.safe_load_file(File.expand_path
/// (File.join(__dir__, "../../data/cldr/likely_subtags.yml")))`: a gem
/// reads its own data beside its code. The emitted `__dir__` names the
/// gem's directory in the source tree, where only its Ruby was carried,
/// so each read was an Errno::ENOENT at boot.
#[test]
fn a_gem_source_reads_its_own_data_files() {
    const SP: &[(&str, &str)] = &[
        (
            "sp.rb",
            "require \"yaml\"\n\nmodule Sp\n  QUERY = File.read(File.join(__dir__, \"sp/query.sql\")).freeze\n  SUBTAGS = YAML.safe_load_file(File.expand_path(File.join(__dir__, \"../data/subtags.yml\")))[\"subtags\"]\nend\n",
        ),
        ("sp/query.sql", "SELECT 1\n"),
        ("../data/subtags.yml", "subtags:\n  en: en-Latn-US\n"),
    ];
    let _ = same_as_native("data", &[("sp", SP)], "sp", "p [Sp::QUERY, Sp::SUBTAGS]\n", "[\"SELECT 1\\n\", {\"en\" => \"en-Latn-US\"}]\n");
}

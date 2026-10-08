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
fn same_as_native(case: &str, gems: &[Gem], entry: &str, script: &str, expected: &str) {
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

    let specs: String = gems.iter().map(|(name, _)| format!("    {name} (1.0.0)\n")).collect();
    let deps: String = gems.iter().map(|(name, _)| format!("  {name}\n")).collect();
    let sources: String = gems.iter().map(|(name, _)| format!("  {name}: vendor/gems/{name}-1.0.0\n")).collect();
    let mut overlay = emit_and_run::empty_app()
        .write("roundhouse.yml", &format!("gem_sources:\n{sources}"))
        .write("Gemfile.lock", &format!("GEM\n  remote: https://rubygems.org/\n  specs:\n{specs}\nDEPENDENCIES\n{deps}"))
        .write("config/boot.rb", "require_relative \"../lib/strict\"\n")
        .write("config/application.rb", &format!("require_relative \"boot\"\nrequire {entry:?}\n"))
        .write("lib/strict.rb", STRICT);
    for (name, files) in gems {
        for (path, text) in *files {
            overlay = overlay.write(&format!("vendor/gems/{name}-1.0.0/lib/{path}"), text);
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
    same_as_native(
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
    same_as_native(
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
    same_as_native(
        "prepend",
        &[("coin", COIN), ("ess", ESS)],
        "ess",
        "puts Coin.new(3).value\nputs Coin.from_cents(250).value\np Coin.ancestors.first(2)\n",
        "6\n5.0\n[CoinOverride, Coin]\n",
    );
}

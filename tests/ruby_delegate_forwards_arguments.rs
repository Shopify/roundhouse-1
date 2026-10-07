//! An instance-side `delegate` forwards every argument on the Ruby target.
//!
//! asset_cloud 2.7.3's `AssetExtension` declares `delegate :add_error, to:
//! :asset` and never calls `add_error` itself; its includers do, with a
//! message. Ingest writes the IR forwarder zero-arg (argument forwarding
//! is the shape the strict targets do not lower), and the Ruby emit wrote
//! that one too: `def add_error; asset.add_error; end`, an ArgumentError
//! where Rails' generated `def add_error(...)` forwards. The Ruby emit now
//! writes the `(...)` forwarder, for `allow_nil:` and `prefix: true` too.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const ASSET: &str = r#"module Kit
  class Asset
    def errors
      @errors ||= []
    end

    def add_error(msg, *rest, level: :error, &blk)
      errors << [msg, rest, level, blk ? blk.call : nil]
      errors.size
    end

    def key
      "k"
    end
  end
end
"#;

const EXTENSION: &str = r#"module Kit
  class Extension
    attr_reader :asset, :spare

    delegate :add_error, to: :asset
    delegate :key, to: :spare, allow_nil: true
    delegate :errors, to: :asset, prefix: true

    def initialize(asset, spare = nil)
      @asset = asset
      @spare = spare
    end
  end
end
"#;

const SCRIPT: &str = r#"a = Kit::Asset.new
e = Kit::Extension.new(a)
p e.add_error("bad")
p e.add_error("worse", 1, 2, level: :warn) { "blk" }
p [e.key, Kit::Extension.new(a, a).key, e.asset_errors]
p Kit::Extension.instance_method(:add_error).arity
"#;

#[test]
fn a_delegated_instance_method_forwards_its_arguments_as_rails_does() {
    let native = emit_and_run::ruby()
        .arg("-e")
        .arg(format!(
            "require \"active_support/core_ext/module/delegation\"\n{ASSET}\n{EXTENSION}\n{SCRIPT}"
        ))
        .output()
        .expect("native Ruby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    let expected = "1\n2\n[nil, \"k\", [[\"bad\", [], :error, nil], [\"worse\", [1, 2], :warn, \"blk\"]]]\n-1\n";
    assert_eq!(String::from_utf8_lossy(&native.stdout), expected);
    let run = emit_and_run::empty_app()
        .write("app/models/kit/asset.rb", ASSET)
        .write("app/models/kit/extension.rb", EXTENSION)
        .run_ruby(SCRIPT);
    run.assert_passes();
    assert_eq!(run.stdout, expected);
}

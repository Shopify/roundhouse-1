//! asset_cloud 2.7.3's lib/asset_cloud.rb, after its requires:
//! `AssetCloud::Base.class_eval do include …; include …; callback_methods
//! :write, :delete end`, a gem class reopened with mixins and a class
//! macro one of them provides; then `AssetCloud::Asset.class_eval`,
//! which adds methods (one calling `super` into a mixin), with
//! `protected` and `private` sections.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

fn app() -> emit_and_run::Overlay {
    emit_and_run::empty_app()
        .write("lib/cloud/asset.rb", "module Cloud\n  class Asset\n    def initialize(key)\n      @key = key\n    end\n\n    attr_reader :key\n  end\nend\n")
        .write("lib/cloud/base.rb", "module Cloud\n  class Base\n    def write(key)\n      \"wrote #{key}\"\n    end\n  end\nend\n")
        .write("lib/cloud/free_key_locator.rb", "module Cloud\n  module FreeKeyLocator\n    def free_key\n      \"k1\"\n    end\n  end\nend\n")
        .write(
            "lib/cloud/callbacks.rb",
            "module Cloud\n  module Callbacks\n    def self.included(base)\n      base.extend(ClassMethods)\n    end\n\n    module ClassMethods\n      def callback_methods(*names)\n        @callback_names = names\n      end\n\n      def callback_names\n        @callback_names\n      end\n    end\n  end\nend\n",
        )
        .write(
            "lib/cloud.rb",
            "require \"cloud/base\"\nrequire \"cloud/free_key_locator\"\nrequire \"cloud/callbacks\"\nrequire \"cloud/asset\"\n\nCloud::Base.class_eval do\n  include Cloud::FreeKeyLocator\n  include Cloud::Callbacks\n  callback_methods :write, :delete\nend\n\nCloud::Asset.class_eval do\n  include Cloud::FreeKeyLocator\n  include Cloud::Callbacks\n  callback_methods :store\n\n  def free_key\n    \"#{super}-#{key}\"\n  end\n\n  protected\n\n  def valid_key_path?\n    key.start_with?(\"a\")\n  end\n\n  private\n\n  def valid_key\n    valid_key_path?\n  end\nend\n",
        )
}

#[test]
fn a_class_eval_of_includes_and_a_mixin_macro_runs() {
    let run = app().run_ruby(
        "p Cloud::Base.new.free_key\np Cloud::Base.callback_names\np Cloud::Base.ancestors.take(3)\n\
         a = Cloud::Asset.new(\"ab\")\np a.free_key\np Cloud::Asset.callback_names\np a.send(:valid_key)\n\
         p [Cloud::Asset.protected_method_defined?(:valid_key_path?), Cloud::Asset.private_method_defined?(:valid_key)]\n",
    );
    assert!(run.success, "{}\n{}", run.stdout, run.stderr);
    assert_eq!(run.stdout, "\"k1\"\n[:write, :delete]\n[Cloud::Base, Cloud::Callbacks, Cloud::FreeKeyLocator]\n\"k1-ab\"\n[:store]\ntrue\n[true, true]\n", "{}", run.stderr);
    assert!(!run.stderr.contains("not carried"), "{}", run.stderr);
}

/// Core's asset_cloud: `AssetCloud::Callbacks` is an
/// ActiveSupport::Concern whose `module ClassMethods` defines
/// `callback_methods`; no authored `included` hook extends the receiver.
/// Concern's `append_features` does, when the class_eval's `include`
/// runs, so the macro after it answers. The concern's `extend` is not
/// written into the emitted module (the includer extends the carrier),
/// and the class_eval body extended nothing: `callback_methods` raised
/// NoMethodError for class AssetCloud::Base at boot.
#[test]
fn a_class_eval_include_of_a_concern_extends_its_class_methods() {
    let run = emit_and_run::empty_app()
        .write("lib/cloud/base.rb", "module Cloud\n  class Base\n    def write(key)\n      \"wrote #{key}\"\n    end\n  end\nend\n")
        .write(
            "lib/cloud/callbacks.rb",
            "module Cloud\n  module Callbacks\n    extend ActiveSupport::Concern\n\n    module ClassMethods\n      def callback_methods(*names)\n        @callback_names = names\n      end\n\n      def callback_names\n        @callback_names\n      end\n    end\n\n    def callbacks_for(symbol)\n      self.class.callback_names.include?(symbol)\n    end\n  end\nend\n",
        )
        .write(
            "lib/cloud.rb",
            "require \"active_support\"\nrequire \"cloud/base\"\nrequire \"cloud/callbacks\"\n\nCloud::Base.class_eval do\n  include Cloud::Callbacks\n  callback_methods :write, :delete\nend\n",
        )
        .run_ruby("p Cloud::Base.callback_names\np Cloud::Base.new.callbacks_for(:write)\np Cloud::Base.singleton_class.include?(Cloud::Callbacks::ClassMethods)\n");
    assert!(run.success, "{}\n{}", run.stdout, run.stderr);
    assert_eq!(run.stdout, "[:write, :delete]\ntrue\ntrue\n", "{}", run.stderr);
    assert!(!run.stderr.contains("not carried"), "{}", run.stderr);
}

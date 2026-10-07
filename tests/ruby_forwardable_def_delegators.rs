//! Forwardable's `def_delegators` / `def_delegator` define real methods.
//!
//! Core's `Apps::Models::UiExtension::Schema::ValidationDelegation` is a
//! module whose whole body is `extend Forwardable; def_delegators
//! :@validation, :group_id, …`. With no `def`, ingest took it for a
//! namespace, so every group class that includes it reported `includes
//! unresolved Schema::ValidationDelegation`. In a class, the declaration
//! was a class-body call nothing modelled: the emit dropped it, and every
//! forwarded name was a NoMethodError at run time.
//!
//! Ruby's Forwardable defines `def m(*args, &block) =
//! accessor.m(*args, &block)` (`ruby2_keywords`); `def_delegator`'s third
//! argument names the method it defines. The emitted tree runs the same
//! script as native CRuby with `require "forwardable"`.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const VALIDATION: &str = r##"module Kit
  class Validation
    def group_id
      :pos
    end

    def validate(x, *rest, scale: 1, &blk)
      [x * scale, rest, blk ? blk.call : nil]
    end

    def [](key)
      "at #{key}"
    end
  end
end
"##;

const DELEGATION: &str = r##"require "forwardable"

module Kit
  module Delegation
    extend Forwardable

    def_delegators :@validation, :group_id, :validate, :[]
    def_delegator :@validation, :group_id, :gid
  end
end
"##;

const GROUP: &str = r##"module Kit
  class Group
    include Kit::Delegation

    def initialize
      @validation = Validation.new
    end

    def describe
      "#{group_id}:#{gid}"
    end
  end
end
"##;

const PRESENTER: &str = r##"module Kit
  class Presenter
    extend Forwardable

    def_delegators :validation, :group_id, :validate
    def_delegator(:@validation, :group_id, :code)

    attr_reader :validation

    def initialize(validation)
      @validation = validation
    end
  end
end
"##;

const SCRIPT: &str = r##"g = Kit::Group.new
p g.describe
p g.validate(2, 3, scale: 10) { :blk }
p g[:a]
p Kit::Group.instance_method(:validate).arity
p Kit::Group.ancestors.include?(Kit::Delegation)
pr = Kit::Presenter.new(Kit::Validation.new)
p [pr.group_id, pr.code, pr.validate(4)]
p Kit::Presenter.instance_method(:group_id).arity
"##;

const EXPECTED: &str = "\"pos:pos\"\n[20, [3], :blk]\n\"at a\"\n-1\ntrue\n[:pos, :pos, [4, [], nil]]\n-1\n";

#[test]
fn forwardable_delegators_in_a_module_and_a_class_forward_as_ruby_does() {
    let native = emit_and_run::ruby()
        .arg("-e")
        .arg(format!("{VALIDATION}\n{DELEGATION}\n{GROUP}\n{PRESENTER}\n{SCRIPT}"))
        .output()
        .expect("native Ruby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    assert_eq!(String::from_utf8_lossy(&native.stdout), EXPECTED);
    let run = emit_and_run::empty_app()
        .write("app/models/kit/validation.rb", VALIDATION)
        .write("app/models/kit/delegation.rb", DELEGATION)
        .write("app/models/kit/group.rb", GROUP)
        .write("app/models/kit/presenter.rb", PRESENTER)
        .run_ruby(SCRIPT);
    run.assert_passes();
    assert_eq!(run.stdout, EXPECTED);
}

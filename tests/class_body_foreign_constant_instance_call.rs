//! A class body's call on a constant ANOTHER module holds, an instance of
//! an app class (`Podding::Redis::ClientFactory = ClientsByType.new`), runs
//! as Ruby runs it when the body loads. Core's `LeakyThrottle` and `Cart`
//! bodies call `Podding::Redis::ClientFactory.register(owner: self,
//! accessor: :redis) { … }`, which binds the `redis` accessor; the Ruby
//! family dropped the call with a `lower_residue` warning, so `redis` was
//! nil and `Resiliency.determine_resource(LeakyThrottle)` raised
//! `undefined method 'semian_resource' for class LeakyThrottle`
//! (e2e v2-r37).

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const CLIENTS: &str = r##"module Pod
  class Clients
    #: () -> void
    def initialize
      @bound = []
    end

    #: (owner: untyped, ?accessor: Symbol) ?{ () -> untyped } -> void
    def register(owner:, accessor: :store, &builder)
      @bound << owner
      value = builder ? builder.call : "default"
      owner.singleton_class.send(:define_method, accessor) { value }
    end

    #: () -> Array[untyped]
    def bound = @bound
  end
end
"##;

const FACTORY: &str = r##"module Pod
  Factory = Clients.new
end
"##;

const THROTTLE: &str = r##"class Throttle
  Pod::Factory.register(owner: self, accessor: :redis) { "proxy" }
end
"##;

#[test]
fn a_call_on_a_constant_another_module_holds_runs_when_the_body_loads() {
    let run = emit_and_run::empty_app()
        .write("lib/pod/clients.rb", CLIENTS)
        .write("lib/pod/factory.rb", FACTORY)
        .write("lib/throttle.rb", THROTTLE)
        .run_ruby("puts Throttle.redis\nputs Pod::Factory.bound.inspect\n");
    assert!(run.success, "emitted tree failed in {}: {}\nerrors: {:#?}", run.emitted.display(), run.stderr, run.errors);
    assert_eq!(run.stdout.lines().collect::<Vec<_>>(), ["proxy", "[Throttle]"], "{}", run.stderr);
}

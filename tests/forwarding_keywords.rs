#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::process::Command;

fn native_result(source: &str, script: &str, expected: &str) {
    let output = Command::new("ruby")
        .args(["-e", &format!("{source}; {script}")])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), expected);
}

#[test]
fn compiled_keyword_producers_preserve_identity_order_and_single_evaluation() {
    let source = r#"
class Counter
  def initialize; @calls = 0; end
  def produce(factor); @calls += 1; {factor: factor, offset: @calls}; end
  def calls; @calls; end
end
class Probe
  def self.target(a,b,factor:,offset: 0,**,&block)
    result = (a-b)*factor+offset
    block ? block.call(result) : result
  end
  def self.call(...); target(...); end
  def self.run
    kw = {factor: 3}
    Probe.call(11,4,**kw)
  end
  def self.positional
    kw = {factor: 3}
    begin
      Probe.call(11,4,kw)
    rescue ArgumentError
      "positional"
    end
  end
  def self.mixed
    counter = Counter.new
    result = Probe.call(11,4,offset: 6,**counter.produce(5),factor: 7,**counter.produce(3))
    result*10+counter.calls
  end
  def self.transformed
    kw = {factor: 3}
    Probe.call(11,4,**kw) { |r| r*2+1 }
  end
end
"#;
    // Without keyword provenance run is an arity error; treating a positional
    // hash as keywords changes positional; duplication/order errors change mixed.
    let script = "puts Probe.run; puts Probe.positional; puts Probe.mixed; puts Probe.transformed";
    let expected = "21\npositional\n232\n43\n";
    native_result(source, script, expected);
    let run = emit_and_run::real_blog()
        .write("app/lib/probe.rb", source)
        .run_ruby(script);
    run.assert_passes();
    assert_eq!(run.stdout, expected);
    let emitted = std::fs::read_to_string(run.emitted.join("app/models/probe.rb")).unwrap();
    assert!(emitted.contains("Probe.call(11, 4, **kw)"), "{emitted}");
    assert!(emitted.contains("Probe.call(11, 4, kw)"), "{emitted}");
}

#[test]
fn keyword_producer_projection_keeps_ordinary_lowering_and_super_contracts() {
    let source = r#"
class Parent
  def call(...); 11; end
end
class Child < Parent
  def call; kw = {factor: 3}; super(7,**kw); end
end
class Sink
  def target(a,b,factor:); (a-b)*factor; end
end
class Probe
  def self.run; helper=Sink.new; kw={factor:3}; helper.target(11,4,**kw); end
  def self.hash; kw={factor:3}; {**kw}; end
end
"#;
    let script = "puts Probe.run; puts Child.new.call; puts Probe.hash[:factor]";
    native_result(source, script, "21\n11\n3\n");
    let run = emit_and_run::real_blog()
        .write("app/lib/probe.rb", source)
        .run_ruby(script);
    run.assert_passes();
    assert_eq!(run.stdout, "21\n11\n3\n");
    let child = std::fs::read_to_string(run.emitted.join("app/models/child.rb")).unwrap();
    assert!(child.contains("super(7, **kw)"), "{child}");
    let ordinary = std::fs::read_to_string(run.emitted.join("app/models/probe.rb")).unwrap();
    assert!(
        !ordinary.contains("helper.target(11, 4, **kw)"),
        "{ordinary}"
    );
}

#[test]
fn instance_keyword_normalization_uses_the_effective_last_definition() {
    let flat = "def target(factor: 2); factor*11; end";
    let full = "def target(...); Leaf.accept(...); end";
    for (first, last, expected) in [(flat, full, "21\n"), (full, flat, "33\n")] {
        let source = format!(
            "class Sink; {first}; {last}; end; class Leaf; def self.accept(factor:); factor*7; end; end; class Probe; def self.run; sink=Sink.new; sink.target(factor:3); end; end"
        );
        let script = "puts Probe.run";
        native_result(&source, script, expected);
        let run = emit_and_run::real_blog()
            .write("app/lib/probe.rb", &source)
            .run_ruby(script);
        run.assert_passes();
        assert_eq!(run.stdout, expected);
    }
}

#[test]
fn unknown_ordinary_super_abi_is_not_mislabeled_as_full_forwarding() {
    // A class's own `super(**options)` whose lookup leaves the app's
    // declarations (here into StandardError) is written as the source
    // wrote it, and the Ruby tree binds it as Ruby does, empty packet
    // included. Simply restoring Legacy would emit super(options),
    // changing Ruby's empty-keyword semantics: the message becomes "{}".
    // A full forwarder elsewhere is not on this lookup, so it changes
    // nothing.
    for extra in [
        "",
        "class Other; def unrelated(...); 11; end; end",
        "class Other; def initialize(...); end; end",
    ] {
        for options in ["{}", "{tag: 1}"] {
            let source = format!("class Probe < StandardError; def initialize; options={options}; super(**options); end; end; {extra}");
            let script = "puts Probe.new.message";
            let native = Command::new("ruby")
                .args(["-e", &format!("{source}; {script}")])
                .output()
                .unwrap();
            assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
            let run = emit_and_run::real_blog()
                .write("app/lib/probe.rb", &source)
                .run_ruby(script);
            run.assert_passes();
            assert_eq!(run.stdout, String::from_utf8_lossy(&native.stdout), "{source}");
        }
    }
    // Not shown to leave the app's declarations, so still refused as an
    // ordinary super, never as full forwarding: a module's `super`
    // continues in its includer, and inside a block it is not shown
    // which method the `super` belongs to.
    for source in [
        "module Mixin; def initialize; options={}; super(**options); end; end; class Probe < StandardError; include Mixin; end",
        "class Probe < StandardError; def initialize; options={}; [1].each { super(**options) }; end; end",
    ] {
        native_result(source, "puts Probe.new.message", "Probe\n");
        let run = emit_and_run::real_blog()
            .write("app/lib/probe.rb", source)
            .run_ruby("puts Probe.new.message");
        assert!(run.errors.iter().any(|e| e.contains("ordinary super") && e.contains("argument ABI")), "{source}: {:?}", run.errors);
        assert!(!run.errors.iter().any(|e| e.contains("full forwarding")), "{:?}", run.errors);
    }
}

#[test]
fn source_super_keyword_rest_keeps_verified_legacy_projection() {
    for (options, expected) in [("{}", "8\n"), ("{left:33,right:11}", "22\n")] {
        let source = format!("class Parent; def initialize(**options); @proof=options.fetch(:left,11)-options.fetch(:right,3); end; def proof; @proof; end; end; class Probe < Parent; def initialize; options={options}; super(**options); end; end");
        let script = "puts Probe.new.proof";
        native_result(&source, script, expected);
        let run = emit_and_run::real_blog()
            .write("app/lib/probe.rb", &source)
            .run_ruby(script);
        run.assert_passes();
        assert_eq!(run.stdout, expected);
    }
}

#[test]
fn compiled_keyword_producer_into_forwarding_operator_keeps_call_syntax() {
    let source = "class Sink; def ==(...); 11; end; end; class Probe; def self.run; kw={factor:3}; Sink.new.==(**kw); end; end";
    native_result(source, "puts Probe.run", "11\n");
    let run = emit_and_run::real_blog()
        .write("app/lib/probe.rb", source)
        .run_ruby("puts Probe.run");
    run.assert_passes();
    assert_eq!(run.stdout, "11\n");
}

#[test]
fn ordinary_keyword_producers_keep_ingest_rewrites_and_call_parentheses() {
    let source = "class Probe; def self.accept(options={}); options[:factor]; end; def self.parens; Probe.accept **{factor:3}; end; def self.reverse; options={factor:7}; defaults={factor:3}; options.reverse_merge(**defaults)[:factor]; end; end";
    let script = "puts Probe.parens; puts Probe.reverse";
    let native = Command::new("ruby")
        .args([
            "-ractive_support/core_ext/hash/reverse_merge",
            "-e",
            &format!("{source};{script}"),
        ])
        .output()
        .unwrap();
    assert!(
        native.status.success(),
        "{}",
        String::from_utf8_lossy(&native.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&native.stdout), "3\n7\n");
    let run = emit_and_run::real_blog()
        .write("app/lib/probe.rb", source)
        .run_ruby(script);
    run.assert_passes();
    assert_eq!(run.stdout, "3\n7\n");
}

#[test]
fn ordinary_exists_keyword_conditions_keep_the_query_rewrite() {
    let source = "class Probe; def self.present(id); Article.exists?(**{id:id}); end; end";
    let native_source = format!(
        "require 'active_record'; ActiveRecord::Base.establish_connection(adapter:'sqlite3',database:':memory:'); ActiveRecord::Schema.define {{ create_table :articles }}; class Article < ActiveRecord::Base; end; {source}; a=Article.create!; puts Probe.present(a.id); puts Probe.present(a.id+11)"
    );
    let native = Command::new("ruby")
        .args(["-e", &native_source])
        .output()
        .unwrap();
    assert!(
        native.status.success(),
        "{}",
        String::from_utf8_lossy(&native.stderr)
    );
    assert!(String::from_utf8_lossy(&native.stdout).ends_with("true\nfalse\n"));
    let run = emit_and_run::real_blog().write("app/lib/probe.rb", source)
        .run_ruby("a=Article.new; a.title='present'; a.body='long enough article body'; a.save; puts Probe.present(a.id); puts Probe.present(a.id+11)");
    run.assert_passes();
    assert_eq!(run.stdout, "true\nfalse\n");
}

#[test]
fn forwarding_packet_is_not_used_as_a_reverse_merge_receiver() {
    let source = "class Sink; def reverse_merge(a,b,factor:); (a-b)*factor; end; end; class Probe; def self.call(...); Sink.new.reverse_merge(...); end; end";
    native_result(source, "puts Probe.call(11,4,factor:3)", "21\n");
    let run = emit_and_run::real_blog()
        .write("app/lib/probe.rb", source)
        .run_ruby("puts Probe.call(11,4,factor:3)");
    run.assert_passes();
    assert_eq!(run.stdout, "21\n");
}

#[test]
fn full_declaration_with_an_erased_ingest_selector_is_refused() {
    for source in [
        "class Probe; def self.reverse_merge(...); 11; end; def self.run; Probe.reverse_merge(**{factor:3}); end; end",
        "class ExistsResult; def exists?; 7; end; end; class Probe; def self.exists?(...); 11; end; def self.where(options); ExistsResult.new; end; def self.run; Probe.exists?(**{factor:3}); end; end",
    ] {
        native_result(source, "puts Probe.run", "11\n");
        let run = emit_and_run::real_blog()
            .write("app/lib/probe.rb", source)
            .run_ruby("puts Probe.run");
        assert!(
            run.errors.iter().any(|e| e.contains("ingest-time")),
            "errors={:?}; actual={}; stderr={}",
            run.errors,
            run.stdout,
            run.stderr
        );
    }
}

#[test]
fn compiled_empty_and_optional_keyword_packets_survive_two_hops() {
    let source = "class Sink; def self.scale(n,factor:2,**); n*factor; end; end; class Probe; def self.call(...); Sink.scale(...); end; def self.bridge(...); call(...); end; def self.empty; kw={}; Probe.bridge(7,**kw); end; def self.present; kw={factor:3}; Probe.bridge(7,**kw); end; end";
    let script = "puts Probe.empty; puts Probe.present";
    native_result(source, script, "14\n21\n");
    let run = emit_and_run::real_blog()
        .write("app/lib/probe.rb", source)
        .run_ruby(script);
    run.assert_passes();
    assert_eq!(run.stdout, "14\n21\n");
}

/// The lexical `target(factor:)` and the override's `target(...)` admit
/// neither one projection: no target without keyword arguments may erase
/// or keep the splat. The Ruby family writes it as written, and Ruby
/// binds it to whichever method the receiver dispatches to.
#[test]
fn competing_ordinary_and_full_virtual_keyword_contracts_refuse_projection() {
    let source = "class Sink; def self.leaf(factor:,extra:); factor-extra; end; end; class Parent; def run; kw={factor:11,extra:4}; target(**kw); end; def target(factor:); factor; end; end; class Child < Parent; def target(...); Sink.leaf(...); end; end";
    let script = "puts Child.new.run; puts Parent.new.run rescue puts $!.class";
    native_result(source, script, "7\nArgumentError\n");
    let run = emit_and_run::real_blog()
        .write("app/lib/probe.rb", source)
        .run_ruby(script);
    run.assert_passes();
    assert_eq!(run.stdout, "7\nArgumentError\n");
    let (_, errors) = emit_and_run::real_blog()
        .write("app/lib/probe.rb", source)
        .emit(roundhouse::project::BuildTarget::Spinel);
    assert!(
        errors.iter().any(|e| e.contains("keyword splat into full argument forwarding")),
        "{errors:?}"
    );
}

#[test]
fn keyword_producer_modifier_keeps_operand_grouping() {
    let source = "class Probe; def self.accept(...); 11; end; def self.run; kw={factor:3}; accept(**(kw rescue {factor:7})); end; end";
    native_result(source, "puts Probe.run", "11\n");
    let run = emit_and_run::real_blog()
        .write("app/lib/probe.rb", source)
        .run_ruby("puts Probe.run");
    run.assert_passes();
    assert_eq!(run.stdout, "11\n");
}

#[test]
fn ordinary_super_binds_natively_only_into_core_or_a_signature_port() {
    // A signature port's method is the native declaration (its parity
    // test holds it to activesupport), so the Ruby tree binds
    // `super(key, **options)` into MemoryStore's as Ruby would.
    // Through the public API, which instruments every operation through
    // ActiveSupport::Notifications: the same events native activesupport
    // publishes (tests/support/rails_oracle.rb's copy, run below).
    let source = "class ProbeStore < ActiveSupport::Cache::MemoryStore; def write_entry(key, entry, **options); options = options.merge(unless_exist: false); super(key, entry, **options); end; end";
    let script = "events = []; ActiveSupport::Notifications.subscribe(/cache_/) { |name, *, payload| events << [name, payload[:key]] }; s = ProbeStore.new; s.write('k', 7); p s.read('k'); s.fetch('j') { 3 }; p events";
    let oracle = std::env::temp_dir().join(format!("rh-cache-oracle-{}.rb", std::process::id()));
    std::fs::write(&oracle, format!(
        "require_relative {:?}\nRailsOracle.activate(\"activesupport\")\nrequire \"active_support\"\nrequire \"active_support/cache\"\nrequire \"active_support/cache/memory_store\"\nrequire \"active_support/notifications\"\n{source}\n{script}\n",
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/rails_oracle"),
    )).unwrap();
    let native = Command::new("ruby").arg(&oracle).output().unwrap();
    let _ = std::fs::remove_file(&oracle);
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    let expected = String::from_utf8_lossy(&native.stdout).into_owned();
    assert!(expected.contains("cache_write.active_support"), "{expected}");
    let run = emit_and_run::real_blog()
        .write("app/lib/probe_store.rb", source)
        .run_ruby(script);
    run.assert_passes();
    assert_eq!(run.stdout, expected);
    // The method's own block, passed on: what the implicit pass does.
    let source = "class Probe < StandardError; def initialize(&blk); options={}; super(**options, &blk); end; end";
    native_result(source, "puts Probe.new.message", "Probe\n");
    let run = emit_and_run::real_blog().write("app/lib/probe.rb", source).run_ruby("puts Probe.new.message");
    run.assert_passes();
    assert_eq!(run.stdout, "Probe\n");
    // The block the super writes is the one bound, whatever it is: a
    // literal block, or a block parameter the method reassigns.
    let source = "class Probe < Array; def sum; options={}; super(**options) { |x| x * 2 }; end; def map(&blk); blk = proc { |x| x + 1 }; options={}; super(**options, &blk); end; end";
    let script = "p Probe.new([1, 2, 3]).sum; p Probe.new([1, 2, 3]).map { |x| x * 5 }";
    native_result(source, script, "12\n[2, 3, 4]\n");
    let run = emit_and_run::real_blog().write("app/lib/probe.rb", source).run_ruby(script);
    run.assert_passes();
    assert_eq!(run.stdout, "12\n[2, 3, 4]\n");
    // Not shown to reach a native ABI, so refused as an ordinary super:
    // the runtime's Logger is no port; a class with no recorded
    // superclass; a relative superclass name an app class could mean;
    // the app reopening a core ancestor with the method.
    for source in [
        "class Probe < Logger; def add(severity, message = nil, progname = nil, **options); super(severity, message, progname, **options); end; end",
        "class Probe; def initialize; options={}; super(**options); end; end",
        "module Shop; class Base; end; end; module Shop; class Probe < Base; def initialize; options={}; super(**options); end; end; end; class Base; end",
        "class Object; def probe_hook(**o); o.size; end; end; class Probe < StandardError; def probe_hook; options={}; super(**options); end; end",
    ] {
        let (_, errors) = emit_and_run::real_blog()
            .write("app/lib/probe.rb", source)
            .emit(roundhouse::project::BuildTarget::Ruby);
        assert!(
            errors.iter().any(|e| e.contains("ordinary super") && e.contains("argument ABI")),
            "{source}: {errors:?}"
        );
        assert!(!errors.iter().any(|e| e.contains("full forwarding")), "{errors:?}");
    }
}

#[test]
fn super_in_methods_defined_from_the_superclass_own_list_binds_natively() {
    // Core's Targeting::Shop::LazySegments shape: a class body loop
    // defining a method for each of the superclass's own public methods,
    // whose `super` therefore reaches that core declaration, whatever the
    // name. The Ruby tree writes the statement and its `super` as written.
    let source = "class ProbeSet < Set\n  SKIP = %i[initialize initialize_dup initialize_clone add add? << merge replace clear inspect to_s].freeze\n  (Set.public_instance_methods(false) - SKIP).each do |name|\n    define_method(name) do |*args, **kwargs, &block|\n      @probe_calls = (@probe_calls || 0) + 1\n      super(*args, **kwargs, &block)\n    end\n  end\n  def probe_calls\n    @probe_calls\n  end\nend\n";
    let script = "s = ProbeSet.new([1, 2]); p s.include?(1); p s.to_a.sort; p s.size; p s.select { |x| x > 1 }.to_a; p s.probe_calls";
    native_result(source, script, "true\n[1, 2]\n2\n[2]\n4\n");
    let run = emit_and_run::real_blog().write("app/lib/probe_set.rb", source).run_ruby(script);
    run.assert_passes();
    assert_eq!(run.stdout, "true\n[1, 2]\n2\n[2]\n4\n");
    // Not shown: a superclass the runtime lowers (its Logger), and names
    // listed off another class than the superclass.
    for source in [
        "class ProbeLog < Logger\n  (Logger.public_instance_methods(false) - [:initialize]).each do |name|\n    define_method(name) do |*args, **kwargs, &block|\n      super(*args, **kwargs, &block)\n    end\n  end\nend\n",
        "class ProbeArr < Array\n  (Set.public_instance_methods(false) - [:initialize]).each do |name|\n    define_method(name) do |*args, **kwargs, &block|\n      super(*args, **kwargs, &block)\n    end\n  end\nend\n",
    ] {
        let (_, errors) = emit_and_run::real_blog()
            .write("app/lib/probe_set.rb", source)
            .emit(roundhouse::project::BuildTarget::Ruby);
        assert!(errors.iter().any(|e| e.contains("keyword splat")), "{source}: {errors:?}");
    }
}

/// The output of `source` and then `script` under native activesupport
/// (tests/support/rails_oracle.rb's copy) with its cache loaded.
fn native_cache_result(source: &str, script: &str) -> String {
    let oracle = std::env::temp_dir().join(format!("rh-cache-oracle-{}-{}.rb", std::process::id(), source.len()));
    std::fs::write(&oracle, format!(
        "require_relative {:?}\nRailsOracle.activate(\"activesupport\")\nrequire \"active_support\"\nrequire \"active_support/cache\"\nrequire \"active_support/cache/memory_store\"\nrequire \"active_support/notifications\"\n{source}\n{script}\n",
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/rails_oracle"),
    )).unwrap();
    let native = Command::new("ruby").arg(&oracle).output().unwrap();
    let _ = std::fs::remove_file(&oracle);
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    String::from_utf8_lossy(&native.stdout).into_owned()
}

#[test]
fn ordinary_super_binds_natively_from_a_reopened_runtime_module_and_a_yielding_block() {
    // The app reopens the runtime's LocalCache, which MemoryStore,
    // NullStore and MemCacheStore prepend: its `super` goes on in each
    // from just after it, into the store's own method, a native ABI.
    // A subclass's `super` reaches the reopening's declaration first,
    // which the Ruby tree writes with its source keywords.
    let local_cache = "module ActiveSupport; module Cache; module Strategy; module LocalCache; def write_entry(key, entry, **options); (@probe_log ||= []) << [:local, key, options.key?(:unless_exist)]; super(key, entry, **options); end; end; end; end; end\n";
    let source = format!("{local_cache}class ProbeStore < ActiveSupport::Cache::MemoryStore; attr_reader :probe_log; def write_entry(key, entry, **options); (@probe_log ||= []) << [:store, key]; options = options.merge(unless_exist: false); super(key, entry, **options); end; end");
    let script = "s = ProbeStore.new; s.write('k', 7); p s.read('k'); s.with_local_cache { s.write('j', 3); p s.read('j') }; p s.probe_log; m = ActiveSupport::Cache::MemoryStore.new; m.write('x', 1); p m.read('x')";
    let expected = native_cache_result(&source, script);
    assert!(expected.contains(":local"), "{expected}");
    let run = emit_and_run::real_blog().write("app/lib/probe_store.rb", &source).run_ruby(script);
    run.assert_passes();
    assert_eq!(run.stdout, expected);
    // A literal block passed to an app method that only yields stays
    // the method's block, so its `super` is the method's.
    let source = "class ProbeStore < ActiveSupport::Cache::MemoryStore; def read_entry(key, **options); guarded { super(key, **options) }; end; private; def guarded; yield; rescue ArgumentError; nil; end; end";
    let script = "s = ProbeStore.new; s.write('k', 7); p s.read('k'); p s.read('missing'); p s.fetch('j') { 3 }";
    let expected = native_cache_result(source, script);
    assert_eq!(expected, "7\nnil\n3\n");
    let run = emit_and_run::real_blog().write("app/lib/probe_store.rb", source).run_ruby(script);
    run.assert_passes();
    assert_eq!(run.stdout, expected);
    let (_, errors) = emit_and_run::real_blog()
        .write("app/lib/probe_store.rb", source)
        .emit(roundhouse::project::BuildTarget::Spinel);
    assert!(errors.iter().any(|e| e.contains("keyword splat in ordinary super")), "{errors:?}");
    // Not shown: a block the callee can capture (its own `&blk`, a
    // runtime method's, or an override's), and a reopened module an
    // app class also mixes in.
    for source in [
        "class ProbeStore < ActiveSupport::Cache::MemoryStore; def read_entry(key, **options); guarded { super(key, **options) }; end; private; def guarded(&blk); blk.call; end; end",
        "class ProbeStore < ActiveSupport::Cache::MemoryStore; def write_entry(key, entry, **options); synchronize { super(key, entry, **options) }; end; end",
        "class ProbeStore < ActiveSupport::Cache::MemoryStore; def read_entry(key, **options); guarded { super(key, **options) }; end; private; def guarded; yield; end; end; class ProbeChild < ProbeStore; private; def guarded(&blk); blk.call; end; end",
        &format!("{local_cache}class ProbeStore; include ActiveSupport::Cache::Strategy::LocalCache; end"),
        &format!("{local_cache}ActiveSupport::Cache::Strategy::LocalCache.module_eval {{ def probe; end }}"),
    ] {
        let (_, errors) = emit_and_run::real_blog()
            .write("app/lib/probe_store.rb", source)
            .emit(roundhouse::project::BuildTarget::Ruby);
        assert!(
            errors.iter().any(|e| e.contains("ordinary super") && e.contains("argument ABI")),
            "{source}: {errors:?}"
        );
    }
}

//! Nameless positional rests (`def m(*)`, `def m(sub, *)`, `def m(*, **)`)
//! reached through dynamic dispatch, as Shopify Core's InProcessRedis
//! writes them: a pipeline `method_missing` relaying `*args, **kwargs`
//! through `send`/`public_send`, `call` dispatching a computed command
//! name, a `send(handler, ...)`, and reified methods. The Ruby emitter
//! writes the `*` as written and keeps `**value` a keyword splat, so the
//! Ruby tree must run them as native Ruby does; native Ruby is the oracle.
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

use std::process::Command;

const LIBRARY: &str = r##"class RestStore
  def initialize
    @data = {}
    @log = []
  end

  def log
    @log
  end

  # The nameless `**` keeps `ex:` a keyword. Without one, the shared
  # ingest flattens an optional keyword to `ex = nil` in emitted Ruby too,
  # and a dynamic `public_send(:set, k, v, **kwargs)` binds the whole hash
  # to it: a separate gap, not a nameless-rest one.
  def set(key, value, ex: nil, **)
    @data[key.to_s] = value
    @log << ["set", key.to_s, ex]
    "OK"
  end

  def get(key)
    @data[key.to_s]
  end

  def watch(*)
    raise ArgumentError, "Blocked watch"
  end

  def select(*)
    raise NoMethodError, "select commands aren't supported"
  end

  def script(subcommand, *)
    return "OK" if subcommand.to_s.downcase == "flush"

    raise ArgumentError, "ERR unknown SCRIPT subcommand #{subcommand}"
  end

  def tally(*, **)
    "tally"
  end

  def keyed(a, *, k: 1)
    [a, k]
  end

  def gather(*, **opts)
    opts
  end

  def around(a, b = 0, *, z)
    [a, b, z]
  end

  def touch(h, *)
    h[:touched] = true
    h
  end

  def each_option(h)
    [yield(**h), yield(1, **h), yield(1, b: 2, **h)]
  end

  def relay(h)
    [touch(**h), send(:touch, **h), gather(1, **h), public_send(:keyed, 2, **h)]
  end

  def call(command, *arguments, **kwargs)
    command, arguments = normalize_call_arguments(command, arguments)
    method_name = command.to_s.downcase.to_sym
    raise NoMethodError, "undefined command #{command}" unless respond_to?(method_name)

    if kwargs.empty?
      public_send(method_name, *arguments)
    else
      public_send(method_name, *arguments, **kwargs)
    end
  end

  def command_parts(command, *args, **kwargs)
    [command.to_s, *args.map(&:to_s), *kwargs.flat_map { |key, value| [key.to_s, value.to_s] }]
  end

  def run_handler(handler, keys, argv)
    send(handler, keys, argv)
  end

  def pipelined
    pipeline = Pipeline.new(self)
    yield pipeline
    [pipeline.commands, pipeline.results]
  end

  class Pipeline
    def initialize(redis)
      @redis = redis
      @commands = []
      @results = []
    end

    def commands
      @commands
    end

    def results
      @results
    end

    def method_missing(method_name, *args, **kwargs, &block)
      if @redis.respond_to?(method_name)
        @commands << @redis.send(:command_parts, method_name, *args, **kwargs)
        result = begin
          if kwargs.empty?
            @redis.public_send(method_name, *args, &block)
          else
            @redis.public_send(method_name, *args, **kwargs, &block)
          end
        rescue ArgumentError, NoMethodError => error
          error.message
        end
        @results << result
        result
      else
        super
      end
    end

    def respond_to_missing?(method_name, include_private = false)
      @redis.respond_to?(method_name, include_private) || super
    end
  end

  private

  def handle_echo(keys, argv)
    [keys, argv]
  end

  def normalize_call_arguments(command, arguments)
    return [command.first, command.drop(1)] if command.is_a?(Array)

    [command, arguments]
  end
end
"##;

const ASSERTIONS: &str = r##"def attempt
  yield.inspect
rescue ArgumentError, NoMethodError => e
  "#{e.class}: #{e.message}"
end

store = RestStore.new
puts attempt { store.script("flush") }
puts attempt { store.script("FLUSH", "sync", 3) }
puts attempt { store.script("load", "x") }
puts attempt { store.script }
puts attempt { store.watch }
puts attempt { store.watch("a", "b", ex: 1) }
puts attempt { store.select(1) }
puts attempt { store.tally(1, a: 2) }
puts attempt { store.tally }
puts attempt { store.call("SET", "k", "v", ex: 5) }
puts attempt { store.call("get", "k") }
puts attempt { store.call(["script", "flush", "extra"]) }
puts attempt { store.call("watch", "k") }
puts attempt { store.call("tally", 1, b: 2) }
puts attempt { store.public_send(:script, "flush", ex: 1) }
puts attempt { store.send(:select, ex: 1) }
puts attempt { store.run_handler(:handle_echo, ["k"], ["v"]) }
puts attempt { store.method(:script).call("flush", 9) }
puts attempt { store.method(:script).arity }
puts attempt { store.method(:watch).arity }
puts attempt { store.method(:tally).arity }
puts attempt { store.method(:script).to_proc.call("flush") }
puts attempt { %w[flush other].map { |s| store.script(s, 1) rescue "raised" } }
piped = attempt do
  store.pipelined do |pipe|
    pipe.set("a", 1, ex: 3)
    pipe.watch("a")
    pipe.script("flush", "extra")
    pipe.script("load", "x", ex: 2)
    pipe.tally(1, c: 3)
    pipe.get("a")
  end
end
puts piped
puts attempt { store.keyed(1, 2, k: 3) }
puts attempt { store.keyed(1) }
puts attempt { store.keyed(1, 2, 3, k: 4) }
puts attempt { store.gather(1, a: 2) }
puts attempt { store.gather }
puts attempt { store.gather(**{ b: 3 }) }
puts attempt { store.around(1, 2) }
puts attempt { store.around(1, 2, 3, 4) }
puts attempt { store.around(1) }
puts attempt { store.public_send(:keyed, 1, k: 5) }
puts attempt { store.method(:gather).call(1, x: 1) }
puts attempt { [{ a: 1 }].map(&store.method(:gather)) }
puts attempt { [[1, 2]].map(&store.method(:keyed)) }
original = { k: 7 }
puts attempt { store.touch(**original) }
puts attempt { store.relay(original) }
puts attempt { original }
puts attempt { store.method(:keyed).parameters }
puts attempt { store.method(:around).parameters }
puts attempt { store.each_option({ a: 1 }) { |x = 0, a: 0, **rest| [x, a, rest] } }
puts attempt { store.each_option({ a: 2 }, &store.method(:gather)) }
puts attempt { store.log }
"##;

fn native() -> String {
    let native = Command::new("ruby")
        .arg("-e")
        .arg(format!("{LIBRARY}\n{ASSERTIONS}"))
        .output()
        .expect("native Ruby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    String::from_utf8_lossy(&native.stdout).into_owned()
}

#[test]
fn native_and_emitted_ruby_agree_on_dispatch_into_nameless_rests() {
    let expected = native();
    assert!(expected.contains("ERR unknown SCRIPT subcommand load"), "{expected}");
    let run = emit_and_run::real_blog()
        .write("app/lib/rest_store.rb", LIBRARY)
        .run_ruby(ASSERTIONS);
    run.assert_passes();
    assert_eq!(run.stdout, expected);
    let emitted = std::fs::read_to_string(run.emitted.join("app/models/rest_store.rb")).unwrap();
    for written in ["def watch(*)", "def script(subcommand, *)", "def tally(*, **)",
                    "def keyed(a, *, k: 1)", "def gather(*, **opts)", "def around(a, b = 0, *, z)",
                    "touch(**h), send(:touch, **h)", "[yield(**h), yield(1, **h)",
                    "@redis.send(:command_parts, method_name, *args, **kwargs)"] {
        assert!(emitted.contains(written), "{written} not kept:\n{emitted}");
    }
}

/// Off the CRuby tree the declaration stays refused, and so do the
/// reified and keyword-splat calls that reach it, by name per target.
#[test]
fn off_cruby_the_rests_and_their_dispatched_calls_are_refused_by_name() {
    use roundhouse::project::BuildTarget;
    for target in [BuildTarget::Jruby, BuildTarget::Spinel, BuildTarget::Typescript] {
        let (_, errors) = emit_and_run::real_blog().write("app/lib/rest_store.rb", LIBRARY).emit(target);
        for construct in [
            "anonymous positional rest declaration not supported",
            "method object of an anonymous positional rest not supported",
            "keyword splat into an anonymous positional rest not supported",
        ] {
            assert!(errors.iter().any(|e| e.contains(construct)), "{target:?} {construct}: {errors:?}");
        }
    }
}

/// The shapes `emit_and_run`'s nameless-rest refusal table once refused
/// on the CRuby tree only for the erased keyword splat or the extra
/// parameters: each runs there as native Ruby runs it, and every other
/// target still refuses it by name.
const ADMITTED_ON_CRUBY: &[(&str, &str, &str)] = &[
    ("keyword_splat_super", "class Probe\n  def m(*)\n    7\n  end\nend\n\nclass Kid < Probe\n  def m(**h)\n    super(**h)\n  end\nend\n", "Kid.new.m(a: 1)"),
    // A class-side `def self.make(h) = new(**h)` is refused for another
    // reason (the RBS writer's unsubstituted `instance`), so `new(**h)` is
    // reached from another class here.
    ("new_keyword_splat", "class Probe\n  def initialize(*, **kw)\n    @kw = kw\n  end\n\n  def kw\n    @kw\n  end\nend\n\nclass ProbeMaker\n  def go(h)\n    Probe.new(**h)\n  end\nend\n", "ProbeMaker.new.go({ k: 1 }).kw"),
    ("send_keyword_splat", "class Probe\n  def m(*, **kw)\n    kw\n  end\n\n  def go(h)\n    send(:m, **h)\n  end\nend\n", "Probe.new.go({ k: 1 })"),
    ("call_keyword_splat", "class Probe\n  def m(*, **kw)\n    kw\n  end\n\n  def go(h)\n    method(:m).call(**h)\n  end\nend\n", "Probe.new.go({ k: 1 })"),
    ("public_send_keyword_splat", "class Probe\n  def m(*, **kw)\n    kw\n  end\n\n  def go(h)\n    public_send(:m, **h)\n  end\nend\n", "Probe.new.go({ k: 1 })"),
    ("method_object_yield", "class Probe\n  def m(*, **kw)\n    kw\n  end\n\n  def each(h)\n    yield(**h)\n  end\n\n  def go(h)\n    each(h, &method(:m))\n  end\nend\n", "Probe.new.go({ k: 1 })"),
    ("method_object_value", "class Probe\n  def m(*, **kw)\n    kw\n  end\n\n  def go(h)\n    f = method(:m)\n    f.call(h)\n  end\nend\n", "Probe.new.go({ k: 1 })"),
    ("required_positional", "class Probe\n  def m(a, *)\n    a\n  end\nend\n", "[Probe.new.m(1, 2, 3), Probe.new.m({ a: 1 }, b: 2)]"),
    ("method_missing_name", "class Probe\n  def method_missing(name, *)\n    name\n  end\nend\n", "Probe.new.foo(1, k: 2)"),
    ("keyword_param", "class Probe\n  def m(a, *, k: 1)\n    [a, k]\n  end\nend\n", "[Probe.new.m(1, 2, k: 3), Probe.new.m(1)]"),
    ("optional_param", "class Probe\n  def m(a, b = 0, *)\n    [a, b]\n  end\nend\n", "[Probe.new.m(1), Probe.new.m(1, 2, 3)]"),
    ("post_param", "class Probe\n  def m(a, *, z)\n    [a, z]\n  end\nend\n", "[Probe.new.m(1, 2), Probe.new.m(1, 2, 3, 4)]"),
    ("method_missing_keywords", "class Probe\n  def method_missing(name, *, **kw)\n    [name, kw]\n  end\n\n  def go(h)\n    foo(**h)\n  end\nend\n", "Probe.new.go({ a: 1 })"),
    ("method_missing_positional", "class Probe\n  def method_missing(name, a, *)\n    a[:y] = 1\n    [name, a]\n  end\n\n  def go(h)\n    foo(**h)\n  end\nend\n", "h = { x: 1 }\n  [Probe.new.go(h), h]"),
    ("reify_public_send", "class Probe\n  def m(*)\n    :m\n  end\n\n  def each(h)\n    yield(**h)\n  end\n\n  def go(h, nm = :method)\n    each(h, &public_send(:method, :m))\n  end\nend\n", "Probe.new.go({ k: 1 })"),
    ("reify_send_strings", "class Probe\n  def m(*)\n    :m\n  end\n\n  def each(h)\n    yield(**h)\n  end\n\n  def go(h, nm = :method)\n    each(h, &__send__(\"method\", \"m\"))\n  end\nend\n", "Probe.new.go({ k: 1 })"),
    ("reify_public_method", "class Probe\n  def m(*)\n    :m\n  end\n\n  def each(h)\n    yield(**h)\n  end\n\n  def go(h, nm = :method)\n    each(h, &send(:public_method, :m))\n  end\nend\n", "Probe.new.go({ k: 1 })"),
    ("reify_computed_name", "class Probe\n  def m(*)\n    :m\n  end\n\n  def each(h)\n    yield(**h)\n  end\n\n  def go(h, nm = :method)\n    each(h, &method(\"m\".to_sym))\n  end\nend\n", "Probe.new.go({ k: 1 })"),
    ("reify_computed_reifier", "class Probe\n  def m(*)\n    :m\n  end\n\n  def each(h)\n    yield(**h)\n  end\n\n  def go(h, nm = :method)\n    each(h, &public_send(nm, :m))\n  end\nend\n", "Probe.new.go({ k: 1 })"),
    ("reify_public_instance_method", "class Probe\n  def m(*)\n    :m\n  end\n\n  def each(h)\n    yield(**h)\n  end\n\n  def go(h, nm = :method, n2 = :m)\n    each(h, &Probe.public_instance_method(:m).bind(self))\n  end\nend\n", "Probe.new.go({ k: 1 })"),
    ("reify_a_reifier", "class Probe\n  def m(*)\n    :m\n  end\n\n  def each(h)\n    yield(**h)\n  end\n\n  def go(h, nm = :method, n2 = :m)\n    each(h, &Kernel.instance_method(:method).bind_call(self, :m))\n  end\nend\n", "Probe.new.go({ k: 1 })"),
    ("reify_a_dispatcher", "class Probe\n  def m(*)\n    :m\n  end\n\n  def each(h)\n    yield(**h)\n  end\n\n  def go(h, nm = :method, n2 = :m)\n    each(h, &method(:public_send).call(:method, :m))\n  end\nend\n", "Probe.new.go({ k: 1 })"),
    ("reify_nested_dispatch", "class Probe\n  def m(*)\n    :m\n  end\n\n  def each(h)\n    yield(**h)\n  end\n\n  def go(h, nm = :method, n2 = :m)\n    each(h, &send(:send, :method, :m))\n  end\nend\n", "Probe.new.go({ k: 1 })"),
    ("reify_splatted_dispatch", "class Probe\n  def m(*)\n    :m\n  end\n\n  def each(h)\n    yield(**h)\n  end\n\n  def go(h, nm = :method, n2 = :m)\n    each(h, &send(*[:method, :m]))\n  end\nend\n", "Probe.new.go({ k: 1 })"),
    ("reify_computed_selector", "class Probe\n  def m(*)\n    :m\n  end\n\n  def each(h)\n    yield(**h)\n  end\n\n  def go(h, nm = :method, n2 = :m)\n    each(h, &public_send(:method, n2))\n  end\nend\n", "Probe.new.go({ k: 1 })"),
    ("keyword_splat_into", "class Probe\n  def m(*, **kw)\n    kw\n  end\n\n  def go(h)\n    m(**h)\n  end\nend\n", "Probe.new.go({ k: 1 })"),
];

/// One library of every case: its `Probe` and `Kid` renamed apart, beside
/// the shared `Base`, with one printed call per case.
fn admitted_cases() -> (String, String) {
    let mut library = String::from("class Base\n  def m(*args)\n    args.size\n  end\nend\n");
    let mut script = String::from(
        "def attempt\n  yield.inspect\nrescue StandardError => e\n  \"#{e.class}: #{e.message}\"\nend\n",
    );
    for (i, (name, probe, call)) in ADMITTED_ON_CRUBY.iter().enumerate() {
        let rename = |text: &str| text.replace("Probe", &format!("Probe{i}")).replace("Kid", &format!("Kid{i}"));
        library.push_str(&format!("\n{}", rename(probe)));
        script.push_str(&format!("puts \"{name}: \" + attempt {{\n  {}\n}}\n", rename(call)));
    }
    (library, script)
}

#[test]
fn shapes_once_refused_on_cruby_run_as_native_ruby_does() {
    let (library, script) = admitted_cases();
    let native = Command::new("ruby")
        .arg("-e")
        .arg(format!("{library}\n{script}"))
        .output()
        .expect("native Ruby control");
    assert!(native.status.success(), "{}", String::from_utf8_lossy(&native.stderr));
    let expected = String::from_utf8_lossy(&native.stdout).into_owned();
    assert_eq!(expected.lines().count(), ADMITTED_ON_CRUBY.len(), "{expected}");
    assert!(!expected.contains("Error:"), "a native case raised:\n{expected}");
    let run = emit_and_run::real_blog().write("app/lib/probes.rb", &library).run_ruby(&script);
    run.assert_passes();
    assert_eq!(run.stdout, expected);
}

#[test]
fn shapes_once_refused_on_cruby_stay_refused_elsewhere() {
    use roundhouse::project::BuildTarget;
    let (library, _) = admitted_cases();
    for target in [BuildTarget::Jruby, BuildTarget::Spinel] {
        let (_, errors) = emit_and_run::real_blog().write("app/lib/probes.rb", &library).emit(target);
        for construct in [
            "anonymous positional rest declaration not supported",
            "method object of an anonymous positional rest not supported",
            "keyword splat into an anonymous positional rest not supported",
        ] {
            assert!(errors.iter().any(|e| e.contains(construct)), "{target:?} {construct}: {errors:?}");
        }
    }
}

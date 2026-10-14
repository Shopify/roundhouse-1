//! `Debounce.enqueue(**token(record))` into `def self.enqueue(a:, b:,
//! c: "x", **rest)` — a class-method callee reached through a constant
//! receiver. Core's `BaseIndexingStrategy#index` is the shape; the emit
//! lost the `**` and CRuby raised `wrong number of arguments (given 1,
//! expected 0; required keywords: …)`.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const PLAIN: &str = "class Dispatcher
  def self.enqueue(shop_id:, record_id:, priority: \"default\", **rest)
    \"#{shop_id}:#{record_id}:#{priority}:#{rest.inspect}\"
  end
end
";

const SINGLETON: &str = "class SingletonDispatcher
  class << self
    def enqueue(shop_id:, record_id:, priority: \"default\", **rest)
      \"#{shop_id}:#{record_id}:#{priority}:#{rest.inspect}\"
    end
  end
end
";

const JOB: &str = "module Search
  class JobDispatcher < ApplicationJob
    queue_as :low

    class << self
      #: (shop_id: Integer, record_id: Integer, ?priority: String, **untyped) -> String
      def enqueue(shop_id:, record_id:, priority: \"default\", **rest)
        \"#{shop_id}:#{record_id}:#{priority}:#{rest.inspect}\"
      end
    end
  end
end
";

/// An unrelated instance method of the same name, with no keywords.
const OTHER: &str = "class Queue
  def enqueue(item)
    item
  end
end
";

/// The class method and an instance method share the name: a call on
/// the constant is the class side's.
const MIXED: &str = "class Mixed
  def self.enqueue(shop_id:, record_id:, **rest)
    \"#{shop_id}:#{record_id}:default:#{rest.inspect}\"
  end

  def enqueue(item)
    item
  end
end
";

const CALLER: &str = "class Caller
  def token(n)
    { shop_id: n, record_id: n + 1, extra: true }
  end

  def via_call(n)
    return :skipped if n < 0

    if n > 100
      :immediate
    elsif n > 50
      :other
    else
      Dispatcher.enqueue(**token(n))
    end
  end

  def via_param(klass, n)
    klass.enqueue(**token(n))
  end

  def via_mixed(n)
    Mixed.enqueue(**token(n))
  end

  def via_local(n)
    t = token(n)
    Dispatcher.enqueue(**t)
  end

  def via_singleton(n)
    SingletonDispatcher.enqueue(**token(n))
  end

  def via_job(n)
    Search::JobDispatcher.enqueue(**token(n))
  end
end
";

#[test]
fn a_double_splat_into_a_class_method_keeps_its_splat() {
    emit_and_run::real_blog()
        .write("app/models/dispatcher.rb", PLAIN)
        .write("app/models/singleton_dispatcher.rb", SINGLETON)
        .write("app/models/job_dispatcher.rb", JOB)
        .write("app/models/mixed.rb", MIXED)
        .write("app/models/queue.rb", OTHER)
        .write("app/models/caller.rb", CALLER)
        .run_ruby(
            r##"
c = Caller.new
want = "1:2:default:{extra: true}"
{ via_mixed: c.via_mixed(1), via_param: c.via_param(Dispatcher, 1), via_call: c.via_call(1), via_local: c.via_local(1),
  via_singleton: c.via_singleton(1), via_job: c.via_job(1) }.each do |k, got|
  raise "#{k}: #{got.inspect}" unless got == want
end
puts "kwsplat class method ok"
"##,
        )
        .assert_passes();
}

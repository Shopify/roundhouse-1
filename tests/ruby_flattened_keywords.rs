//! Calls to a method whose optional keywords ingest flattened.
//!
//! Ingest writes `def set(key, value, ex: nil, nx: false, **kwargs)` as
//! `def set(key, value, ex = nil, nx = false, kwargs = {})`
//! (`Param::from_keyword`, `Param::from_kwrest`). Ruby 3 binds a trailing
//! keyword Hash to the first optional positional, so a call no lowering
//! rewrote binds wrong: `set("d", 1, ex: 9)` gave `ex = {ex: 9}`. Core's
//! `InProcessRedis#set` has this shape. The ruby-family trees write the
//! flattened params back as keywords. Each lowering that moved a keyword
//! into a positional slot marks it (`lower::keyword_marks`), and the
//! emitter writes the source call back.
//!
//! A trailing keyword Hash the analyzer re-typed as a positional Hash
//! (`normalize_trailing_kwargs`, against a flattened `**opts`) is marked
//! the same way and written back unbraced.
//!
//! Native Ruby is the oracle: the same library and script run on CRuby
//! directly and in the emitted tree must print the same lines.

#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const LIBRARY: &str = r##"class KvStore
  def set(key, value, ex: nil, nx: false, **kwargs)
    "#{key}=#{value} ex=#{ex.inspect} nx=#{nx.inspect} rest=#{kwargs.inspect}"
  end

  def get(key, default: nil)
    "#{key} default=#{default.inspect}"
  end

  def window(start, from: 1, to: 2, step: 3)
    "window #{start} from=#{from} to=#{to} step=#{step}"
  end

  def tags(name, **opts)
    "tags #{name} #{opts.inspect}"
  end

  def self.span(start, from: 1, to: 2, step: 3)
    "span #{start} from=#{from} to=#{to} step=#{step}"
  end
end

class KvProxy
  def initialize(store)
    @store = store
  end

  def call(command, *arguments, **kwargs)
    @store.public_send(command, *arguments, **kwargs)
  end

  def direct
    @store.set("d", 1, ex: 9)
  end

  def typed_get
    KvStore.new.get("t", default: 4)
  end

  def typed_window
    KvStore.new.window(0, step: 9)
  end

  def class_span
    KvStore.span(1, to: 5)
  end

  def typed_tags
    KvStore.new.tags("x", a: 1)
  end
end
"##;

const ASSERTIONS: &str = r##"
proxy = KvProxy.new(KvStore.new)
puts proxy.call(:set, "k", 1, ex: 5)
puts proxy.call(:set, "k", 1)
puts proxy.call(:set, "k", 1, nx: true, keepttl: true)
puts proxy.call(:get, "g", default: 3)
puts proxy.call(:get, "g")
puts proxy.direct
puts proxy.typed_get
puts proxy.typed_window
puts proxy.class_span
puts proxy.typed_tags
"##;

/// Two view helpers: a forwarded `**` aimed past an optional keyword
/// (`lower::kwrest_forward`), and a receiverless keyword call that skips
/// two optional keywords (`lower::helper_kwargs`' gap defaults).
const TIME_HELPER: &str = r##"module KvTimeHelper
  def kv_tagged(name, style: :time, **attributes)
    "tagged #{name} style=#{style.inspect} attrs=#{attributes.inspect}"
  end

  def kv_range(name, from: 1, to: 2, step: 3)
    "range #{name} from=#{from} to=#{to} step=#{step}"
  end
end
"##;

const ROOMS_HELPER: &str = r##"module KvRoomsHelper
  def kv_labelled(name, **attributes)
    kv_tagged name, **attributes
  end

  def kv_stepped(name)
    kv_range(name, step: 7)
  end
end
"##;

/// A job's class-side wrappers (`lower::job_class_side`) forward the
/// instance `perform`'s flattened keyword on.
const JOB: &str = r##"class KvJob < ApplicationJob
  def perform(id, retries: 2)
    puts "job #{id} retries=#{retries.inspect}"
  end
end
"##;

/// The helpers are module functions in the emitted tree and mixins in
/// Rails; each side names its own receiver for the same calls.
const HELPER_ASSERTIONS: &str = r##"
puts ROOMS.kv_labelled("n", class: "x")
puts ROOMS.kv_labelled("n")
puts ROOMS.kv_stepped("s")
puts TIME.kv_tagged("t", style: :date, id: 1)
KvJob.perform_now(3, retries: 5)
KvJob.perform_now(4)
"##;

const NATIVE_PRELUDE: &str = r##"
class ApplicationJob
  def self.perform_now(*args, **kwargs) = new.perform(*args, **kwargs)
end
"##;

fn native() -> String {
    let out = std::process::Command::new("ruby")
        .arg("-e")
        .arg(format!(
            "{NATIVE_PRELUDE}\n{LIBRARY}\n{TIME_HELPER}\n{ROOMS_HELPER}\n{JOB}\n\
             ROOMS = TIME = Object.new.extend(KvTimeHelper, KvRoomsHelper)\n{ASSERTIONS}\n{HELPER_ASSERTIONS}"
        ))
        .output()
        .expect("spawn ruby");
    assert!(out.status.success(), "native oracle failed:\n{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("utf8")
}

/// Every emitted file under `dir` that defines one of this test's `Kv`
/// classes, modules or helpers.
fn emitted_kv_sources(dir: &std::path::Path) -> String {
    let mut out = String::new();
    let mut entries: Vec<_> = std::fs::read_dir(dir).expect("read emitted dir").flatten().map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            out.push_str(&emitted_kv_sources(&path));
        } else if path.extension().is_some_and(|e| e == "rb") {
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            if text.contains("Kv") || text.contains("kv_") {
                out.push_str(&format!("# {}\n{text}\n", path.display()));
            }
        }
    }
    out
}

#[test]
fn flattened_keywords_bind_as_native_ruby_binds_them() {
    let expected = native();
    assert_eq!(expected.lines().count(), 16, "oracle printed:\n{expected}");
    let run = emit_and_run::real_blog()
        .write("app/lib/kv_proxy.rb", LIBRARY)
        .write("app/helpers/kv_time_helper.rb", TIME_HELPER)
        .write("app/helpers/kv_rooms_helper.rb", ROOMS_HELPER)
        .write("app/jobs/kv_job.rb", JOB)
        .run_ruby(&format!("ROOMS = KvRoomsHelper\nTIME = KvTimeHelper\n{ASSERTIONS}\n{HELPER_ASSERTIONS}"));
    let emitted = emitted_kv_sources(&run.emitted.join("app"));
    assert_eq!(
        run.stdout, expected,
        "emitted CRuby differs from native\nstderr:\n{}\n{emitted}",
        run.stderr
    );
    // The lowered calls are written back as the source wrote them: the
    // spliced keywords by name, the filled-in defaults dropped.
    for call in [
        "KvStore.new.get(\"t\", default: 4)",
        "KvStore.new.window(0, step: 9)",
        "KvStore.span(1, to: 5)",
        "KvStore.new.tags(\"x\", a: 1)",
        "KvTimeHelper.kv_range(name, step: 7)",
        "KvTimeHelper.kv_tagged(name, **attributes)",
        "new.perform(id, retries: retries)",
        "def set(key, value, ex: nil, nx: false, **kwargs)",
    ] {
        assert!(emitted.contains(call), "missing `{call}` in\n{emitted}");
    }
}

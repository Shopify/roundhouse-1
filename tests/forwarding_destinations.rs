//! Which `...` destinations the analyzer can verify (src/analyze/
//! forwarding.rs). Core's delegates forward to a memoized instance
//! (`delegate :with_shop_exclusive_lock, to: :announcer` over
//! `@announcer ||= Podding::Locking::RedisAnnouncer.new`; GraphApi::
//! SchemaRegistry's `def default = @default ||= new`; api_versioning's
//! `current`, `@schedule ||= new` or `@schedule = new`), and to a library
//! value (`@set.each(...)`, `@listen_thread.join(...)`). The receiver is
//! then an instance of exactly one class, or a value whose methods the app
//! does not define: both are verified. Each admission keeps its
//! near-miss refused: another write of the ivar, an override of the
//! reader, a subclass `new` may build, an app subclass of the library
//! class.

use roundhouse::det::HashMap;
use std::path::PathBuf;
use std::process::Command;

use roundhouse::analyze::{diagnose, Analyzer};
use roundhouse::diagnostic::DiagnosticKind;
use roundhouse::ingest::ingest_app_from_tree;

/// The `full argument forwarding` refusals' details, by source line.
fn refusals(source: &str) -> Vec<(u32, String)> {
    let mut app = ingest_app_from_tree(HashMap::from_iter([(
        PathBuf::from("app/lib/probe.rb"),
        source.as_bytes().to_vec(),
    )]))
    .expect("ingest forwarding probe");
    Analyzer::new(&app).analyze(&mut app);
    diagnose(&app)
        .into_iter()
        .filter_map(|d| match &d.kind {
            DiagnosticKind::Unsupported { construct, detail, .. } if construct.as_str() == "full argument forwarding" => {
                let line = source[..d.span.start as usize].matches('\n').count() as u32 + 1;
                Some((line, detail.clone()))
            }
            _ => None,
        })
        .collect()
}

fn native(source: &str, script: &str) -> String {
    let out = Command::new("ruby").args(["-rset", "-e", &format!("{source}\n{script}")]).output().expect("ruby");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

const MEMOIZED: &str = r##"class Announcer
  def announce(a, b = 1)
    a + b
  end
end

class Locking
  def announcer
    @announcer ||= Announcer.new
  end

  def reset_announcer
    @announcer = nil
  end

  def announce(...) = announcer.announce(...)
end

class Registry
  def audience_for(name, default = "all")
    "#{name}:#{default}"
  end

  class << self
    def default = @default ||= new

    def audience_for(...) = default.audience_for(...)
  end
end

class Schedule
  def latest(n = 1)
    n * 2
  end

  class << self
    def current
      if @fresh
        @schedule ||= new
      else
        @fresh = true
        @schedule = new
      end
    end

    def latest(...) = current.latest(...)
  end
end

class Listener
  def initialize
    @set = Set.new([3, 1])
    @thread = Thread.new { 7 }
  end

  def each(...) = @set.each(...)

  def join(...) = @thread.join(...)
end
"##;

#[test]
fn memoized_instances_and_library_values_are_verified_destinations() {
    assert_eq!(refusals(MEMOIZED), Vec::<(u32, String)>::new());
    let script = "l = Locking.new; p [l.announce(1, 2), Registry.audience_for(:x, \"y\"), Schedule.latest(3), Schedule.latest, Listener.new.each.to_a, Listener.new.join(1).value]";
    assert_eq!(native(MEMOIZED, script), "[3, \"x:y\", 6, 2, [3, 1], 7]\n");
}

#[test]
fn near_misses_stay_unverified() {
    let unverified = "forwarding destination's declaration cannot be verified";
    for (case, source, line) in [
        (
            "another write of the memo",
            "class A; def go(x); x; end; end\nclass B; def go(x); x; end; end\nclass L\n  def a = @a ||= A.new\n  def swap = @a = B.new\n  def go(...) = a.go(...)\nend\n",
            6,
        ),
        (
            "a subclass overrides the reader",
            "class A; def go(x); x; end; end\nclass B; def go(x); x; end; end\nclass L\n  def a = @a ||= A.new\n  def go(...) = a.go(...)\nend\nclass M < L\n  def a = B.new\nend\n",
            5,
        ),
        (
            "`new` may build a subclass",
            "class R\n  def go(x); x; end\n  class << self\n    def default = @default ||= new\n    def go(...) = default.go(...)\n  end\nend\nclass S < R\n  def go(x, y); y; end\nend\n",
            5,
        ),
        (
            "the reader may answer something else",
            "class A; def go(x); x; end; end\nclass L\n  def a(flag = true)\n    return nil unless flag\n    @a ||= A.new\n  end\n  def go(...) = a.go(...)\nend\n",
            7,
        ),
        (
            "an app subclass of the library class",
            "class MySet < Set\n  def each(x); x; end\nend\nclass L\n  def initialize\n    @set = Set.new\n  end\n  def each(...) = @set.each(...)\nend\n",
            8,
        ),
    ] {
        let found = refusals(source);
        assert!(
            found.iter().any(|(l, detail)| *l == line && detail == unverified),
            "{case}: expected line {line} refused, got {found:?}"
        );
    }
}

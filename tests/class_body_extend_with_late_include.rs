//! A class-body `extend M` stays above the class-body calls that need
//! it, when an include further down is written among those calls.
//!
//! Core's `GraphApi::Admin::LineItem` writes `extend GraphApi::Model`
//! (which gives the class `model=`), then `pre_resolve_access`,
//! `self.model = -> { ::LineItem }`, then includes a module whose
//! `included(base)` hook sends `base` a class macro. That include is
//! written where the body's statements reach it, and the extend, moved
//! to sit right before it, ran after `self.model =`: "undefined method
//! 'model=' for class GraphApi::Admin::LineItem" (9 unit-1000 ids).
#[path = "support/emit_and_run.rs"]
mod emit_and_run;

const BASE: &str = r##"class ProbeBase
  def self.declared
    @declared ||= []
  end

  def self.declare(name)
    declared << name
  end
end
"##;

const MODEL_DSL: &str = r##"module ProbeModelDsl
  def model=(value)
    @model = value
  end

  def model
    @model.call
  end
end
"##;

const GUARD: &str = r##"module ProbeGuard
  def self.included(base)
    base.declare(:guard)
  end
end
"##;

const WIDGET: &str = r##"class ProbeWidget < ProbeBase
  extend ProbeModelDsl
  declare :first
  self.model = -> { String }
  include ProbeGuard
end
"##;

const FILES: [(&str, &str); 4] = [
    ("app/models/probe_base.rb", BASE),
    ("app/models/probe_model_dsl.rb", MODEL_DSL),
    ("app/models/probe_guard.rb", GUARD),
    ("app/models/probe_widget.rb", WIDGET),
];

fn app() -> emit_and_run::Overlay {
    let mut app = emit_and_run::empty_app().write("config/routes.rb", "Rails.application.routes.draw do\nend\n");
    for (path, source) in FILES {
        app = app.write(path, source);
    }
    app
}

const SCRIPT: &str = "p [ProbeWidget.model, ProbeWidget.declared]\n";
const EXPECTED: &str = "[String, [:first, :guard]]";

#[test]
fn the_extend_stays_above_the_calls_written_after_it() {
    let (emitted, errors) = app().emit(roundhouse::project::BuildTarget::Ruby);
    assert!(errors.iter().all(|e| !e.contains("probe")), "{}", errors.join("\n"));
    let widget = std::fs::read_to_string(emitted.join("app/models/probe_widget.rb")).unwrap();
    let extend = widget.find("extend ProbeModelDsl").unwrap_or_else(|| panic!("no extend:\n{widget}"));
    let model = widget.find("self.model = ").unwrap_or_else(|| panic!("no model=:\n{widget}"));
    assert!(extend < model, "extend must precede `self.model =`:\n{widget}");
}

#[test]
fn the_class_loads_and_answers_as_plain_ruby_does() {
    let dir = std::env::temp_dir().join(format!("rh-extend-late-include-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut load = String::new();
    for (path, source) in FILES {
        let name = std::path::Path::new(path).file_name().unwrap();
        std::fs::write(dir.join(name), source).unwrap();
        load.push_str(&format!("require_relative \"{}\"\n", name.to_str().unwrap().trim_end_matches(".rb")));
    }
    let native = emit_and_run::ruby()
        .arg("-e")
        .arg(format!("{load}{SCRIPT}"))
        .current_dir(&dir)
        .output()
        .expect("spawn ruby");
    assert_eq!(String::from_utf8_lossy(&native.stdout).trim(), EXPECTED, "{}", String::from_utf8_lossy(&native.stderr));
    let emitted = app().run_ruby(SCRIPT);
    emitted.assert_passes();
    assert_eq!(emitted.stdout.trim(), EXPECTED, "emitted:\n{}", emitted.stdout);
}

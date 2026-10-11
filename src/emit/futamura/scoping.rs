//! Which models may carry a `default_scope`.
//!
//! `Model.all` applies a model's default scopes, and the specialized block
//! is built once and cached, so a default scope that reads request state
//! (`default_scope { where(shop: Current.shop) }`) would be frozen into the
//! statement. A chain on such a model stays Rails'. Rails records default
//! scopes per class and inherits them down the superclass chain
//! (`ActiveRecord::Scoping::Default::ClassMethods#default_scopes`,
//! activerecord/lib/active_record/scoping/default.rb), and a module reaches
//! a class through `include`/`prepend`/`extend`, transitively. So a model
//! is judged by its own source, its superclass chain, and every module it
//! pulls in, followed through their sources.
//!
//! The walk reads source text rather than the IR: an `included do` block's
//! `default_scope` call is not modeled. A reference is followed by the last
//! segment of its constant name to every app source that is, or declares, a
//! class or module of that name (an over-approximation across namespaces).
//! A constant with no app source is a gem's or the framework's, whose
//! scopes Rails reports at run time (`default_scopes`, which the runtime
//! checks before it builds a statement). An include whose argument is not
//! a constant path cannot be followed, so the model stays residue.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use crate::App;
use crate::dialect::Model;

/// What one source file says about scopes and the modules it pulls in.
#[derive(Clone, Default)]
struct Facts {
    /// A non-comment line mentions `default_scope`.
    default_scope: bool,
    /// An `include`/`prepend`/`extend` whose argument is not a constant path.
    dynamic: bool,
    /// Normalized last segments of included modules and superclasses.
    refs: Vec<String>,
}

pub struct DefaultScopes<'a> {
    app: &'a App,
    /// Normalized constant name -> sources that are or declare it.
    by_name: HashMap<String, Vec<usize>>,
    facts: RefCell<HashMap<usize, Facts>>,
}

/// `ApiClient`, `API_client` and `api_client` are one name here, so a
/// constant spelled under an app inflection still finds its file.
fn normalize(segment: &str) -> String {
    segment.chars().filter(|c| *c != '_').map(|c| c.to_ascii_lowercase()).collect()
}

fn last_segment(path: &str) -> &str {
    path.rsplit("::").next().unwrap_or(path)
}

/// Namespaces the framework owns: never an app source, and its own scopes
/// are reported by `default_scopes` at run time.
const FRAMEWORK_ROOTS: [&str; 12] = [
    "ActiveRecord", "ActiveSupport", "ActiveModel", "ActionController", "ActionView", "ActiveJob", "ActiveStorage",
    "ActionText", "ActionMailer", "Rails", "Comparable", "Enumerable",
];

fn is_framework(path: &str) -> bool {
    let root = path.trim_start_matches("::").split("::").next().unwrap_or("");
    FRAMEWORK_ROOTS.contains(&root)
}

fn is_const_path(s: &str) -> bool {
    let s = s.strip_prefix("::").unwrap_or(s);
    !s.is_empty()
        && s.split("::").all(|seg| {
            seg.chars().next().is_some_and(|c| c.is_ascii_uppercase())
                && seg.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
}

/// The first whitespace-delimited token of `rest`, minus call parentheses.
fn token(rest: &str) -> &str {
    let rest = rest.trim_start_matches(['(', ' ', '\t']);
    rest.split(|c: char| c.is_whitespace() || c == ')' || c == ',').next().unwrap_or("")
}

fn scan(text: &str) -> Facts {
    let mut facts = Facts::default();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.contains("default_scope") {
            facts.default_scope = true;
        }
        for keyword in ["include", "prepend", "extend"] {
            let Some(rest) = line.strip_prefix(keyword) else { continue };
            if !rest.starts_with([' ', '\t', '(']) {
                continue;
            }
            // `include A, B` mixes in both; `extend self` is the module's own.
            for arg in rest.split(',') {
                let arg = token(arg);
                if arg == "self" || arg.is_empty() {
                    continue;
                }
                if is_framework(arg) {
                } else if is_const_path(arg) {
                    facts.refs.push(normalize(last_segment(arg)));
                } else {
                    facts.dynamic = true;
                }
            }
        }
        if let Some(rest) = line.strip_prefix("class ")
            && let Some((_, parent)) = rest.split_once('<')
        {
            let parent = token(parent);
            if is_const_path(parent) && !is_framework(parent) {
                facts.refs.push(normalize(last_segment(parent)));
            }
        }
    }
    facts
}

impl<'a> DefaultScopes<'a> {
    pub fn new(app: &'a App) -> Self {
        let mut by_name: HashMap<String, Vec<usize>> = HashMap::new();
        let mut add = |name: String, source: usize| {
            let entry = by_name.entry(name).or_default();
            if !entry.contains(&source) {
                entry.push(source);
            }
        };
        for (i, s) in app.sources.iter().enumerate() {
            if !s.path.ends_with(".rb") || s.path.contains("/test/") || s.path.starts_with("test/") || s.path.contains("/spec/") {
                continue;
            }
            let stem = s.path.rsplit('/').next().unwrap_or(&s.path).trim_end_matches(".rb");
            add(normalize(stem), i);
            for line in s.text.lines() {
                let line = line.trim_start();
                if let Some(rest) = line.strip_prefix("class ").or_else(|| line.strip_prefix("module ")) {
                    let name = rest.split(|c: char| c.is_whitespace() || c == '<' || c == ';').next().unwrap_or("");
                    if is_const_path(name) {
                        add(normalize(last_segment(name)), i);
                    }
                }
            }
        }
        DefaultScopes { app, by_name, facts: RefCell::new(HashMap::new()) }
    }

    fn facts(&self, source: usize) -> Facts {
        self.facts
            .borrow_mut()
            .entry(source)
            .or_insert_with(|| scan(&self.app.sources[source].text))
            .clone()
    }

    /// Why `model` may have a default scope, or `None` when nothing it
    /// inherits or includes declares one.
    pub fn why(&self, model: &Model) -> Option<String> {
        let mut seen: HashSet<usize> = HashSet::new();
        let mut stack: Vec<usize> = Vec::new();
        let own = (model.span.file.0 as usize).wrapping_sub(1);
        if own < self.app.sources.len() {
            stack.push(own);
        }
        let mut names = vec![normalize(last_segment(model.name.0.as_str()))];
        names.extend(model.parent.iter().map(|p| normalize(last_segment(p.0.as_str()))));
        for name in names {
            stack.extend(self.by_name.get(&name).into_iter().flatten().copied());
        }
        while let Some(source) = stack.pop() {
            if !seen.insert(source) {
                continue;
            }
            let path = &self.app.sources[source].path;
            let facts = self.facts(source);
            if facts.default_scope {
                return Some(format!("{path} declares a default_scope"));
            }
            if facts.dynamic {
                return Some(format!("{path} includes a module the specializer cannot resolve"));
            }
            for name in &facts.refs {
                stack.extend(self.by_name.get(name).into_iter().flatten().copied());
            }
        }
        None
    }
}

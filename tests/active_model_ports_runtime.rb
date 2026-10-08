# Native Rails oracle (the Rails native Core runs; tests/support/
# rails_oracle.rb) for the ActiveModel ports of the PostgreSQL CRuby
# tree: runtime/spinel/active_model_naming.rb (Name, Naming,
# Conversion), active_model_attribute_methods.rb (AttributeMethods,
# MissingAttributeError, ActiveSupport::CodeGenerator),
# active_model_serialization.rb (Serialization, Serializers::JSON) and
# active_support_string_inflections.rb (String's inflection methods).
#
#   ruby tests/active_model_ports_runtime.rb compare
#
# runs this script twice: `native` requires the pinned activemodel's
# active_model (with active_support/core_ext, as a Rails app has it);
# `emitted` loads the ports with what the emitted PostgreSQL tree's boot
# loads before them (the CRuby overlay's core_ext, Module#delegate, the
# Inflector), on Core's gem home alone (bin/run-target's GEM_PATH). Each
# prints one line per observation and `compare` requires them equal.
#
# Not observed: what reads I18n (`Name#human` with an `i18n_scope`,
# ActiveModel::Translation), which the tree does not carry.
require_relative "support/rails_oracle"

if ARGV.first == "compare"
  require "open3"
  outputs = ["native", "emitted"].map do |mode|
    env = {}
    if mode == "emitted"
      env = RailsOracle.core_gems_env.merge("GEM_HOME" => RailsOracle::CORE_GEM_HOME, "GEM_PATH" => RailsOracle::CORE_GEM_HOME)
    end
    stdout, stderr, status = Open3.capture3(env, RbConfig.ruby, File.expand_path(__FILE__), mode)
    abort "#{mode}: #{stderr}" unless status.success?
    stdout
  end
  native, emitted = outputs.map(&:lines)
  equal = native.zip(emitted).count { |x, y| x == y }
  unless outputs.uniq.length == 1
    diff = native.zip(emitted).each_with_index.reject { |(x, y), _| x == y }.first(8)
    abort "ports differ (#{equal}/#{native.length} observations equal, emitted has #{emitted.length}):\n" +
      diff.map { |(x, y), i| "line #{i + 1}\n  native:  #{x}  emitted: #{y}" }.join("\n")
  end
  puts outputs.first
  puts "observations: #{equal}/#{native.length} equal"
  puts "Native Rails and the emitted ActiveModel ports agree (#{RailsOracle.label})"
  exit
end

MODE = ARGV.first
abort "usage: #{$PROGRAM_NAME} compare | native | emitted" unless %w[native emitted].include?(MODE)

if MODE == "native"
  RailsOracle.activate("activesupport", "activemodel")
  require "active_support"
  require "active_support/core_ext/object/blank"
  require "active_support/core_ext/object/json"
  require "active_support/core_ext/string/inflections"
  require "active_model"
else
  runtime = File.expand_path("../runtime/spinel", __dir__)
  require File.join(runtime, "scaffold/ruby_overlay/runtime/active_support_core_ext")
  require File.join(runtime, "module_delegate")
  require File.join(runtime, "active_support_inflector")
  require File.join(runtime, "active_support_string_inflections")
  require File.join(runtime, "active_model_naming")
  require File.join(runtime, "active_model_attribute_methods")
  require File.join(runtime, "active_model_serialization")
end

def show(label, value)
  puts "#{label}: #{value.inspect}"
end

def outcome
  yield.inspect
rescue Exception => e
  "#{e.class}: #{e.message.lines.first.to_s.chomp.sub(/0x\h+/, "0x").inspect}"
end

# ---- String inflections
words = %w[post Person octopus sheep ApiKey api/v1/user_record admin_users Status quiz person_id]
words.each do |w|
  show("string #{w}", [w.pluralize, w.singularize, w.camelize, w.camelize(:lower), w.underscore, w.dasherize,
                       w.demodulize, w.deconstantize, w.tableize, w.classify, w.humanize, w.titleize,
                       w.foreign_key, w.foreign_key(false), w.upcase_first, w.downcase_first, w.pluralize(1),
                       w.pluralize(2), w.humanize(capitalize: false), w.titleize(keep_id_suffix: true)])
end
show("string camelize invalid", outcome { "x".camelize(:middle) })
show("string constantize", ["Comparable".constantize, "Nope::Missing".safe_constantize, outcome { "NoSuchThing".constantize }])
show("string camelcase/titlecase", ["ab_cd".camelcase, "ab_cd".titlecase])

# ---- ActiveModel::Name / Naming
class Post
  extend ActiveModel::Naming
  include ActiveModel::Conversion
  attr_accessor :id
  def initialize(id = nil) = @id = id
  def persisted? = !id.nil?
end

class SpecialPost < Post; end

module Blog
  class Post
    extend ActiveModel::Naming
  end
end

module Admin
  def self.use_relative_model_naming? = true
  class User
    extend ActiveModel::Naming
    include ActiveModel::Conversion
  end
end

class Sheep
  extend ActiveModel::Naming
end

def name_fields(name)
  [name.name, name.singular, name.plural, name.element, name.collection, name.singular_route_key,
   name.route_key, name.param_key, name.i18n_key, name.human, name.uncountable?, name.cache_key, name.to_s]
end

[Post, SpecialPost, Blog::Post, Admin::User, Sheep].each do |klass|
  show("model_name #{klass}", name_fields(klass.model_name))
end
show("model_name class and frozen", [Post.model_name.class.name, Post.model_name.frozen?, Post.model_name.equal?(Post.model_name)])
show("subclass model_name is its own", [SpecialPost.model_name.equal?(Post.model_name), SpecialPost.model_name.name])
show("instance model_name", [Post.new.model_name.name, Post.new.respond_to?(:model_name)])
named = ActiveModel::Name.new(Post, nil, "Article")
show("explicit name", name_fields(named))
show("namespaced name", name_fields(ActiveModel::Name.new(Blog::Post, Blog)))
show("anonymous", outcome { ActiveModel::Name.new(Class.new) })
show("anonymous named", name_fields(ActiveModel::Name.new(Class.new, nil, "Anon")))
n = Post.model_name
show("name comparisons", [n == "Post", n === "Post", n <=> "Post", n <=> "Q", n =~ /os/, n !~ /zz/, n.eql?("Post"),
                          n.match?(/P/), n.to_str, n.as_json, "x#{n}", n == Post.model_name, n < ActiveModel::Name.new(Blog::Post, Blog)])
show("Naming helpers class", [ActiveModel::Naming.plural(Post), ActiveModel::Naming.singular(Post), ActiveModel::Naming.uncountable?(Sheep),
                              ActiveModel::Naming.singular_route_key(Blog::Post), ActiveModel::Naming.route_key(Sheep),
                              ActiveModel::Naming.param_key(Admin::User)])
show("Naming helpers record", [ActiveModel::Naming.plural(Post.new), ActiveModel::Naming.param_key(Admin::User.new),
                               ActiveModel::Naming.route_key(Admin::User.new)])
show("Naming private helper", outcome { ActiveModel::Naming.model_name_from_record_or_class(Post) })

# ---- ActiveModel::Conversion
class Pair
  extend ActiveModel::Naming
  include ActiveModel::Conversion
  attr_accessor :id, :saved
  def initialize(id, saved = true)
    @id = id
    @saved = saved
  end
  def persisted? = saved
end

class UnderscorePair < Pair
  self.param_delimiter = "_"
end

class Plain
  include ActiveModel::Conversion
  def self.name = "Shop::PlainThing"
  def persisted? = true
  def id = 7
end

show("conversion", [Post.new.to_key, Post.new(3).to_key, Post.new.to_param, Post.new(3).to_param, Post.new(3).to_model.class.name])
show("composite", [Pair.new([1, 2]).to_param, UnderscorePair.new([1, 2]).to_param, Pair.new([1, nil]).to_param,
                   Pair.new([1, 2], false).to_param, Pair.new(nil).to_key, Pair.new(false).to_key])
show("param_delimiter", [Pair.param_delimiter, UnderscorePair.param_delimiter, Post.param_delimiter, Pair.param_delimiter?,
                         Pair.new(1).respond_to?(:param_delimiter), Pair.new(1).respond_to?(:param_delimiter=)])
pair = Pair.new([4, 5])
pair.param_delimiter = "+"
show("instance writer", [pair.to_param, Pair.param_delimiter])
show("partial path", [Post.new.to_partial_path, Blog::Post.respond_to?(:_to_partial_path), Admin::User.new.to_partial_path,
                      Plain.new.to_partial_path, Plain.new.to_param, UnderscorePair.new(1).to_partial_path])
show("conversion ancestors", [Post.include?(ActiveModel::Conversion), Post.singleton_class.include?(ActiveModel::Conversion::ClassMethods)])

# ---- ActiveModel::AttributeMethods
class Person
  include ActiveModel::AttributeMethods

  attribute_method_suffix "_changed?", "?"
  attribute_method_prefix "clear_"
  attribute_method_affix prefix: "reset_", suffix: "_to_default!"
  define_attribute_methods :name, :age
  alias_attribute :nickname, :name

  attr_accessor :attributes

  def initialize(attributes = {})
    @attributes = { "name" => "Bob", "age" => 3 }.merge(attributes)
  end

  private
    def attribute(attr) = @attributes[attr]
    def attribute?(attr) = !!@attributes[attr]
    def attribute_changed?(attr) = attr == "age"
    def clear_attribute(attr) = @attributes[attr] = nil
    def reset_attribute_to_default!(attr) = @attributes[attr] = "Default"
end

p1 = Person.new
show("attribute readers", [p1.name, p1.age, p1.name?, p1.age_changed?, p1.name_changed?, p1.nickname, p1.nickname?])
p1.clear_name
p1.reset_age_to_default!
show("attribute writers", [p1.attributes, p1.name?, p1.nickname])
show("respond_to", %i[name age? clear_age reset_name_to_default! nickname_changed? email email? clear_email].map { |m| p1.respond_to?(m) })
show("attribute alias", [Person.attribute_alias?(:nickname), Person.attribute_alias(:nickname), Person.attribute_aliases, Person.attribute_alias?(:name)])
show("patterns", Person.attribute_method_patterns.map { |pt| [pt.prefix, pt.suffix, pt.proxy_target] })
generated = Person.ancestors.find { |a| a.is_a?(Module) && a.name.nil? && !a.is_a?(Class) }
show("generated methods", generated.instance_methods(false).sort)
show("missing", outcome { p1.email })
show("missing ?", outcome { Person.new.email? })
show("attribute_missing via method_missing", begin
  extra = Person.new("email" => "a@b.c")
  [extra.respond_to?(:email), extra.email, extra.email?, extra.clear_email, extra.attributes["email"]]
end)
Person.undefine_attribute_methods
show("after undefine", [generated.instance_methods(false).sort, Person.new.respond_to?(:name), Person.new.name, Person.new.respond_to?(:age_changed?)])
Person.define_attribute_methods :age
show("after redefine", [generated.instance_methods(false).sort, Person.new.age?])
show("define one", [Person.define_attribute_method(:name).class, Person.new.respond_to?(:clear_name)])
show("MissingAttributeError", [ActiveModel::MissingAttributeError.ancestors.take(3).map(&:name), outcome { raise ActiveModel::MissingAttributeError, "gone" }])

class Child < Person
  attribute_method_suffix "_was"
  define_attribute_methods :name
  private def attribute_was(attr) = "was #{attribute(attr)}"
end
show("subclass patterns", [Child.attribute_method_patterns.count, Person.attribute_method_patterns.count, Child.new.name_was,
                           Person.new.respond_to?(:name_was)])

# ---- ActiveModel::Serialization / Serializers::JSON
class Author
  include ActiveModel::Serializers::JSON
  attr_accessor :name, :email
  def initialize(name, email = nil)
    @name = name
    @email = email
  end
  def attributes = { "name" => nil, "email" => nil }
end

class Book
  include ActiveModel::Serializers::JSON
  attr_accessor :title, :pages, :author, :coauthors, :published
  def initialize(title, pages, author, coauthors = [])
    @title = title
    @pages = pages
    @author = author
    @coauthors = coauthors
    @published = Time.utc(2024, 1, 2, 3, 4, 5)
  end
  def attributes = { "title" => nil, "pages" => nil, "published" => nil }
  def greet = "hi #{title}"
end

book = Book.new("Dune", 412, Author.new("Frank", "f@x"), [Author.new("Brian"), Author.new("Kevin")])
show("serializable_hash", book.serializable_hash)
show("only/except", [book.serializable_hash(only: :title), book.serializable_hash(only: ["title", :pages, :nope]),
                     book.serializable_hash(except: [:pages, :published])])
show("methods", book.serializable_hash(only: :title, methods: [:greet]))
show("include", book.serializable_hash(only: :title, include: :author))
show("include nested", book.serializable_hash(only: :title, include: { author: { only: :name }, coauthors: { except: :email } }))
show("include list", book.serializable_hash(only: :title, include: [:author, { coauthors: { only: :name } }]))
show("as_json", [book.as_json(only: [:title, :published]), book.as_json(only: :title, root: true), book.as_json(only: :title, root: "doc")])
show("include_root_in_json", [Book.include_root_in_json, Book.include_root_in_json?, book.include_root_in_json])
Book.include_root_in_json = true
show("as_json with root", [book.as_json(only: :pages), book.as_json(only: :pages, root: false), Author.include_root_in_json])
show("json naming", [Book.model_name.name, book.model_name.element, Book.singleton_class.include?(ActiveModel::Naming)])
show("read_attribute_for_serialization", book.read_attribute_for_serialization(:title))

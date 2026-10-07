# sorbet-runtime's inline type check, the one `T.let`, `T.cast`,
# `T.bind` and `T.assert_type!` perform (sorbet-runtime 0.6.13502:
# lib/types/_types.rb, private/casts.rb, types/base.rb, types/simple.rb,
# types/union.rb, types/typed_*.rb, types/class_of.rb, utils.rb).
#
# No tree loads sorbet-runtime, so `ingest::expr` lowers each of those
# calls to `SorbetRuntimeCheck.cast(value, type, "T.let", path, line)`:
# the value when it passes, else the TypeError sorbet raises, message
# and `Caller:` line included. Core does not replace
# `T::Configuration`'s inline type error handler (whose default re-raises)
# nor turn off `include_value_in_type_errors`, so neither is modeled.
#
# The type argument is a Module, as written, or one of the type objects
# below, which the ingest builds for `T.nilable`, `T.any`, `T::Boolean`,
# `T.untyped`, `T.anything`, `T.class_of` and the stdlib generics
# (`T::Array[X]` and friends, which check the outer class only, as
# sorbet's `valid?` does). A user generic `Foo[X]` is `Foo` itself:
# `T::Generic#[]` answers `self`. Type forms the ingest cannot build here
# (`T.proc`, `T.self_type`, `T.attached_class`, type parameters, shapes)
# are refused at the site and never reach this file.
#
# CRuby overlay only (it reads `Method#owner` and binds `Module#name`);
# `project` refuses the call on every other tree.
module SorbetRuntimeCheck
  NAME_METHOD = ::Module.instance_method(:name)

  # `T.let(value, type)` and the other three: the value when sorbet's
  # `valid?` accepts it, else sorbet's TypeError.
  def self.cast(value, type, cast_method, path, line)
    if type.is_a?(::Module)
      return value if value.is_a?(type)
    elsif type.valid?(value)
      return value
    end
    raise ::TypeError.new("#{cast_method}: #{coerce(type).error_message(value)}\nCaller: #{path}:#{line}")
  end

  # `T::Types::Simple::Private::Pool.type_for_module`: a bare stdlib
  # collection class names itself as its untyped generic.
  def self.coerce(type)
    return type unless type.is_a?(::Module)
    if type == ::Array
      Generic.new("T::Array", ::Array, [UNTYPED])
    elsif type == ::Hash
      Generic.new("T::Hash", ::Hash, [UNTYPED, UNTYPED])
    elsif type == ::Enumerable
      Generic.new("T::Enumerable", ::Enumerable, [UNTYPED])
    elsif type == ::Enumerator
      Generic.new("T::Enumerator", ::Enumerator, [UNTYPED])
    elsif type == ::Range
      Generic.new("T::Range", ::Range, [UNTYPED])
    elsif defined?(::Set) && type == ::Set
      Generic.new("T::Set", ::Set, [UNTYPED])
    else
      Simple.new(type)
    end
  end

  # `T::Types::Base#describe_obj` and `#error_message`.
  module Described
    def describe_obj(obj)
      case obj
      when nil, true, false
        return "type #{obj.class}"
      end
      begin
        if obj.method(:inspect).owner == ::Kernel
          "type #{obj.class} with hash #{obj.hash}"
        else
          "type #{obj.class} with value #{SorbetRuntimeCheck.string_truncate_middle(obj.inspect, 30, 30)}"
        end
      rescue StandardError, SystemStackError
        "type #{obj.class} with unprintable value"
      end
    end

    def error_message(obj)
      "Expected type #{name}, got #{describe_obj(obj)}"
    end
  end

  # `T::Types::TypedEnumerable#type_from_instance`: the type a failed
  # stdlib-generic check names an Enumerable value by.
  def self.type_from_instance(obj)
    if [true, false].include?(obj)
      return BOOLEAN
    elsif !obj.is_a?(::Enumerable)
      return obj.class
    end
    case obj
    when ::Array
      Generic.new("T::Array", ::Array, [type_from_instances(obj)])
    when ::Hash
      Generic.new("T::Hash", ::Hash, [type_from_instances(obj.keys), type_from_instances(obj.values)])
    when ::Range
      typeable_objects = [obj.begin, obj.end].compact
      if typeable_objects.empty?
        Generic.new("T::Range", ::Range, [UNTYPED])
      else
        Generic.new("T::Range", ::Range, [type_from_instances(typeable_objects)])
      end
    when ::Enumerator::Lazy
      Generic.new("T::Enumerator::Lazy", ::Enumerator::Lazy, [type_from_instances(obj)])
    when ::Enumerator::Chain
      Generic.new("T::Enumerator::Chain", ::Enumerator::Chain, [type_from_instances(obj)])
    when ::Enumerator
      Generic.new("T::Enumerator", ::Enumerator, [type_from_instances(obj)])
    when ::Set
      Generic.new("T::Set", ::Set, [type_from_instances(obj)])
    when ::IO
      obj.class
    else
      ::Object.instance_method(:class).bind_call(obj)
    end
  end

  # `T::Types::TypedEnumerable#type_from_instances`.
  def self.type_from_instances(objs)
    return objs.class unless objs.is_a?(::Enumerable)
    obtained_types = []
    begin
      objs.each do |x|
        obtained_types << type_from_instance(x)
      end
    rescue
      return UNTYPED
    end
    if obtained_types.count > 1
      Union.new(obtained_types)
    elsif obtained_types.empty?
      NORETURN
    else
      obtained_types.first
    end
  end

  # `T::Utils.string_truncate_middle`.
  def self.string_truncate_middle(str, start_len, end_len, ellipsis = "...")
    return str unless str
    str = str.to_s
    return str if str.length <= start_len + end_len
    start_part = str[0...start_len - ellipsis.length]
    end_part = end_len == 0 ? "" : str[-end_len..-1]
    "#{start_part}#{ellipsis}#{end_part}"
  end

  # `T::Types::Simple`.
  class Simple
    include Described

    def initialize(raw)
      @raw = raw
    end

    def valid?(obj)
      obj.is_a?(@raw)
    end

    def name
      NAME_METHOD.bind_call(@raw) || @raw.name
    end

    # `T::Types::Simple#error_message`: two constants of one name.
    def error_message(obj)
      error_message = super(obj)
      actual_name = obj.class.name

      return error_message unless name == actual_name

      <<~MSG.strip
        #{error_message}

        The expected type and received object type have the same name but refer to different constants.
        Expected type is #{name} with object id #{@raw.__id__}, but received type is #{actual_name} with object id #{obj.class.__id__}.

        There might be a constant reloading problem in your application.
      MSG
    end
  end

  # `T::Types::Untyped` / `T::Types::Anything`.
  class Any
    include Described
    attr_reader :name

    def initialize(name)
      @name = name
    end

    def valid?(_obj)
      true
    end
  end

  UNTYPED = Any.new("T.untyped")
  ANYTHING = Any.new("T.anything")

  # `T::Types::TypedArray` and the other stdlib generics: `valid?` is the
  # outer class, `name` spells the arguments. `T::Class` / `T::Module`
  # test with `===`, the rest with `is_a?`.
  class Generic
    include Described

    def initialize(label, outer, args)
      @label = label
      @outer = outer
      @args = args
    end

    def valid?(obj)
      if module_like?
        @outer === obj
      else
        obj.is_a?(@outer)
      end
    end

    # `T::Types::TypedEnumerable#describe_obj` (not `T::Class` /
    # `T::Module`, which keep `Base`'s).
    def describe_obj(obj)
      return super if module_like? || !obj.is_a?(::Enumerable)
      SorbetRuntimeCheck.type_from_instance(obj).name
    end

    private

    def module_like?
      @outer == ::Class || @outer == ::Module
    end

    public

    def name
      "#{@label}[#{@args.map { |arg| SorbetRuntimeCheck.coerce(arg).name }.join(', ')}]"
    end
  end

  # `T::Types::ClassOf`.
  class ClassOf
    include Described

    def initialize(type)
      @type = type
    end

    def describe_obj(obj)
      obj.inspect
    end

    def valid?(obj)
      obj.is_a?(@type.singleton_class)
    end

    def name
      "T.class_of(#{@type})"
    end
  end

  # `T::Private::Types::StringHolder`: the `T::Boolean` stand-in a union
  # name puts in place of TrueClass and FalseClass.
  class Named
    attr_reader :name

    def initialize(name)
      @name = name
    end
  end

  # `T::Types::Union`: nested unions flatten, members dedupe by name (a
  # type's `==` and `hash` are its name's), and the name is sorbet's
  # `type_shortcuts`.
  class Union
    include Described
    attr_reader :types

    def initialize(types)
      @types = types.flat_map do |type|
        type = SorbetRuntimeCheck.coerce(type)
        type.is_a?(Union) ? type.types : [type]
      end.uniq(&:name)
    end

    def valid?(obj)
      @types.any? { |type| type.valid?(obj) }
    end

    def name
      shortcuts(@types)
    end

    private

    def shortcuts(types)
      return types.fetch(0).name if types.size == 1
      if types.any? { |type| type.name == "NilClass" }
        "T.nilable(#{shortcuts(types.reject { |type| type.name == "NilClass" })})"
      elsif types.any? { |type| type.name == "TrueClass" } && types.any? { |type| type.name == "FalseClass" }
        rest = types.reject { |type| type.name == "TrueClass" || type.name == "FalseClass" }
        shortcuts([Named.new("T::Boolean")] + rest)
      else
        "T.any(#{types.map(&:name).compact.sort.join(', ')})"
      end
    end
  end

  BOOLEAN = Union.new([::TrueClass, ::FalseClass])
  NORETURN = Named.new("T.noreturn")

  def self.nilable(type)
    Union.new([type, ::NilClass])
  end

  def self.any(*types)
    Union.new(types)
  end

  def self.generic(label, outer, *args)
    Generic.new(label, outer, args)
  end

  def self.class_of(type)
    ClassOf.new(type)
  end

  def self.untyped
    UNTYPED
  end

  def self.anything
    ANYTHING
  end

  def self.boolean
    BOOLEAN
  end
end

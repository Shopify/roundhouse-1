class Finder
  #: (Room) -> untyped
  def self.loaded(room)
    room.messages.to_a
  end

  #: (Room) -> untyped
  def self.loaded_ordered(room)
    room.ordered_messages.to_a
  end

  #: (Room) -> untyped
  def self.first_one(room)
    room.messages.first
  end

  #: (Room) -> untyped
  def self.linked(room)
    room.messages.order(:id).to_a
  end

  #: (Room) -> untyped
  def self.last_one(room)
    room.messages.last
  end

  #: (Room) -> untyped
  def self.take_one(room)
    room.messages.take
  end

  #: (Room) -> untyped
  def self.first_two(room)
    room.messages.first(2)
  end

  #: (Room) -> untyped
  def self.last_two(room)
    room.messages.last(2)
  end

  #: (Room) -> untyped
  def self.found_by(room)
    room.messages.find_by(body: "m1")
  end

  #: (Room) -> untyped
  def self.linked_first(room)
    room.messages.order(:id).first
  end

  #: (Room) -> untyped
  def self.reflection_first(room)
    room.strict_messages.first
  end

  #: (Room) -> untyped
  def self.reflection_last(room)
    room.strict_messages.last
  end

  #: (Room) -> untyped
  def self.reflection_take(room)
    room.strict_messages.take
  end

  #: (Room) -> untyped
  def self.reflection_to_a(room)
    room.strict_messages.to_a
  end

  #: (Room) -> untyped
  def self.reflection_found_by(room)
    room.strict_messages.find_by(body: "m1")
  end
end

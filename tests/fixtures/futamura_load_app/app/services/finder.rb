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
end

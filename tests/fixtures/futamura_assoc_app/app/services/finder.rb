class Finder
  #: (Room) -> untyped
  def self.not_blank(room)
    room.messages.where.not(body: nil).order(:id).to_a
  end

  #: (Room) -> untyped
  def self.pinned(room)
    room.pinned.to_a
  end

  #: (Room) -> untyped
  def self.pinned_ordered(room)
    room.pinned.order(:id).to_a
  end

  #: (Room) -> untyped
  def self.first_pinned(room)
    room.pinned.first
  end

  #: (Room) -> untyped
  def self.newest(room)
    room.newest.first(2)
  end

  #: (Room) -> untyped
  def self.notes(room)
    room.notes.to_a
  end

  #: (Room) -> untyped
  def self.scoped(room)
    room.scoped_messages.to_a
  end

  #: (Room) -> untyped
  def self.same_column(room)
    room.same_column.to_a
  end

  def self.fragment(room_id)
    Message.where(room_id: room_id).where("body LIKE ?", "a%").order(:id).to_a
  end

  def self.dynamic_fragment(room_id, prefix)
    Message.where("room_id = ? AND body LIKE ?", room_id, prefix).to_a
  end

  def self.kinds
    Message.where(kind: ["visible", "hidden"]).order(:id).to_a
  end

  def self.kinds_not
    Message.where.not(kind: ["visible"]).order(:id).to_a
  end

  def self.kinds_in(kinds)
    Message.where(kind: kinds).to_a
  end

  def self.not_fragment
    Message.where.not("body IS NULL").order(:id).to_a
  end

  #: (Room) -> untyped
  def self.remarks(room)
    room.remarks.to_a
  end
end

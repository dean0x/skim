# FIXTURE: Ruby class with inheritance and mixins
# TESTS: Class structure extraction

module Serializable
  def to_json
    JSON.generate(to_h)
  end

  def to_h
    raise NotImplementedError
  end
end

class Animal
  attr_accessor :name, :age

  def initialize(name, age)
    @name = name
    @age = age
  end

  def speak
    raise NotImplementedError
  end
end

class Dog < Animal
  include Serializable

  def speak
    "Woof!"
  end

  def fetch(item)
    "Fetching #{item}..."
  end

  def to_h
    { name: name, age: age, type: 'dog' }
  end
end

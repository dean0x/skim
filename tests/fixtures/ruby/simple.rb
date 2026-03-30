# FIXTURE: Simple Ruby class
# TESTS: Basic method signature extraction

require 'json'
require 'net/http'

class UserService
  attr_reader :logger

  def initialize(logger)
    @logger = logger
  end

  def find_user(id)
    user = User.find(id)
    raise NotFoundError, "User #{id} not found" unless user
    user
  end

  def delete_user(id)
    User.destroy(id)
    logger.info("Deleted user #{id}")
  end

  private

  def validate_id(id)
    raise ArgumentError, "Invalid ID" unless id.positive?
  end
end

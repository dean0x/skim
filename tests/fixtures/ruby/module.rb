# FIXTURE: Ruby modules and mixins
# TESTS: Module structure extraction

module Validators
  module Email
    def valid_email?(email)
      email.match?(/\A[\w+\-.]+@[a-z\d\-]+(\.[a-z\d\-]+)*\.[a-z]+\z/i)
    end
  end

  module Phone
    COUNTRY_CODES = %w[1 44 81 91].freeze

    def valid_phone?(phone)
      phone.match?(/\A\+?\d{10,15}\z/)
    end

    def format_phone(phone)
      "+#{phone.gsub(/\D/, '')}"
    end
  end
end

module Configurable
  def self.included(base)
    base.extend(ClassMethods)
  end

  module ClassMethods
    def config_option(name, default: nil)
      define_method(name) do
        instance_variable_get("@#{name}") || default
      end

      define_method("#{name}=") do |value|
        instance_variable_set("@#{name}", value)
      end
    end
  end
end

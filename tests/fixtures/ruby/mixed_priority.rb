# FIXTURE: Ruby file with mixed priority items
# TESTS: Truncation priority testing

require 'json'
require 'net/http'
require 'uri'

Config = Struct.new(:host, :port, :timeout)

module LogLevel
  DEBUG = 0
  INFO = 1
  WARN = 2
  ERROR = 3
end

class Server
  attr_reader :config

  def initialize(config)
    @config = config
  end

  def start
    puts "Starting on #{config.host}:#{config.port}"
  end

  def stop
    puts "Stopping server"
  end
end

def process_request(url, config)
  return -1 if url.nil? || config.nil?
  puts "Processing: #{url} on port #{config.port}"
  0
end

def validate_url(url)
  URI.parse(url)
  true
rescue URI::InvalidURIError
  false
end

MAX_RETRIES = 3
DEFAULT_PORT = 8080

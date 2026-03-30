# FIXTURE: Ruby blocks, procs, and lambdas
# TESTS: Block handling in structure mode

class DataProcessor
  def initialize(data)
    @data = data
  end

  def transform(&block)
    @data.map(&block)
  end

  def filter
    @data.select { |item| yield(item) }
  end

  def process_all
    @data.each do |item|
      result = process_item(item)
      puts "Processed: #{result}"
    end
  end

  def with_retry(attempts: 3)
    attempts.times do |i|
      begin
        return yield
      rescue StandardError => e
        puts "Attempt #{i + 1} failed: #{e.message}"
        raise if i == attempts - 1
      end
    end
  end

  private

  def process_item(item)
    item.to_s.upcase
  end
end

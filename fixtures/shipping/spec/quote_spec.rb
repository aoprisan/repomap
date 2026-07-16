require_relative "../lib/quote"

# Minimal spec-style check without a framework dependency.
def test_quote_total
  Quote.new.total("eu", 2) == 1000
end

"""Catalog pricing rules for the Python fixture."""

BASE_PRICES = {"apple": 100, "pear": 120}


def unit_price(sku):
    """Price of one unit, in cents."""
    return BASE_PRICES.get(sku, 0)


def discount(qty):
    """Bulk discount multiplier."""
    return 0.9 if qty >= 10 else 1.0

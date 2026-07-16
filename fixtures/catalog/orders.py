"""Order totals built on pricing."""

import pricing


def order_total(sku, qty):
    """Total in cents for qty units of sku."""
    return int(pricing.unit_price(sku) * qty * pricing.discount(qty))

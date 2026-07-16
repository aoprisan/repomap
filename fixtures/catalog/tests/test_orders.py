"""Tests for order totals."""

from orders import order_total


def test_order_total_applies_bulk_discount():
    assert order_total("apple", 10) == 900

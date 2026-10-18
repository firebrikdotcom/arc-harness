"""Compact provider usage and dated list-price estimates; never infer missing tokens."""
from __future__ import annotations

import math
from urllib.parse import urlsplit
from typing import Any

# https://docs.typesafe.ai/models, checked 2026-10-17. Output is free.
PRICES = {"jev-1.13.0": 0.042, "jev-1.14.0": 0.042}
PRICE_VERSION = "typesafe-models-2026-10-17"


def normalize_usage(response: dict[str, Any], endpoint: str) -> dict[str, Any] | None:
    usage = response.get("usage")
    if not isinstance(usage, dict):
        return None
    result = {k: v for k, v in usage.items()
              if k in {"input_tokens", "output_tokens", "total_tokens"}
              and type(v) is int and 0 <= v <= 2**63 - 1}
    if "total_tokens" not in result and all(k in result for k in ("input_tokens", "output_tokens")):
        total = result["input_tokens"] + result["output_tokens"]
        if total <= 2**63 - 1:
            result["total_tokens"] = total
    cost = usage.get("cost_usd")
    if type(cost) in (int, float) and 0 <= cost <= 1e100 and math.isfinite(cost):
        result.update(cost_usd=cost, cost_source="provider")
    elif (urlsplit(endpoint).scheme == "https" and urlsplit(endpoint).netloc == "api.typesafe.ai"
          and isinstance(response.get("model"), str)
          and response["model"] in PRICES and "input_tokens" in result):
        rate = PRICES[response["model"]]
        result.update(cost_usd=result["input_tokens"] / 1_000_000 * rate,
                      cost_source="list_price_estimate", price_version=PRICE_VERSION,
                      input_usd_per_million=rate, output_usd_per_million=0)
    return result or None

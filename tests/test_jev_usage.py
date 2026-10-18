import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "scripts"))
from jev_usage import normalize_usage
from audit_emit import checkpoint_payload, route_payload


class UsageTests(unittest.TestCase):
    def usage(self, usage, model="jev-1.13.0", endpoint="https://api.typesafe.ai/v1/systemone"):
        return normalize_usage({"model": model, "usage": usage}, endpoint)

    def test_known_models_use_dated_input_price_and_free_output(self):
        for model in ("jev-1.13.0", "jev-1.14.0"):
            usage = self.usage({"input_tokens": 1_000_000, "output_tokens": 300}, model)
            self.assertEqual(usage["total_tokens"], 1_000_300)
            self.assertEqual(usage["cost_usd"], 0.042)
            self.assertEqual(usage["cost_source"], "list_price_estimate")
            self.assertEqual(usage["price_version"], "typesafe-models-2026-10-17")

    def test_provider_cost_takes_precedence_including_zero(self):
        for cost in (0, 0.123):
            usage = self.usage({"input_tokens": 100, "cost_usd": cost})
            self.assertEqual(usage["cost_usd"], cost)
            self.assertEqual(usage["cost_source"], "provider")

    def test_missing_invalid_and_partial_values_stay_unknown(self):
        for usage in (None, {}, {"input_tokens": True, "output_tokens": -1, "cost_usd": "0"},
                      {"input_tokens": 2**64, "cost_usd": float("nan")}):
            self.assertIsNone(self.usage(usage))
        self.assertEqual(self.usage({"output_tokens": 4}), {"output_tokens": 4})
        self.assertNotIn("total_tokens", self.usage({"input_tokens": 0}))
        self.assertEqual(self.usage({"input_tokens": 0, "output_tokens": 0})["cost_usd"], 0)

    def test_unknown_models_aliases_and_other_endpoints_are_not_priced(self):
        for model, endpoint in (("fixture", "https://api.typesafe.ai/v1/systemone"),
                                ("jev-latest", "https://api.typesafe.ai/v1/systemone"),
                                ("jev-1.13.0", "https://proxy.example/v1/systemone")):
            self.assertNotIn("cost_usd", self.usage({"input_tokens": 12}, model, endpoint))

    def test_both_event_types_preserve_usage_and_provenance_without_private_context(self):
        usage = self.usage({"input_tokens": 12, "output_tokens": 3})
        for producer in (checkpoint_payload, route_payload):
            event = producer({"usage": usage, "context": "PRIVATE"})
            self.assertEqual(event["jev_total_tokens"], 15)
            self.assertEqual(event["jev_cost_usd"], usage["cost_usd"])
            self.assertEqual(event["jev_cost_source"], "list_price_estimate")
            self.assertEqual(event["jev_price_version"], usage["price_version"])
            self.assertNotIn("context", event)


if __name__ == "__main__":
    unittest.main()

import argparse
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("results_admin", Path(__file__).parents[1] / "results-admin.py")
admin = importlib.util.module_from_spec(spec)
spec.loader.exec_module(admin)
ELECTION = "295d864d-80c8-4a87-ab16-3f4f3fa5ddb7"


def args(**changes):
    values = dict(action="pause", election_id=ELECTION, environment="prod", starts_at=None, ends_at=None, expected_source_url=None, acceptance_record=None)
    values.update(changes)
    return argparse.Namespace(**values)


class AdminTests(unittest.TestCase):
    def test_pause_and_recovery_use_existing_scoped_rpcs(self):
        self.assertEqual(admin.plan(args()), ("configure_sos_results_feed", {"p_election_id": ELECTION, "p_enabled": False}))
        for action, rpc_action in [("retry", "retry"), ("pointer-repair", "retry"), ("abandon", "abandon")]:
            self.assertEqual(admin.plan(args(action=action)), ("recover_sos_results_job", {"p_election_id": ELECTION, "p_action": rpc_action}))

    def test_production_resume_requires_complete_matching_acceptance(self):
        request = args(action="resume", starts_at="2026-11-03T12:00:00Z", ends_at=admin.STOP_AT, expected_source_url=admin.SOURCE)
        with self.assertRaises(ValueError):
            admin.plan(request)
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "acceptance.json"
            request.acceptance_record = path
            record = dict(accepted=True, environment="prod", election_id=ELECTION, source_url=admin.SOURCE, election_date="2026-11-03", initial_poll_interval_seconds=3600, poll_interval_seconds=300, poll_transition_at=admin.TRANSITION_AT, starts_at=request.starts_at, ends_at=request.ends_at, operator="reviewed operator", checks={name: True for name in admin.ACCEPTANCE_CHECKS})
            path.write_text(json.dumps(record))
            self.assertTrue(admin.plan(request)[1]["p_enabled"])
            for change in [{"accepted": False}, {"election_id": "wrong"}, {"source_url": "https://unreviewed.example"}, {"operator": ""}, {"checks": {"dummy": True}}]:
                path.write_text(json.dumps(record | change))
                with self.assertRaises(ValueError):
                    admin.plan(request)

    def test_dev_resume_still_requires_exact_url_and_window(self):
        with self.assertRaises(ValueError):
            admin.plan(args(action="resume", environment="dev"))
        request = args(action="resume", environment="test", expected_source_url=admin.SOURCE, starts_at="2026-11-03T12:00:00Z", ends_at="2026-11-03T13:00:00Z")
        self.assertTrue(admin.plan(request)[1]["p_enabled"])
        request.ends_at = request.starts_at
        with self.assertRaises(ValueError):
            admin.plan(request)
        request.ends_at = "2026-11-03T13:00:00"
        with self.assertRaises(ValueError):
            admin.plan(request)

    def test_origin_rejects_credentials_redirect_targets_and_prod_http(self):
        for origin in ["http://127.0.0.1:54321", "https://token@example.com", "https://example.com/?secret=token", "https://example.com/not-an-origin", "https://example.com/#token"]:
            with self.assertRaises(ValueError):
                admin.target("prod", origin)
        self.assertEqual(admin.target("test", "http://127.0.0.1:54321"), "http://127.0.0.1:54321")
        self.assertEqual(admin.target("prod", "https://project.supabase.co/"), "https://project.supabase.co")

    def test_window_preparation_never_enables_collection_or_requires_activation_acceptance(self):
        request = args(action="window", environment="prod", expected_source_url=admin.SOURCE, starts_at="2026-11-03T19:00:00Z", ends_at=admin.STOP_AT)
        rpc, body = admin.plan(request)
        self.assertEqual(rpc, "configure_sos_results_feed_window")
        self.assertEqual(body["p_ends_at"], admin.STOP_AT)
        self.assertNotIn("p_enabled", body)

    def test_paused_window_readback_requires_matching_identity_and_instant(self):
        request = args(action="window", environment="prod", expected_source_url=admin.SOURCE, starts_at="2026-11-03T19:00:00Z", ends_at=admin.STOP_AT)
        result = {"election_id": ELECTION, "enabled": False, "source_url": admin.SOURCE, "starts_at": "2026-11-03T19:00:00+00:00", "ends_at": admin.STOP_AT}
        admin.verify_window(result, request, ELECTION)
        for change in [{"enabled": True}, {"source_url": "wrong"}, {"starts_at": None}, {"election_id": "wrong"}]:
            with self.assertRaises(ValueError):
                admin.verify_window(result | change, request, ELECTION)

    def test_cadence_stays_in_single_coordinator_and_extension_is_rejected(self):
        request = args(action="resume", environment="prod", expected_source_url=admin.SOURCE, starts_at="2026-11-03T19:00:00Z", ends_at=admin.STOP_AT)
        expected = {"p_election_id": ELECTION, "p_interval_seconds": 3600, "p_transition_at": admin.TRANSITION_AT, "p_after_interval_seconds": 300}
        self.assertEqual(admin.cadence(request), expected)
        request.initial_interval_seconds = 300
        with self.assertRaises(ValueError):
            admin.cadence(request)
        request.initial_interval_seconds = 3600
        request.ends_at = "2026-11-12T00:00:00Z"
        with self.assertRaises(ValueError):
            admin.plan(request)

    def test_environment_mismatch_fails_before_network(self):
        with patch.dict("os.environ", {"CIVPULSE_ENV": "prod"}, clear=True):
            with self.assertRaises(ValueError):
                admin.execute("test", "recover_sos_results_job", {})


if __name__ == "__main__":
    unittest.main()

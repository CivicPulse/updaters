import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("resource_job", Path(__file__).resolve().parents[1] / "results-resource-job.py")
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class ResourceJobTests(unittest.TestCase):
    def test_offline_single_job_is_bounded_without_runtime_credentials(self):
        docs = module.manifest("sha256:" + "a" * 64, 134217728, 16777216)["items"]
        policy, job = docs
        self.assertEqual(policy["spec"]["egress"], [])
        self.assertEqual(job["metadata"]["namespace"], "civpulse-test")
        pod = job["spec"]["template"]["spec"]
        self.assertFalse(pod["automountServiceAccountToken"])
        self.assertNotIn("secretKeyRef", str(pod))
        self.assertEqual(pod["containers"][0]["args"][2], "--pipeline")
        self.assertEqual(pod["containers"][0]["resources"]["limits"]["memory"], "1Gi")

    def test_caps_cannot_raise_or_invert_contract(self):
        for raw, scope in [(0, 1), (536870913, 1), (536870912, 67108865), (1, 2)]:
            with self.assertRaises(ValueError):
                module.manifest("sha256:" + "a" * 64, raw, scope)

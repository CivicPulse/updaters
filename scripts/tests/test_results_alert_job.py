import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("alert_job", Path(__file__).parents[1] / "results-alert-test.py")
job = importlib.util.module_from_spec(spec)
spec.loader.exec_module(job)


class AlertJobTests(unittest.TestCase):
    def test_verification_uses_only_email_secret_and_never_claims_source_work(self):
        rendered = job.manifest("test", "sha256:" + "a" * 64)
        spec = rendered["spec"]["template"]["spec"]
        self.assertFalse(spec["automountServiceAccountToken"])
        self.assertEqual(rendered["spec"]["backoffLimit"], 0)
        container = spec["containers"][0]
        self.assertEqual(container["args"], ["alert-test"])
        env = {entry["name"]: entry for entry in container["env"]}
        self.assertEqual(env["SOS_COLLECTOR_ENABLED"]["value"], "false")
        self.assertNotIn("SUPABASE_SERVICE_ROLE_KEY", env)
        self.assertNotIn("R2_SECRET_ACCESS_KEY", env)
        self.assertEqual(env["CLOUDFLARE_EMAIL_API_TOKEN"]["valueFrom"]["secretKeyRef"]["name"], "results-alert-email")

    def test_mutable_images_and_unrecognized_namespaces_are_rejected(self):
        for environment, digest in [("test", "latest"), ("production", "sha256:" + "a" * 64), ("prod", "sha256:" + "g" * 64)]:
            with self.assertRaises(ValueError):
                job.manifest(environment, digest)


if __name__ == "__main__":
    unittest.main()

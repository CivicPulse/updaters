"""Actual CLI boundary regressions: measurement must never truncate source evidence."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[3]
SCRIPT = ROOT/'scripts/measure/results.sh'
FIXTURE = ROOT/'crates/civpulse-results-contract/tests/fixtures/base.json'


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


class MeasurementReports(unittest.TestCase):
    def invoke(self, source, prefix):
        return subprocess.run(['bash', str(SCRIPT), str(source), '2026-11-03', str(prefix), '2', '2'],
                              cwd=ROOT, env=dict(os.environ, LC_ALL='C', CIVPULSE_MEASURE_SCRATCH_ROOT=str(ROOT/'target')),
                              capture_output=True, timeout=900)

    def test_report_names_cannot_alias_the_input(self):
        for suffix in ('.json', '.time.txt'):
            with self.subTest(suffix=suffix), tempfile.TemporaryDirectory() as directory:
                prefix = Path(directory)/'feed'
                source = Path(str(prefix)+suffix)
                source.write_bytes(FIXTURE.read_bytes())
                before = digest(source)
                result = self.invoke(source, prefix)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(digest(source), before)
                other = Path(str(prefix)+('.time.txt' if suffix == '.json' else '.json'))
                self.assertFalse(other.exists(), 'rejected reservation must leave no partial new report')

    def test_symlink_and_hardlink_report_aliases_preserve_source(self):
        for link in ('symlink', 'hardlink'):
            for suffix in ('.json', '.time.txt'):
                with self.subTest(link=link, suffix=suffix), tempfile.TemporaryDirectory() as directory:
                    source = Path(directory)/'original.json'
                    source.write_bytes(FIXTURE.read_bytes())
                    prefix = Path(directory)/'report'
                    alias = Path(str(prefix)+suffix)
                    if link == 'symlink':
                        alias.symlink_to(source)
                    else:
                        os.link(source, alias)
                    before = digest(source)
                    result = self.invoke(source, prefix)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertEqual(digest(source), before)
                    self.assertEqual(digest(alias), before)
                    self.assertEqual(alias.is_symlink(), link == 'symlink')

    def test_existing_reports_are_never_overwritten(self):
        for existing in (('.json',), ('.time.txt',), ('.json', '.time.txt')):
            with self.subTest(existing=existing), tempfile.TemporaryDirectory() as directory:
                source = Path(directory)/'source.json'
                source.write_bytes(FIXTURE.read_bytes())
                prefix = Path(directory)/'report'
                prior = {}
                for suffix in existing:
                    path = Path(str(prefix)+suffix)
                    content = b'{"measurement":"retained-existing-evidence"}' if suffix == '.json' else b'Retained GNU time evidence\n'
                    path.write_bytes(content)
                    prior[path] = content
                before = digest(source)
                result = self.invoke(source, prefix)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(digest(source), before)
                for path, content in prior.items():
                    self.assertEqual(path.read_bytes(), content)
                for suffix in set(('.json', '.time.txt'))-set(existing):
                    self.assertFalse(Path(str(prefix)+suffix).exists())

    def test_dangling_symlink_is_not_followed(self):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory)/'source.json'
            source.write_bytes(FIXTURE.read_bytes())
            prefix = Path(directory)/'report'
            missing = Path(directory)/'must-not-be-created'
            Path(str(prefix)+'.json').symlink_to(missing)
            before = digest(source)
            self.assertNotEqual(self.invoke(source, prefix).returncode, 0)
            self.assertEqual(digest(source), before)
            self.assertFalse(missing.exists())

    def test_failed_validation_removes_only_new_reports(self):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory)/'source.json'
            source.write_bytes(FIXTURE.read_bytes())
            before = digest(source)
            prefix = Path(directory)/'failed'
            result = subprocess.run(['bash', str(SCRIPT), str(source), '2026-11-04', str(prefix), '2', '2'],
                                    cwd=ROOT, env=dict(os.environ, CIVPULSE_MEASURE_SCRATCH_ROOT=str(ROOT/'target')),
                                    capture_output=True, timeout=900)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(digest(source), before)
            self.assertFalse(Path(str(prefix)+'.json').exists())
            self.assertFalse(Path(str(prefix)+'.time.txt').exists())

    def test_fresh_prefix_measures_a_valid_fixture(self):
        with tempfile.TemporaryDirectory(prefix='civpulse measure ') as directory:
            source = Path(directory)/'source file.json'
            source.write_bytes(FIXTURE.read_bytes())
            before = digest(source)
            prefix = Path(directory)/'fresh report'
            result = self.invoke(source, prefix)
            self.assertEqual(result.returncode, 0, result.stderr.decode())
            self.assertEqual(digest(source), before)
            report_path = Path(str(prefix)+'.json')
            report = json.loads(report_path.read_bytes())
            self.assertEqual(report, json.loads(result.stdout))
            self.assertEqual(report['raw_sha256'], before)
            self.assertEqual(report['source_bytes'], source.stat().st_size)
            self.assertEqual(report['jurisdictions'], 2)
            self.assertEqual(report['items'], 2)
            self.assertFalse(report['production_acceptance'])
            self.assertIn('Maximum resident set size', Path(str(prefix)+'.time.txt').read_text())
            retained = {path:path.read_bytes() for path in (report_path, Path(str(prefix)+'.time.txt'))}
            self.assertNotEqual(self.invoke(source, prefix).returncode, 0)
            self.assertEqual(digest(source), before)
            for path, content in retained.items():
                self.assertEqual(path.read_bytes(), content)


if __name__ == '__main__':
    unittest.main()

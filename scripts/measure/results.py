#!/usr/bin/env python3
"""Reserve create-only report descriptors before building or running measurement.

GNU time reopens its -o path with truncation even under shell noclobber. Give it
an already exclusively reserved descriptor through procfs, never a source/report
pathname that could resolve to someone else's evidence.
"""
import os
from pathlib import Path
import shutil
import stat
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]


def still_owned(path, descriptor):
    try:
        current = os.stat(path, follow_symlinks=False)
    except FileNotFoundError:
        return False
    owned = os.fstat(descriptor)
    return (current.st_dev, current.st_ino) == (owned.st_dev, owned.st_ino)


def main():
    if len(sys.argv) < 4:
        print('Usage: scripts/measure/results.sh INPUT DATE OUTPUT_PREFIX [--pipeline] [JURISDICTIONS ITEMS]', file=sys.stderr)
        return 2
    source = Path(sys.argv[1]).absolute()
    date = sys.argv[2]
    prefix = Path(sys.argv[3]).absolute()
    if not stat.S_ISREG(source.stat().st_mode):
        raise OSError('Measurement input must be a regular file')
    reports = []
    completed = False
    try:
        # O_EXCL rejects every existing entry, including symlinks (even dangling
        # ones), hard links, and either report name aliasing the input. All report
        # paths are reserved before Cargo or GNU time can perform any writes.
        for suffix in ('.json', '.time.txt'):
            path = Path(str(prefix) + suffix)
            descriptor = os.open(path, os.O_RDWR | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
            reports.append((path, descriptor))
        json_fd, time_fd = (entry[1] for entry in reports)
        revision = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip()
        environment = dict(os.environ, CIVPULSE_MEASURE_CODE_REVISION=revision)
        subprocess.run(['cargo', 'build', '--locked', '--release', '-p', 'civpulse-results-collector', '--example', 'measure'],
                       cwd=ROOT, env=environment, check=True)
        subprocess.run(['/usr/bin/time', '-v', '-o', f'/proc/self/fd/{time_fd}',
                        str(ROOT/'target/release/examples/measure'), str(source), date, *sys.argv[4:]],
                       cwd=ROOT, stdout=json_fd, pass_fds=(time_fd,), check=True)
        for path, descriptor in reports:
            os.fsync(descriptor)
            if not still_owned(path, descriptor):
                raise OSError('A reserved measurement report path changed during the run')
        completed = True
        # Read the owned descriptor, not a pathname that another process could replace.
        os.lseek(json_fd, 0, os.SEEK_SET)
        with os.fdopen(os.dup(json_fd), 'rb') as report:
            shutil.copyfileobj(report, sys.stdout.buffer)
        return 0
    finally:
        for path, descriptor in reports:
            try:
                # Never remove a replacement file/symlink that this run did not create.
                if not completed and still_owned(path, descriptor):
                    path.unlink()
            finally:
                os.close(descriptor)


if __name__ == '__main__':
    try:
        raise SystemExit(main())
    except FileExistsError:
        raise SystemExit('Measurement report paths must not already exist; choose a fresh output prefix') from None
    except subprocess.CalledProcessError as error:
        raise SystemExit(error.returncode) from None
    except OSError:
        raise SystemExit('Measurement input/report I/O failed; existing files were not overwritten') from None

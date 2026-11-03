"""Require a Conventional Commit title without interpreting it as shell code."""

import os
import re
import sys

pattern = r"(?:feat|fix|docs|style|refactor|perf|test|build|ci|chore|revert)(?:\([a-zA-Z0-9_./-]+\))?!?: \S[^\r\n]*"
title = os.environ.get("PR_TITLE", "")
if re.fullmatch(pattern, title) is None:
    print("PR title must use Conventional Commits, e.g. feat(collector): add worker")
    sys.exit(1)
print("Conventional PR title accepted")
